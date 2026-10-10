//! launchd activation over XPC; authenticated commands still use the existing Unix socket.

#[cfg(feature = "client")]
use anyhow::Context as _;
use anyhow::{Result, anyhow};
use block2::{Block, RcBlock};
use parking_lot::Mutex;
#[cfg(feature = "standalone")]
use std::{collections::BTreeMap, time::Instant};
use std::{
    ffi::{CString, c_char, c_void},
    ptr::NonNull,
    sync::Arc,
    time::Duration,
};

type Xpc = *mut c_void;
#[repr(C)]
struct XpcType {
    _opaque: [u8; 0],
}

#[link(name = "System")]
unsafe extern "C" {
    fn xpc_connection_create_mach_service(name: *const c_char, queue: Xpc, flags: u64) -> Xpc;
    fn xpc_connection_set_event_handler(connection: Xpc, handler: &Block<dyn Fn(Xpc)>);
    fn xpc_connection_resume(connection: Xpc);
    fn xpc_connection_cancel(connection: Xpc);
    #[cfg(feature = "client")]
    fn xpc_connection_send_message_with_reply(connection: Xpc, message: Xpc, queue: Xpc, handler: &Block<dyn Fn(Xpc)>);
    #[cfg(feature = "client")]
    fn xpc_dictionary_create(keys: *const *const c_char, values: *const Xpc, count: usize) -> Xpc;
    fn xpc_dictionary_set_bool(dictionary: Xpc, key: *const c_char, value: bool);
    fn xpc_dictionary_get_bool(dictionary: Xpc, key: *const c_char) -> bool;
    fn xpc_get_type(object: Xpc) -> *const XpcType;
    fn xpc_release(object: Xpc);
    static _xpc_type_dictionary: XpcType;
    #[cfg(feature = "standalone")]
    fn xpc_retain(object: Xpc) -> Xpc;
    #[cfg(feature = "standalone")]
    fn xpc_dictionary_create_reply(message: Xpc) -> Xpc;
    #[cfg(feature = "standalone")]
    fn xpc_dictionary_get_remote_connection(message: Xpc) -> Xpc;
    #[cfg(feature = "standalone")]
    fn xpc_connection_send_message(connection: Xpc, message: Xpc);
    #[cfg(feature = "standalone")]
    static _xpc_type_connection: XpcType;
}

struct Connection(NonNull<c_void>);
// XPC connections are reference-counted and their operations are thread-safe. Callback
// captures own only synchronized Rust state; no borrowed native event escapes a callback.
unsafe impl Send for Connection {}
unsafe impl Sync for Connection {}

impl Connection {
    fn new(name: &str, flags: u64) -> Result<Self> {
        let name = CString::new(name)?;
        // SAFETY: name is a live C string; a null queue selects XPC's background queue.
        let raw = unsafe { xpc_connection_create_mach_service(name.as_ptr(), std::ptr::null_mut(), flags) };
        NonNull::new(raw)
            .map(Self)
            .ok_or_else(|| anyhow!("could not create the service activation connection"))
    }
    fn pointer(&self) -> Xpc {
        self.0.as_ptr()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns one native reference. Cancellation finishes asynchronously;
        // XPC retains callbacks and events for their execution independently of this reference.
        unsafe {
            xpc_connection_cancel(self.pointer());
            xpc_release(self.pointer());
        }
    }
}

#[cfg(all(feature = "client", not(feature = "test")))]
static CLIENT: once_cell::sync::Lazy<Mutex<Option<Arc<Connection>>>> = once_cell::sync::Lazy::new(|| Mutex::new(None));

#[cfg(all(feature = "client", not(feature = "test")))]
pub(crate) async fn wake_service() -> Result<()> {
    for attempt in 0..2 {
        let connection = client_connection()?;
        match send_wake(&connection).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                let timed_out = error.downcast_ref::<tokio::time::error::Elapsed>().is_some();
                let mut slot = CLIENT.lock();
                if slot.as_ref().is_some_and(|current| Arc::ptr_eq(current, &connection)) {
                    *slot = None;
                }
                drop(slot);
                if attempt == 1 || timed_out {
                    return Err(error);
                }
            }
        }
        // An idle daemon may have committed its exit just before the new client arrived.
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(anyhow!("the service could not be activated"))
}

#[cfg(all(feature = "client", not(feature = "test")))]
fn client_connection() -> Result<Arc<Connection>> {
    let connection = {
        let mut slot = CLIENT.lock();
        if slot.is_none() {
            // PRIVILEGED resolves the system LaunchDaemon, not a same-named user agent.
            let connection = Connection::new(crate::MACOS_SERVICE_ID, 1 << 1)?;
            let handler = RcBlock::new(|_: Xpc| {});
            // SAFETY: the callback has no unsynchronized captures; XPC copies the block.
            unsafe {
                xpc_connection_set_event_handler(connection.pointer(), &handler);
                xpc_connection_resume(connection.pointer());
            }
            *slot = Some(Arc::new(connection));
        }
        slot.as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("service activation connection is missing"))?
    };
    Ok(connection)
}

#[cfg(feature = "client")]
async fn send_wake(connection: &Connection) -> Result<()> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    {
        let tx_slot = Mutex::new(Some(tx));
        let reply = RcBlock::new(move |event: Xpc| {
            // SAFETY: the event is borrowed for this callback; type checking precedes dictionary access.
            let ready = unsafe {
                !event.is_null()
                    && xpc_get_type(event) == &raw const _xpc_type_dictionary
                    && xpc_dictionary_get_bool(event, c"ready".as_ptr())
            };
            // The reply block is called once, but Fn requires synchronized interior mutability.
            if let Some(tx) = tx_slot.lock().take() {
                let _ = tx.send(ready);
            }
        });
        // SAFETY: an empty dictionary is valid; send copies both the dictionary and reply block.
        unsafe {
            let message = xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0);
            if message.is_null() {
                return Err(anyhow!("could not allocate the service activation request"));
            }
            xpc_dictionary_set_bool(message, c"wake".as_ptr(), true);
            xpc_connection_send_message_with_reply(connection.pointer(), message, std::ptr::null_mut(), &reply);
            xpc_release(message);
        }
    }
    // launchd may first need to recover an accepted core. Match the bounded lifecycle
    // budget rather than declaring a healthy, cold service incompatible after a short probe.
    match tokio::time::timeout(Duration::from_secs(30), rx)
        .await
        .context("service activation timed out")??
    {
        true => Ok(()),
        false => Err(anyhow!("the service activation connection was rejected or interrupted")),
    }
}

#[cfg(feature = "standalone")]
#[derive(Default)]
struct Peers {
    connections: BTreeMap<usize, Connection>,
    idle_since: Option<Instant>,
    closing: bool,
}

#[cfg(feature = "standalone")]
pub(crate) struct ActivationListener {
    _connection: Connection,
    peers: Arc<Mutex<Peers>>,
}

#[cfg(feature = "standalone")]
impl ActivationListener {
    pub(crate) fn new(name: &str) -> Result<Self> {
        let connection = Connection::new(name, 1 << 0)?;
        let peers = Arc::new(Mutex::new(Peers::default()));
        let observed = Arc::clone(&peers);
        let handler = RcBlock::new(move |peer: Xpc| {
            // SAFETY: listener events are borrowed native objects and can be connections or errors.
            unsafe {
                if peer.is_null() || xpc_get_type(peer) != &raw const _xpc_type_connection {
                    return;
                }
                let mut state = observed.lock();
                if state.closing {
                    xpc_connection_cancel(peer);
                    return;
                }
                xpc_retain(peer);
                let Some(pointer) = NonNull::new(peer) else {
                    return;
                };
                state.connections.insert(peer as usize, Connection(pointer));
                state.idle_since = None;
                drop(state);
                let weak = Arc::downgrade(&observed);
                let key = peer as usize;
                let messages = RcBlock::new(move |message: Xpc| {
                    let Some(peers) = weak.upgrade() else {
                        return;
                    };
                    if message.is_null() {
                        return;
                    }
                    if xpc_get_type(message) != &raw const _xpc_type_dictionary {
                        let retired = peers.lock().connections.remove(&key);
                        drop(retired);
                        return;
                    }
                    let reply = xpc_dictionary_create_reply(message);
                    if !reply.is_null() {
                        xpc_dictionary_set_bool(
                            reply,
                            c"ready".as_ptr(),
                            xpc_dictionary_get_bool(message, c"wake".as_ptr()) && !peers.lock().closing,
                        );
                        let remote = xpc_dictionary_get_remote_connection(message);
                        if !remote.is_null() {
                            xpc_connection_send_message(remote, reply);
                        }
                        xpc_release(reply);
                    }
                });
                xpc_connection_set_event_handler(peer, &messages);
                xpc_connection_resume(peer);
            }
        });
        // SAFETY: synchronized captures are Send + Sync; XPC copies the callback before return.
        unsafe {
            xpc_connection_set_event_handler(connection.pointer(), &handler);
            xpc_connection_resume(connection.pointer());
        }
        Ok(Self {
            _connection: connection,
            peers,
        })
    }

    /// Called while holding the authenticated owner lifecycle lock. Closing admission and
    /// checking native clients share one lock, so a new client cannot race the idle decision.
    pub(crate) fn prepare_idle_exit(&self, owner_active: bool) -> bool {
        let mut state = self.peers.lock();
        if owner_active || !state.connections.is_empty() {
            state.idle_since = None;
            return false;
        }
        let now = Instant::now();
        let since = *state.idle_since.get_or_insert(now);
        if now.duration_since(since) < Duration::from_secs(2) {
            return false;
        }
        state.closing = true;
        true
    }
}

#[cfg(all(test, feature = "standalone", feature = "client"))]
mod tests {
    use super::*;
    use std::{
        fs,
        io::Write,
        path::PathBuf,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    #[ignore = "child process for the native launchd lifecycle test"]
    fn activation_fixture_process() -> Result<()> {
        let Ok(name) = std::env::var("ZENCLASH_TEST_ACTIVATION_NAME") else {
            return Ok(());
        };
        let output = PathBuf::from(std::env::var("ZENCLASH_TEST_ACTIVATION_OUTPUT")?);
        let owner = output.with_extension("owner");
        let listener = ActivationListener::new(&name)?;
        let pid = std::process::id();
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&output)?
            .write_all(format!("start {pid}\n").as_bytes())?;
        loop {
            std::thread::sleep(Duration::from_millis(100));
            if listener.prepare_idle_exit(owner.exists()) {
                break;
            }
        }
        fs::OpenOptions::new()
            .append(true)
            .open(output)?
            .write_all(format!("exit {pid}\n").as_bytes())?;
        Ok(())
    }

    struct LaunchdFixture {
        target: String,
        root: PathBuf,
    }
    impl Drop for LaunchdFixture {
        fn drop(&mut self) {
            let _ = Command::new("launchctl").args(["bootout", &self.target]).output();
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn native_client(name: &str) -> Result<Connection> {
        let connection = Connection::new(name, 0)?;
        let handler = RcBlock::new(|_: Xpc| {});
        // SAFETY: a capture-free block can run on XPC's queue. The connection remains owned
        // by the test until the simulated GUI process releases it.
        unsafe {
            xpc_connection_set_event_handler(connection.pointer(), &handler);
            xpc_connection_resume(connection.pointer());
        }
        Ok(connection)
    }

    fn wait_until(output: &std::path::Path, predicate: impl Fn(&str) -> bool) -> Result<String> {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let text = fs::read_to_string(output).unwrap_or_default();
            if predicate(&text) {
                return Ok(text);
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "launchd fixture did not reach its expected state: {text}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    #[ignore = "registers an isolated job in the logged-in macOS user's launchd domain"]
    fn launchd_starts_on_demand_exits_after_last_client_and_starts_again() -> Result<()> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let name = format!("org.zenclash.activation-test.{}-{stamp}", std::process::id());
        let root = std::env::temp_dir().join(&name);
        fs::create_dir(&root)?;
        // SAFETY: geteuid has no pointer arguments or side effects.
        let domain = format!("gui/{}", unsafe { platform_lib::geteuid() });
        let fixture = LaunchdFixture {
            target: format!("{domain}/{name}"),
            root,
        };
        let output = fixture.root.join("lifecycle.txt");
        let program = std::env::current_exe()?.to_string_lossy().into_owned();
        let mut plist = format!(
            include_str!("../resources/launchd.plist.tmpl"),
            group_name = "staff",
            service_id = name,
            app_bundle_id = "org.zenclash.activation-test",
            service_binary = program,
        );
        plist = plist.replace(
            &format!("<string>{program}</string>"),
            &format!("<string>{program}</string><string>--ignored</string><string>--exact</string><string>macos_activation::tests::activation_fixture_process</string><string>--nocapture</string>"),
        );
        plist = plist.replace("</dict>\n</plist>", &format!(
            "<key>EnvironmentVariables</key><dict><key>ZENCLASH_TEST_ACTIVATION_NAME</key><string>{name}</string><key>ZENCLASH_TEST_ACTIVATION_OUTPUT</key><string>{}</string></dict></dict>\n</plist>", output.display()
        ));
        let path = fixture.root.join("job.plist");
        fs::write(&path, plist)?;
        let registered = Command::new("launchctl")
            .args(["bootstrap", &domain])
            .arg(&path)
            .output()?;
        anyhow::ensure!(
            registered.status.success(),
            "launchd registration failed: {}",
            String::from_utf8_lossy(&registered.stderr)
        );
        std::thread::sleep(Duration::from_millis(300));
        assert!(!output.exists(), "registration must not start the service");
        let runtime = tokio::runtime::Runtime::new()?;
        let first = native_client(&name)?;
        runtime.block_on(send_wake(&first))?;
        let second = native_client(&name)?;
        runtime.block_on(send_wake(&second))?;
        wait_until(&output, |text| {
            text.lines().filter(|line| line.starts_with("start ")).count() == 1
        })?;
        drop(first);
        std::thread::sleep(Duration::from_millis(2500));
        assert!(
            !fs::read_to_string(&output)?.contains("exit "),
            "the remaining client must keep the service available"
        );
        let owner = output.with_extension("owner");
        fs::write(&owner, "active owner")?;
        drop(second);
        std::thread::sleep(Duration::from_millis(2500));
        assert!(
            !fs::read_to_string(&output)?.contains("exit "),
            "an active core owner must be retired before the daemon exits"
        );
        fs::remove_file(owner)?;
        wait_until(&output, |text| text.contains("exit "))?;
        let third = native_client(&name)?;
        runtime.block_on(send_wake(&third))?;
        let starts = wait_until(&output, |text| {
            text.lines().filter(|line| line.starts_with("start ")).count() == 2
        })?;
        let pids = starts
            .lines()
            .filter_map(|line| line.strip_prefix("start "))
            .collect::<Vec<_>>();
        assert_ne!(pids[0], pids[1], "the second activation must start a new process");
        drop(third);
        wait_until(&output, |text| {
            text.lines().filter(|line| line.starts_with("exit ")).count() == 2
        })?;
        Ok(())
    }
}
