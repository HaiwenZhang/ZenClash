use std::{io, ptr};
#[cfg(feature = "server")]
use std::{
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(feature = "server")]
use tokio::sync::watch;
use windows_sys::Win32::{Foundation::ERROR_SERVICE_DOES_NOT_EXIST, System::Services::*};
#[cfg(feature = "server")]
use windows_sys::Win32::{
    Foundation::{
        ERROR_INVALID_PARAMETER, ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_MARKED_FOR_DELETE,
        ERROR_SERVICE_NOT_ACTIVE, WAIT_OBJECT_0,
    },
    Storage::FileSystem::{DELETE, READ_CONTROL},
    System::Threading::WaitForSingleObject,
};

use super::{SERVICE_NAME, wide};
#[cfg(feature = "server")]
#[path = "registration.rs"]
mod registration;
#[cfg(feature = "server")]
use super::{
    denied,
    security::{service_root, validate_protected_path},
};

struct ServiceHandle(SC_HANDLE);
impl Drop for ServiceHandle {
    fn drop(&mut self) {
        unsafe { CloseServiceHandle(self.0) };
    }
}

#[cfg(feature = "server")]
const REGISTRATION_ACCESS: u32 = SERVICE_QUERY_CONFIG | READ_CONTROL;

#[cfg(feature = "server")]
fn require_owned_registration(service: &ServiceHandle) -> io::Result<()> {
    super::security::validate_service_registration(service.0)?;
    let root = service_root()?;
    let executable = root.join("zenclash-service.exe");
    registration::check(service.0, &executable)?;
    validate_protected_path(&root, true)?;
    // Repair/uninstall must still stop a fixed registration whose helper is missing.
    match std::fs::symlink_metadata(&executable) {
        Ok(_) => validate_protected_path(&executable, false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn manager(access: u32) -> io::Result<ServiceHandle> {
    let handle = unsafe { OpenSCManagerW(ptr::null(), ptr::null(), access) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(ServiceHandle(handle))
}

fn service(manager: &ServiceHandle, access: u32) -> io::Result<ServiceHandle> {
    let name = wide(SERVICE_NAME)?;
    let handle = unsafe { OpenServiceW(manager.0, name.as_ptr(), access) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(ServiceHandle(handle))
}

#[cfg(feature = "server")]
pub(crate) fn service_state() -> io::Result<Option<u32>> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    let handle = match service(&manager, SERVICE_QUERY_STATUS) {
        Ok(service) => service,
        Err(error) if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    Ok(Some(query(&handle)?.dwCurrentState))
}

pub(super) fn service_pid() -> io::Result<Option<u32>> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    let service = match service(&manager, SERVICE_QUERY_STATUS) {
        Ok(service) => service,
        Err(error) if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let status = query(&service)?;
    Ok(
        (status.dwCurrentState == SERVICE_RUNNING && status.dwProcessId != 0)
            .then_some(status.dwProcessId),
    )
}

fn query(service: &ServiceHandle) -> io::Result<SERVICE_STATUS_PROCESS> {
    let mut status: SERVICE_STATUS_PROCESS = unsafe { std::mem::zeroed() };
    let mut needed = 0;
    if unsafe {
        QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
            std::mem::size_of_val(&status) as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(status)
}

#[cfg(feature = "server")]
pub(crate) fn register_service() -> io::Result<()> {
    let root = service_root()?;
    validate_protected_path(&root, true)?;
    let executable = root.join("zenclash-service.exe");
    validate_protected_path(&executable, false)?;
    let binary = wide(format!("\"{}\" run", executable.display()))?;
    let name = wide(SERVICE_NAME)?;
    let display = wide("ZenClash Runtime Service")?;
    let manager = manager(SC_MANAGER_CREATE_SERVICE | SC_MANAGER_CONNECT)?;
    let existing = service(
        &manager,
        SERVICE_CHANGE_CONFIG | SERVICE_START | SERVICE_QUERY_STATUS | REGISTRATION_ACCESS,
    );
    match existing {
        Ok(existing) => {
            require_owned_registration(&existing)?;
            if query(&existing)?.dwCurrentState != SERVICE_STOPPED {
                return Err(denied("stop service before replacement"));
            }
            let system = wide("LocalSystem")?;
            if unsafe {
                ChangeServiceConfigW(
                    existing.0,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    binary.as_ptr(),
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null(),
                    system.as_ptr(),
                    ptr::null(),
                    display.as_ptr(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            configure_windows_service_recovery(&existing)?;
        }
        Err(error) if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) => {
            let handle = unsafe {
                CreateServiceW(
                    manager.0,
                    name.as_ptr(),
                    display.as_ptr(),
                    SERVICE_QUERY_STATUS
                        | SERVICE_CHANGE_CONFIG
                        | SERVICE_START
                        | REGISTRATION_ACCESS,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    binary.as_ptr(),
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                )
            };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let handle = ServiceHandle(handle);
            require_owned_registration(&handle)?;
            configure_windows_service_recovery(&handle)?;
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

#[cfg(feature = "server")]
pub(crate) fn start_service() -> io::Result<()> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    let service = service(
        &manager,
        SERVICE_START | SERVICE_QUERY_STATUS | SERVICE_CHANGE_CONFIG | REGISTRATION_ACCESS,
    )?;
    require_owned_registration(&service)?;
    configure_then_control(
        || set_start_type(&service, SERVICE_AUTO_START),
        || {
            if unsafe { StartServiceW(service.0, 0, ptr::null()) } == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_SERVICE_ALREADY_RUNNING as i32) {
                    return Err(error);
                }
            }
            Ok(())
        },
    )?;
    wait_state(&service, SERVICE_RUNNING)
}

#[cfg(feature = "server")]
pub(crate) fn stop_service() -> io::Result<()> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    let service = match service(
        &manager,
        SERVICE_STOP | SERVICE_QUERY_STATUS | SERVICE_CHANGE_CONFIG | REGISTRATION_ACCESS,
    ) {
        Ok(service) => service,
        Err(error) if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) => {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    require_owned_registration(&service)?;
    let process = configure_then_control(
        || set_start_type(&service, SERVICE_DISABLED),
        || {
            let previous = query(&service)?;
            let process = if previous.dwProcessId != 0 {
                match super::identity::process_handle(previous.dwProcessId) {
                    Ok(process) => Some(process),
                    Err(error) if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) => {
                        None
                    }
                    Err(error) => return Err(error),
                }
            } else {
                None
            };
            let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
            if unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) } == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_SERVICE_NOT_ACTIVE as i32) {
                    return Err(error);
                }
            }
            Ok(process)
        },
    )?;
    wait_state(&service, SERVICE_STOPPED)?;
    // SCM STOPPED is reported by the callback before the hosting process exits.
    // Await the pinned process before replacing its image or approving a restart.
    if let Some(process) = process
        && unsafe { WaitForSingleObject(process.0, 10000) } != WAIT_OBJECT_0
    {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "service stopped but host process has not exited",
        ));
    }
    Ok(())
}

#[cfg(feature = "server")]
fn wait_state(service: &ServiceHandle, expected: u32) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = query(service)?;
        if status.dwCurrentState == expected {
            return Ok(());
        }
        if expected == SERVICE_RUNNING && status.dwCurrentState == SERVICE_STOPPED {
            return Err(io::Error::other("service exited before becoming ready"));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "service transition timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(feature = "server")]
pub(crate) fn unregister_service() -> io::Result<()> {
    match stop_service() {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(ERROR_SERVICE_MARKED_FOR_DELETE as i32) => {
            return wait_service_absent(&manager(SC_MANAGER_CONNECT)?);
        }
        Err(error) => return Err(error),
    }
    let manager = manager(SC_MANAGER_CONNECT)?;
    let handle = match service(&manager, DELETE | REGISTRATION_ACCESS) {
        Ok(service) => service,
        Err(error) if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) => {
            return Ok(());
        }
        Err(error) if error.raw_os_error() == Some(ERROR_SERVICE_MARKED_FOR_DELETE as i32) => {
            return wait_service_absent(&manager);
        }
        Err(error) => return Err(error),
    };
    require_owned_registration(&handle)?;
    delete_and_wait(
        handle,
        |service| {
            if unsafe { DeleteService(service.0) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        },
        || service_absent(service(&manager, SERVICE_QUERY_STATUS)),
        || std::thread::sleep(Duration::from_millis(100)),
        200,
    )
}

#[cfg(feature = "server")]
fn wait_service_absent(manager: &ServiceHandle) -> io::Result<()> {
    poll_until(
        200,
        || service_absent(service(manager, SERVICE_QUERY_STATUS)),
        || std::thread::sleep(Duration::from_millis(100)),
    )
}

#[cfg(feature = "server")]
fn service_absent<T>(opened: io::Result<T>) -> io::Result<bool> {
    match opened {
        Ok(_) => Ok(false),
        Err(error) if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) => Ok(true),
        Err(error) if error.raw_os_error() == Some(ERROR_SERVICE_MARKED_FOR_DELETE as i32) => {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

#[cfg(feature = "server")]
fn configure_windows_service_recovery(service: &ServiceHandle) -> io::Result<()> {
    configure_recovery_with(|level, configuration| {
        // SAFETY: The synchronous callback receives a live native structure and its backing array.
        if unsafe { ChangeServiceConfig2W(service.0, level, configuration) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })
}

// Adapted from Tunglies' clash-verge-service-ipc install_service.rs (GPL-3.0); see UPSTREAM.md.
#[cfg(feature = "server")]
fn configure_recovery_with(
    mut update: impl FnMut(SERVICE_CONFIG, *const std::ffi::c_void) -> io::Result<()>,
) -> io::Result<()> {
    let mut actions = [5000, 10000, 30000].map(|delay| SC_ACTION {
        Type: SC_ACTION_RESTART,
        Delay: delay,
    });
    let failure_actions = SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: 24 * 60 * 60,
        lpRebootMsg: ptr::null_mut(),
        lpCommand: ptr::null_mut(),
        cActions: actions.len() as u32,
        lpsaActions: actions.as_mut_ptr(),
    };
    update(
        SERVICE_CONFIG_FAILURE_ACTIONS,
        (&failure_actions as *const SERVICE_FAILURE_ACTIONSW).cast(),
    )?;
    let flag = SERVICE_FAILURE_ACTIONS_FLAG {
        fFailureActionsOnNonCrashFailures: 1,
    };
    update(
        SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
        (&flag as *const SERVICE_FAILURE_ACTIONS_FLAG).cast(),
    )
}

#[cfg(feature = "server")]
fn set_start_type(service: &ServiceHandle, start_type: u32) -> io::Result<()> {
    if unsafe {
        ChangeServiceConfigW(
            service.0,
            SERVICE_NO_CHANGE,
            start_type,
            SERVICE_NO_CHANGE,
            ptr::null(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(feature = "server")]
fn configure_then_control<T>(
    configure: impl FnOnce() -> io::Result<()>,
    control: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    // DISABLED prevents a queued SCM recovery restart from replacing the host before it is pinned.
    // Explicit start restores AUTO_START before requesting the transition.
    configure()?;
    control()
}

#[cfg(feature = "server")]
fn delete_and_wait<T>(
    handle: T,
    delete: impl FnOnce(&T) -> io::Result<()>,
    probe: impl FnMut() -> io::Result<bool>,
    pause: impl FnMut(),
    attempts: usize,
) -> io::Result<()> {
    match delete(&handle) {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(ERROR_SERVICE_MARKED_FOR_DELETE as i32) => {}
        Err(error) => return Err(error),
    }
    // A retained service handle can itself keep a stopped, marked service from being deleted.
    drop(handle);
    poll_until(attempts, probe, pause)
}

// Adapted from Tunglies' clash-verge-service-ipc uninstall_service.rs (GPL-3.0); see UPSTREAM.md.
#[cfg(feature = "server")]
fn poll_until(
    attempts: usize,
    mut probe: impl FnMut() -> io::Result<bool>,
    mut pause: impl FnMut(),
) -> io::Result<()> {
    for attempt in 0..attempts {
        if probe()? {
            return Ok(());
        }
        if attempt + 1 < attempts {
            pause();
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "service remains present or marked for deletion",
    ))
}

#[cfg(feature = "server")]
type ServiceRunner = fn(watch::Receiver<bool>) -> io::Result<()>;
#[cfg(feature = "server")]
static RUNNER: OnceLock<ServiceRunner> = OnceLock::new();
#[cfg(feature = "server")]
static STOP: OnceLock<watch::Sender<bool>> = OnceLock::new();
#[cfg(feature = "server")]
static STATUS: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "server")]
pub(crate) fn dispatch_service(runner: ServiceRunner) -> io::Result<()> {
    RUNNER
        .set(runner)
        .map_err(|_| io::Error::other("service dispatcher already started"))?;
    let mut name = wide(SERVICE_NAME)?;
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(feature = "server")]
unsafe extern "system" fn service_main(_: u32, _: *mut *mut u16) {
    let Ok(name) = wide(SERVICE_NAME) else {
        return;
    };
    let handle = unsafe {
        RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(control_handler), ptr::null_mut())
    };
    if handle.is_null() {
        return;
    }
    STATUS.store(handle as usize, Ordering::Release);
    let (sender, receiver) = watch::channel(false);
    if STOP.set(sender).is_err() {
        report(SERVICE_STOPPED, 1);
        return;
    }
    report(SERVICE_START_PENDING, 0);
    let result = RUNNER
        .get()
        .ok_or_else(|| io::Error::other("service runner missing"))
        .and_then(|run| run(receiver));
    report(SERVICE_STOPPED, if result.is_ok() { 0 } else { 1 });
}

#[cfg(feature = "server")]
pub(crate) fn report_ready() {
    report(SERVICE_RUNNING, 0);
}

#[cfg(feature = "server")]
unsafe extern "system" fn control_handler(
    control: u32,
    _: u32,
    _: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
) -> u32 {
    if matches!(control, SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN) {
        report(SERVICE_STOP_PENDING, 0);
        if let Some(stop) = STOP.get() {
            let _ = stop.send(true);
        }
    }
    0
}

#[cfg(feature = "server")]
fn report(state: u32, error: u32) {
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: error,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: u32::from(matches!(
            state,
            SERVICE_START_PENDING | SERVICE_STOP_PENDING
        )),
        dwWaitHint: if matches!(state, SERVICE_START_PENDING | SERVICE_STOP_PENDING) {
            30000
        } else {
            0
        },
    };
    let handle = STATUS.load(Ordering::Acquire) as SERVICE_STATUS_HANDLE;
    if !handle.is_null() {
        unsafe { SetServiceStatus(handle, &status) };
    }
}

#[cfg(all(test, feature = "server"))]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn recovery_writes_native_restart_actions_then_non_crash_flag() {
        let calls = RefCell::new(Vec::new());
        configure_recovery_with(|level, value| {
            calls.borrow_mut().push(level);
            match level {
                SERVICE_CONFIG_FAILURE_ACTIONS => {
                    // SAFETY: configure_recovery_with owns this struct/array for this callback.
                    let actions = unsafe { &*value.cast::<SERVICE_FAILURE_ACTIONSW>() };
                    assert_eq!((actions.dwResetPeriod, actions.cActions), (86400, 3));
                    assert!(actions.lpRebootMsg.is_null());
                    assert!(actions.lpCommand.is_null());
                    let native = unsafe {
                        std::slice::from_raw_parts(actions.lpsaActions, actions.cActions as usize)
                    };
                    let observed: Vec<_> = native
                        .iter()
                        .map(|action| (action.Type, action.Delay))
                        .collect();
                    assert_eq!(
                        observed,
                        [
                            (SC_ACTION_RESTART, 5000),
                            (SC_ACTION_RESTART, 10000),
                            (SC_ACTION_RESTART, 30000)
                        ]
                    );
                }
                SERVICE_CONFIG_FAILURE_ACTIONS_FLAG => {
                    // SAFETY: The flag is live for this synchronous callback.
                    let flag = unsafe { &*value.cast::<SERVICE_FAILURE_ACTIONS_FLAG>() };
                    assert_eq!(flag.fFailureActionsOnNonCrashFailures, 1);
                }
                _ => panic!("unexpected native recovery operation"),
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(
            *calls.borrow(),
            [
                SERVICE_CONFIG_FAILURE_ACTIONS,
                SERVICE_CONFIG_FAILURE_ACTIONS_FLAG
            ]
        );
    }

    #[test]
    fn recovery_failure_preserves_native_error_and_does_not_skip_failed_step() {
        for failed_step in [1, 2] {
            let calls = Cell::new(0);
            let result = configure_recovery_with(|_, _| {
                calls.set(calls.get() + 1);
                if calls.get() == failed_step {
                    Err(io::Error::from_raw_os_error(5))
                } else {
                    Ok(())
                }
            });
            assert_eq!(result.unwrap_err().raw_os_error(), Some(5));
            assert_eq!(calls.get(), failed_step);
        }
    }

    #[test]
    fn explicit_start_restores_auto_after_failed_maintenance_stop() {
        let start_type = Cell::new(SERVICE_AUTO_START);
        let stopped: io::Result<()> = configure_then_control(
            || {
                start_type.set(SERVICE_DISABLED);
                Ok(())
            },
            || Err(io::Error::from_raw_os_error(5)),
        );
        assert!(stopped.is_err());
        configure_then_control(
            || {
                start_type.set(SERVICE_AUTO_START);
                Ok(())
            },
            || {
                assert_eq!(start_type.get(), SERVICE_AUTO_START);
                Ok(())
            },
        )
        .unwrap();
    }

    #[test]
    fn marked_for_delete_is_pending_until_not_found_and_drops_probe_handle() {
        let closed = Cell::new(false);
        assert!(!service_absent(Ok(TrackedHandle(&closed))).unwrap());
        assert!(closed.get());
        assert!(
            !service_absent::<()>(Err(io::Error::from_raw_os_error(
                ERROR_SERVICE_MARKED_FOR_DELETE as i32
            )))
            .unwrap()
        );
        assert!(
            service_absent::<()>(Err(io::Error::from_raw_os_error(
                ERROR_SERVICE_DOES_NOT_EXIST as i32
            )))
            .unwrap()
        );
        assert_eq!(
            service_absent::<()>(Err(io::Error::from_raw_os_error(5)))
                .unwrap_err()
                .raw_os_error(),
            Some(5)
        );
    }

    #[test]
    fn concurrently_marked_delete_closes_handle_and_waits_for_absence() {
        let closed = Cell::new(false);
        let probes = Cell::new(0);
        delete_and_wait(
            TrackedHandle(&closed),
            |_| {
                Err(io::Error::from_raw_os_error(
                    ERROR_SERVICE_MARKED_FOR_DELETE as i32,
                ))
            },
            || {
                assert!(closed.get());
                probes.set(probes.get() + 1);
                service_absent::<()>(Err(io::Error::from_raw_os_error(if probes.get() == 1 {
                    ERROR_SERVICE_MARKED_FOR_DELETE as i32
                } else {
                    ERROR_SERVICE_DOES_NOT_EXIST as i32
                })))
            },
            || {},
            2,
        )
        .unwrap();
        assert_eq!(probes.get(), 2);
    }

    #[test]
    fn failed_delete_drops_handle_and_preserves_error_without_polling() {
        let closed = Cell::new(false);
        let result = delete_and_wait(
            TrackedHandle(&closed),
            |_| Err(io::Error::from_raw_os_error(5)),
            || panic!("failed deletion must not be reported as absence"),
            || panic!("failed deletion must not retry"),
            3,
        );
        assert!(closed.get());
        assert_eq!(result.unwrap_err().raw_os_error(), Some(5));
    }

    #[test]
    fn probe_failure_does_not_become_absence_or_timeout() {
        let result = delete_and_wait(
            (),
            |_| Ok(()),
            || Err(io::Error::from_raw_os_error(5)),
            || panic!("probe access failure must not retry"),
            3,
        );
        assert_eq!(result.unwrap_err().raw_os_error(), Some(5));
    }

    #[test]
    fn maintenance_pins_host_after_disabling_before_stop_and_wait() {
        let calls = RefCell::new(Vec::new());
        let pinned = configure_then_control(
            || {
                calls.borrow_mut().push("disable");
                Ok(())
            },
            || {
                calls.borrow_mut().extend(["observe", "pin", "stop"]);
                Ok(Some(42))
            },
        )
        .unwrap();
        calls.borrow_mut().push("wait-stopped");
        assert_eq!(pinned, Some(42));
        calls.borrow_mut().push("wait-pinned-host");
        assert_eq!(
            *calls.borrow(),
            [
                "disable",
                "observe",
                "pin",
                "stop",
                "wait-stopped",
                "wait-pinned-host"
            ]
        );
    }

    #[test]
    fn maintenance_disables_before_stop_and_explicit_start_restores_auto() {
        for start_type in [SERVICE_DISABLED, SERVICE_AUTO_START] {
            let calls = RefCell::new(Vec::new());
            configure_then_control(
                || {
                    calls.borrow_mut().push(start_type);
                    Ok(())
                },
                || {
                    calls.borrow_mut().push(999);
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(*calls.borrow(), [start_type, 999]);
        }
    }

    #[test]
    fn configuration_failure_never_sends_stop_or_start() {
        let control_sent = Cell::new(false);
        let result = configure_then_control(
            || Err(io::Error::from_raw_os_error(5)),
            || {
                control_sent.set(true);
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err().raw_os_error(), Some(5));
        assert!(!control_sent.get());
    }

    #[test]
    fn failed_stop_keeps_disabled_state_until_explicit_repair_or_start() {
        let start_type = Cell::new(SERVICE_AUTO_START);
        let result: io::Result<()> = configure_then_control(
            || {
                start_type.set(SERVICE_DISABLED);
                Ok(())
            },
            || Err(io::Error::from_raw_os_error(5)),
        );
        assert_eq!(result.unwrap_err().raw_os_error(), Some(5));
        assert_eq!(start_type.get(), SERVICE_DISABLED);
    }

    struct TrackedHandle<'a>(&'a Cell<bool>);
    impl Drop for TrackedHandle<'_> {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    #[test]
    fn delete_closes_own_handle_before_poll_and_waits_for_absence() {
        let closed = Cell::new(false);
        let probes = Cell::new(0);
        let pauses = Cell::new(0);
        delete_and_wait(
            TrackedHandle(&closed),
            |_| {
                assert!(!closed.get());
                Ok(())
            },
            || {
                assert!(closed.get());
                probes.set(probes.get() + 1);
                Ok(probes.get() == 3)
            },
            || pauses.set(pauses.get() + 1),
            3,
        )
        .unwrap();
        assert_eq!((probes.get(), pauses.get()), (3, 2));
    }

    #[test]
    fn delete_timeout_does_not_report_uninstall_success() {
        let probes = Cell::new(0);
        let pauses = Cell::new(0);
        let result = delete_and_wait(
            (),
            |_| Ok(()),
            || {
                probes.set(probes.get() + 1);
                Ok(false)
            },
            || pauses.set(pauses.get() + 1),
            3,
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!((probes.get(), pauses.get()), (3, 2));
    }
}
