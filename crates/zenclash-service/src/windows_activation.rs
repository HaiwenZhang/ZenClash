//! SCM trigger-start and a retained, read-only pipe connection for each GUI process.
//! Authenticated mutations continue to use the existing control pipe.

use anyhow::{Context as _, Result, bail};
use platform_lib::service::Service;
#[cfg(not(feature = "test"))]
use platform_lib::service::ServiceAccess;
#[cfg(not(feature = "test"))]
use platform_lib::service_manager::{ServiceManager, ServiceManagerAccess};
use std::io;
#[cfg(any(not(feature = "test"), test))]
use std::time::Duration;
use windows_sys::Win32::System::Services::{
    ChangeServiceConfig2W, NAMED_PIPE_EVENT_GUID, QueryServiceConfig2W, SERVICE_CONFIG_TRIGGER_INFO, SERVICE_TRIGGER,
    SERVICE_TRIGGER_ACTION_SERVICE_START, SERVICE_TRIGGER_DATA_TYPE_STRING, SERVICE_TRIGGER_INFO,
    SERVICE_TRIGGER_SPECIFIC_DATA_ITEM, SERVICE_TRIGGER_TYPE_NETWORK_ENDPOINT,
};

#[cfg(all(feature = "standalone", any(not(feature = "test"), test)))]
const IDLE_TIMEOUT: Duration = Duration::from_secs(3);
static STOPPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn pipe_path() -> String {
    format!("{}-activation", crate::IPC_PATH)
}

fn trigger_pipe_name() -> Vec<u16> {
    pipe_path()
        .trim_start_matches(r"\\.\pipe\")
        .encode_utf16()
        .chain(Some(0))
        .collect()
}

/// Configures a named-pipe start trigger without granting users SCM control rights.
/// The caller must hold the installed service's CHANGE_CONFIG access.
///
/// # Errors
/// Returns an error when Windows refuses the trigger configuration.
pub fn configure_windows_service_activation(service: &Service) -> Result<()> {
    let mut name = trigger_pipe_name();
    let mut subtype = NAMED_PIPE_EVENT_GUID;
    let mut data = SERVICE_TRIGGER_SPECIFIC_DATA_ITEM {
        dwDataType: SERVICE_TRIGGER_DATA_TYPE_STRING,
        cbData: std::mem::size_of_val(name.as_slice()) as u32,
        pData: name.as_mut_ptr().cast(),
    };
    let mut trigger = SERVICE_TRIGGER {
        dwTriggerType: SERVICE_TRIGGER_TYPE_NETWORK_ENDPOINT,
        dwAction: SERVICE_TRIGGER_ACTION_SERVICE_START,
        pTriggerSubtype: &mut subtype,
        cDataItems: 1,
        pDataItems: &mut data,
    };
    let info = SERVICE_TRIGGER_INFO {
        cTriggers: 1,
        pTriggers: &mut trigger,
        pReserved: std::ptr::null_mut(),
    };
    // SAFETY: all nested buffers outlive this synchronous call, and the borrowed
    // service handle remains owned by the caller.
    if unsafe {
        ChangeServiceConfig2W(
            service.raw_handle(),
            SERVICE_CONFIG_TRIGGER_INFO,
            (&info as *const SERVICE_TRIGGER_INFO).cast(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error()).context("failed to configure service pipe activation");
    }
    Ok(())
}

/// Checks that the installed service can be restarted by opening its activation pipe.
/// Old installations remain resident until an installer configures this trigger.
///
/// # Errors
/// Returns native configuration-query errors.
pub fn windows_service_can_activate(service: &Service) -> Result<bool> {
    let mut required = 0;
    // SAFETY: this first call only obtains the required buffer size.
    let result = unsafe {
        QueryServiceConfig2W(
            service.raw_handle(),
            SERVICE_CONFIG_TRIGGER_INFO,
            std::ptr::null_mut(),
            0,
            &mut required,
        )
    };
    if result == 0 && io::Error::last_os_error().raw_os_error() != Some(122) {
        return Err(io::Error::last_os_error()).context("failed to size service trigger configuration");
    }
    if required < std::mem::size_of::<SERVICE_TRIGGER_INFO>() as u32 || required > 65536 {
        bail!("invalid service trigger configuration size");
    }
    // Use an aligned buffer: the result contains pointers to native structs.
    let mut buffer = vec![0_usize; (required as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        QueryServiceConfig2W(
            service.raw_handle(),
            SERVICE_CONFIG_TRIGGER_INFO,
            buffer.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    } == 0
    {
        return Err(io::Error::last_os_error()).context("failed to query service pipe activation");
    }
    // SAFETY: Windows initializes this aligned buffer and its nested pointers;
    // none are retained after the backing allocation is dropped.
    let info = unsafe { &*buffer.as_ptr().cast::<SERVICE_TRIGGER_INFO>() };
    if info.cTriggers == 0 || info.pTriggers.is_null() {
        return Ok(false);
    }
    let expected = trigger_pipe_name();
    let triggers = unsafe { std::slice::from_raw_parts(info.pTriggers, info.cTriggers as usize) };
    Ok(triggers
        .iter()
        .any(|trigger| unsafe { matches_pipe_trigger(trigger, &expected) }))
}

// The native trigger and its data must point into a live SCM query buffer.
unsafe fn matches_pipe_trigger(trigger: &SERVICE_TRIGGER, expected: &[u16]) -> bool {
    if trigger.dwTriggerType != SERVICE_TRIGGER_TYPE_NETWORK_ENDPOINT
        || trigger.dwAction != SERVICE_TRIGGER_ACTION_SERVICE_START
        || trigger.pTriggerSubtype.is_null()
        || trigger.pDataItems.is_null()
    {
        return false;
    }
    let subtype = unsafe { &*trigger.pTriggerSubtype };
    if subtype.data1 != NAMED_PIPE_EVENT_GUID.data1
        || subtype.data2 != NAMED_PIPE_EVENT_GUID.data2
        || subtype.data3 != NAMED_PIPE_EVENT_GUID.data3
        || subtype.data4 != NAMED_PIPE_EVENT_GUID.data4
    {
        return false;
    }
    let items = unsafe { std::slice::from_raw_parts(trigger.pDataItems, trigger.cDataItems as usize) };
    items.iter().any(|data| {
        data.dwDataType == SERVICE_TRIGGER_DATA_TYPE_STRING
            && !data.pData.is_null()
            && data.cbData as usize == std::mem::size_of_val(expected)
            && unsafe { std::slice::from_raw_parts(data.pData.cast::<u16>(), expected.len()) } == expected
    })
}

#[cfg(not(feature = "test"))]
pub(crate) fn registered_service_can_activate() -> Result<bool> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    match manager.open_service(crate::WINDOWS_SERVICE_NAME, ServiceAccess::QUERY_CONFIG) {
        Ok(service) => windows_service_can_activate(&service),
        Err(platform_lib::Error::Winapi(error)) if error.raw_os_error() == Some(1060) => Ok(false),
        Err(error) => Err(error).context("failed to inspect service activation registration"),
    }
}

/// Marks shutdown before pipe teardown so SCM queues new trigger events for restart.
pub fn windows_service_begin_stopping() {
    STOPPING.store(true, std::sync::atomic::Ordering::Release);
}

/// Whether a trigger arriving now must be queued until this service stops.
pub fn windows_service_is_stopping() -> bool {
    STOPPING.load(std::sync::atomic::Ordering::Acquire)
}

#[cfg(all(feature = "client", not(feature = "test")))]
static CLIENT: once_cell::sync::Lazy<tokio::sync::Mutex<Option<tokio::net::windows::named_pipe::NamedPipeClient>>> =
    once_cell::sync::Lazy::new(|| tokio::sync::Mutex::new(None));

#[cfg(all(feature = "client", not(feature = "test")))]
pub(crate) async fn wake_service() -> Result<()> {
    use std::os::windows::io::AsRawHandle as _;
    use tokio::io::AsyncReadExt as _;
    use tokio::net::windows::named_pipe::ClientOptions;
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;

    let mut slot = CLIENT.lock().await;
    if slot.as_ref().is_some_and(
        |client| matches!(client.try_read(&mut [0]), Err(error) if error.kind() == io::ErrorKind::WouldBlock),
    ) {
        return Ok(());
    }
    *slot = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        match ClientOptions::new().read(true).write(false).open(pipe_path()) {
            Ok(mut client) => {
                let mut pid = 0;
                // SAFETY: this live client owns the pipe handle and pid is writable.
                if unsafe { GetNamedPipeServerProcessId(client.as_raw_handle(), &mut pid) } == 0 {
                    return Err(io::Error::last_os_error()).context("failed to inspect activation pipe peer");
                }
                crate::client::windows_identity::verify_registered_service_process_id(pid)?;
                let mut ack = [0];
                match tokio::time::timeout_at(deadline, client.read_exact(&mut ack)).await {
                    Ok(Ok(_)) if ack == [1] => {
                        *slot = Some(client);
                        return Ok(());
                    }
                    Ok(Err(error))
                        if matches!(error.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe) => {}
                    Ok(Err(error)) => return Err(error).context("activation handshake failed"),
                    _ => bail!("service activation handshake was not acknowledged"),
                }
            }
            Err(error) if matches!(error.raw_os_error(), Some(2 | 231 | 233)) => {}
            Err(error) => return Err(error).context("failed to open service activation pipe"),
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("timed out activating the installed Windows service");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[cfg(all(feature = "standalone", any(not(feature = "test"), test)))]
mod listener {
    use super::*;
    use parking_lot::Mutex;
    use std::{sync::Arc, time::Instant};
    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _},
        net::windows::named_pipe::ServerOptions,
        task::{JoinHandle, JoinSet},
    };

    #[derive(Default)]
    struct ClientState {
        clients: usize,
        quiet_since: Option<Instant>,
        stopping: bool,
    }

    impl ClientState {
        fn prepare_idle_exit(&mut self, busy: bool, now: Instant) -> bool {
            if busy || self.clients != 0 {
                self.quiet_since = None;
                return false;
            }
            let quiet_since = self.quiet_since.get_or_insert(now);
            if now.duration_since(*quiet_since) < IDLE_TIMEOUT {
                return false;
            }
            self.stopping = true;
            true
        }
    }

    struct ClientLease(Arc<Mutex<ClientState>>);
    impl Drop for ClientLease {
        fn drop(&mut self) {
            self.0.lock().clients -= 1;
        }
    }

    pub(crate) struct ActivationListener {
        state: Arc<Mutex<ClientState>>,
        task: JoinHandle<()>,
    }

    impl ActivationListener {
        #[cfg(not(feature = "test"))]
        pub(crate) fn new() -> Result<Self> {
            Self::bind(&pipe_path())
        }

        fn bind(path: &str) -> Result<Self> {
            // The default Windows pipe DACL permits users to read, not create
            // new server instances. Clients request read access only.
            let mut server = ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .create(path)?;
            let state = Arc::new(Mutex::new(ClientState::default()));
            let task_state = state.clone();
            let path = path.to_owned();
            let task = tokio::spawn(async move {
                let mut clients = JoinSet::new();
                loop {
                    tokio::select! {
                        result = server.connect() => {
                            if let Err(error) = result {
                                tracing::warn!(%error, "service activation listener failed");
                                break;
                            }
                            let next = match ServerOptions::new().reject_remote_clients(true).create(&path) {
                                Ok(next) => next,
                                Err(error) => {
                                    tracing::warn!(%error, "service activation pipe could not be renewed");
                                    break;
                                }
                            };
                            let mut connection = std::mem::replace(&mut server, next);
                            let lease = {
                                let mut state = task_state.lock();
                                if state.stopping {
                                    break;
                                }
                                state.clients += 1;
                                state.quiet_since = None;
                                ClientLease(task_state.clone())
                            };
                            clients.spawn(async move {
                                let _lease = lease;
                                if connection.write_all(&[1]).await.is_ok() {
                                    // GUI clients never write. EOF/broken-pipe observes process death.
                                    let _ = connection.read(&mut [0]).await;
                                }
                            });
                        }
                        _ = clients.join_next(), if !clients.is_empty() => {}
                    }
                }
            });
            Ok(Self { state, task })
        }

        pub(crate) fn prepare_idle_exit(&self, busy: bool) -> bool {
            let mut state = self.state.lock();
            if self.task.is_finished() {
                // Never exit without a functioning endpoint that can observe clients.
                return false;
            }
            if !state.prepare_idle_exit(busy, Instant::now()) {
                return false;
            }
            windows_service_begin_stopping();
            true
        }

        #[cfg(not(feature = "test"))]
        pub(crate) fn is_failed(&self) -> bool {
            self.task.is_finished()
        }
    }

    impl Drop for ActivationListener {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use tokio::net::windows::named_pipe::ClientOptions;

        #[test]
        fn live_gui_or_core_prevents_idle_exit() {
            let now = Instant::now();
            let mut state = ClientState {
                clients: 1,
                quiet_since: Some(now),
                stopping: false,
            };
            assert!(!state.prepare_idle_exit(false, now + IDLE_TIMEOUT));
            state.clients = 0;
            assert!(!state.prepare_idle_exit(true, now + IDLE_TIMEOUT));
        }

        #[test]
        fn service_exits_only_after_a_complete_idle_grace_period() {
            let now = Instant::now();
            let mut state = ClientState::default();
            assert!(!state.prepare_idle_exit(false, now));
            assert!(!state.prepare_idle_exit(false, now + IDLE_TIMEOUT - Duration::from_millis(1)));
            assert!(state.prepare_idle_exit(false, now + IDLE_TIMEOUT));
        }

        #[tokio::test]
        async fn read_only_gui_connection_is_retained_until_its_handle_closes() -> Result<()> {
            let path = format!(
                r"\\.\pipe\zenclash-activation-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos()
            );
            let listener = ActivationListener::bind(&path)?;
            let mut client = ClientOptions::new().read(true).write(false).open(&path)?;
            let mut ack = [0];
            tokio::time::timeout(Duration::from_secs(2), client.read_exact(&mut ack)).await??;
            assert_eq!(ack, [1]);
            assert_eq!(listener.state.lock().clients, 1);
            assert!(!listener.prepare_idle_exit(false));
            drop(client);
            tokio::time::timeout(Duration::from_secs(2), async {
                while listener.state.lock().clients != 0 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await?;
            Ok(())
        }

        #[tokio::test]
        async fn one_gui_closing_does_not_retire_another_gui_connection() -> Result<()> {
            let path = format!(
                r"\\.\pipe\zenclash-activation-multiple-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos()
            );
            let listener = ActivationListener::bind(&path)?;
            let mut first = ClientOptions::new().read(true).write(false).open(&path)?;
            tokio::time::timeout(Duration::from_secs(2), first.read_exact(&mut [0])).await??;
            let mut second = ClientOptions::new().read(true).write(false).open(&path)?;
            tokio::time::timeout(Duration::from_secs(2), second.read_exact(&mut [0])).await??;
            drop(first);
            tokio::time::timeout(Duration::from_secs(2), async {
                while listener.state.lock().clients != 1 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await?;
            assert!(!listener.prepare_idle_exit(false));
            drop(second);
            Ok(())
        }

        #[tokio::test]
        async fn restarted_listener_can_bind_while_a_gui_retains_the_dead_service_handle() -> Result<()> {
            let path = format!(
                r"\\.\pipe\zenclash-activation-restart-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos()
            );
            let mut listener = ActivationListener::bind(&path)?;
            let mut client = ClientOptions::new().read(true).write(false).open(&path)?;
            tokio::time::timeout(Duration::from_secs(2), client.read_exact(&mut [0])).await??;
            listener.task.abort();
            let _ = (&mut listener.task).await;
            let read = tokio::time::timeout(Duration::from_secs(2), client.read_exact(&mut [0])).await?;
            assert!(read.is_err(), "a GUI must observe the dead service connection");

            let replacement = ActivationListener::bind(&path)?;
            let mut reconnected = ClientOptions::new().read(true).write(false).open(&path)?;
            tokio::time::timeout(Duration::from_secs(2), reconnected.read_exact(&mut [0])).await??;
            assert_eq!(replacement.state.lock().clients, 1);
            Ok(())
        }
    }
}

#[cfg(all(feature = "standalone", not(feature = "test")))]
pub(crate) use listener::ActivationListener;

#[cfg(test)]
mod trigger_tests {
    use super::*;

    #[test]
    fn only_the_exact_local_activation_endpoint_can_authorize_idle_exit() {
        let expected = trigger_pipe_name();
        let mut name = expected.clone();
        let mut subtype = NAMED_PIPE_EVENT_GUID;
        let mut data = SERVICE_TRIGGER_SPECIFIC_DATA_ITEM {
            dwDataType: SERVICE_TRIGGER_DATA_TYPE_STRING,
            cbData: std::mem::size_of_val(name.as_slice()) as u32,
            pData: name.as_mut_ptr().cast(),
        };
        let mut trigger = SERVICE_TRIGGER {
            dwTriggerType: SERVICE_TRIGGER_TYPE_NETWORK_ENDPOINT,
            dwAction: SERVICE_TRIGGER_ACTION_SERVICE_START,
            pTriggerSubtype: &mut subtype,
            cDataItems: 1,
            pDataItems: &mut data,
        };
        // SAFETY: every pointer names a live local value for the whole test.
        assert!(unsafe { matches_pipe_trigger(&trigger, &expected) });
        name[0] = u16::from(b'x');
        assert!(!unsafe { matches_pipe_trigger(&trigger, &expected) });
        name[0] = expected[0];
        trigger.dwAction = windows_sys::Win32::System::Services::SERVICE_TRIGGER_ACTION_SERVICE_STOP;
        assert!(!unsafe { matches_pipe_trigger(&trigger, &expected) });
        trigger.dwAction = SERVICE_TRIGGER_ACTION_SERVICE_START;
        trigger.pTriggerSubtype = std::ptr::null_mut();
        assert!(!unsafe { matches_pipe_trigger(&trigger, &expected) });
    }
}
