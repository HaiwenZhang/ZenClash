//! Actual ordinary child reads; these fixtures do not create a TUN device.

use super::*;
use std::{fs, path::PathBuf, process::Command};

struct Fixture {
    root: PathBuf,
    launch: MihomoLaunchConfig,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "zenclash-current-child-geodata-frozen-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let source = root.join("child.rs");
        fs::write(&source, r#"
use std::{fs, io::Read, path::PathBuf, time::Duration};
fn wait(path: &std::path::Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !path.exists() && std::time::Instant::now() < deadline { std::thread::sleep(Duration::from_millis(5)); }
    assert!(path.exists());
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let value = |flag: &str| args[args.iter().position(|arg| arg == flag).unwrap() + 1].clone();
    let home = PathBuf::from(value("-d"));
    let config = value("-f");
    if args.iter().any(|arg| arg == "-t") {
        let payload = if std::env::var_os("CLASH_CONFIG_STRING").is_some() {
            b"tun: {enable: true}\n".to_vec()
        } else { fs::read(config).unwrap() };
        fs::write(home.join("checked"), payload).unwrap();
        wait(&home.join("allow-check"));
        return;
    }
    let n: usize = fs::read_to_string(home.join("launch-count")).unwrap_or_default().parse().unwrap_or(0) + 1;
    fs::write(home.join("launch-count"), n.to_string()).unwrap();
    wait(&home.join(format!("read-{n}")));
    let payload = if std::env::var_os("CLASH_CONFIG_STRING").is_some() {
        b"tun: {enable: true}\n".to_vec()
    } else if config == "-" {
        let mut bytes = Vec::new(); std::io::stdin().read_to_end(&mut bytes).unwrap(); bytes
    } else { fs::read(config).unwrap() };
    fs::write(home.join(format!("observed-{n}")), payload).unwrap();
    std::thread::sleep(Duration::from_secs(30));
}
"#).unwrap();
        let binary = root.join(if cfg!(windows) {
            "mihomo.exe"
        } else {
            "mihomo"
        });
        let compiled = Command::new("rustc")
            .arg("--edition=2024")
            .arg(source)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let config = root.join("profile.yaml");
        fs::write(&config, "tun: {enable: false}\nmode: direct\n").unwrap();
        Self {
            root,
            launch: MihomoLaunchConfig {
                kind: CoreKind::Mihomo,
                binary,
                config_file: config,
                home_dir: home,
                endpoint: MihomoEndpoint::default(),
                controller_override: None,
            },
        }
    }

    fn wait(&self, file: &str) -> PathBuf {
        let path = self.launch.home_dir.join(file);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(path.exists(), "fixture did not publish {file}");
        path
    }

    fn private_files(&self) -> usize {
        fs::read_dir(&self.launch.home_dir)
            .unwrap()
            .filter(|entry| {
                let name = entry.as_ref().unwrap().file_name();
                let name = name.to_string_lossy();
                name.starts_with(".zenclash-start-") || name.starts_with(".zenclash-check-")
            })
            .count()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn frozen_fifo_restart_is_rejected_without_stopping_the_current_child() {
    use std::{
        ffi::CString,
        io::Write,
        os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    };
    let fixture = Fixture::new("fifo");
    fs::write(fixture.launch.home_dir.join("read-1"), "").unwrap();
    fs::write(fixture.launch.home_dir.join("allow-check"), "").unwrap();
    let process = MihomoProcess::spawn(fixture.launch.clone()).unwrap();
    fixture.wait("observed-1");
    let pid = process.snapshot().pid;
    fs::remove_file(&fixture.launch.config_file).unwrap();
    let path = CString::new(fixture.launch.config_file.as_os_str().as_bytes()).unwrap();
    // SAFETY: path is a valid, NUL-terminated private fixture pathname.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let restarting = process.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || sender.send(restarting.restart()).unwrap());
    let early = receiver.recv_timeout(Duration::from_secs(1));
    let bounded = early.is_ok();
    if !bounded {
        // Release a regressed blocking open before asserting, so no worker leaks.
        let mut writer = fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fixture.launch.config_file)
            .unwrap();
        writer.write_all(b"tun: {enable: false}\n").unwrap();
    }
    let result = early.unwrap_or_else(|_| receiver.recv_timeout(Duration::from_secs(5)).unwrap());
    worker.join().unwrap();
    let current = process.snapshot().pid;
    process.stop().unwrap();
    assert!(bounded, "opening a FIFO blocked restart");
    assert!(result.is_err());
    assert_eq!(current, pid);
    assert_eq!(fixture.private_files(), 0);
}

#[test]
fn frozen_spawn_reads_checked_bytes_after_original_changes_to_tun_on() {
    let fixture = Fixture::new("spawn");
    let process = MihomoProcess::spawn(fixture.launch.clone()).unwrap();
    fixture.wait("launch-count");
    fs::write(&fixture.launch.config_file, "tun: {enable: true}\n").unwrap();
    fs::write(fixture.launch.home_dir.join("read-1"), "").unwrap();
    let observed = fs::read_to_string(fixture.wait("observed-1")).unwrap();
    process.stop().unwrap();
    assert_eq!(observed, "tun: {enable: false}\nmode: direct\n");
    assert_eq!(
        fs::read_to_string(&fixture.launch.config_file).unwrap(),
        "tun: {enable: true}\n"
    );
}

#[test]
fn frozen_restart_reuses_prechecked_bytes_after_original_changes() {
    let fixture = Fixture::new("restart");
    fs::write(fixture.launch.home_dir.join("read-1"), "").unwrap();
    let process = MihomoProcess::spawn(fixture.launch.clone()).unwrap();
    fixture.wait("observed-1");
    let previous = process.snapshot().pid;
    let restarting = process.clone();
    let worker = std::thread::spawn(move || restarting.restart());
    let checked = fs::read_to_string(fixture.wait("checked")).unwrap();
    fs::write(&fixture.launch.config_file, "tun: {enable: true}\n").unwrap();
    fs::write(fixture.launch.home_dir.join("allow-check"), "").unwrap();
    let result = worker.join().unwrap();
    if result.is_ok() {
        fs::write(fixture.launch.home_dir.join("read-2"), "").unwrap();
        let observed = fs::read_to_string(fixture.wait("observed-2")).unwrap();
        assert_eq!(observed, "tun: {enable: false}\nmode: direct\n");
        assert_ne!(process.snapshot().pid, previous);
    }
    process.stop().unwrap();
    assert_eq!(checked, "tun: {enable: false}\nmode: direct\n");
    assert!(
        result.is_ok(),
        "restart did not retain its prechecked candidate: {result:?}"
    );
}

#[test]
fn frozen_held_restart_does_not_require_the_original_source() {
    let fixture = Fixture::new("held-source");
    fs::write(fixture.launch.home_dir.join("read-1"), "").unwrap();
    fs::write(fixture.launch.home_dir.join("allow-check"), "").unwrap();
    let process = MihomoProcess::spawn(fixture.launch.clone()).unwrap();
    fixture.wait("observed-1");
    let payload = process.read_launch_payload().unwrap();
    fs::remove_file(&fixture.launch.config_file).unwrap();
    let lease = crate::data_coordinator::DataWriteLease::shared(process.write_scopes());
    let result = process.restart_payload_with_write_lease(&lease, None, Some(&payload));
    let observed = if result.is_ok() {
        fs::write(fixture.launch.home_dir.join("read-2"), "").unwrap();
        Some(fs::read_to_string(fixture.wait("observed-2")).unwrap())
    } else {
        None
    };
    process.stop().unwrap();
    assert!(
        result.is_ok(),
        "held restart reopened its source: {result:?}"
    );
    assert_eq!(observed.as_deref(), Some(payload.as_str()));
    assert_eq!(fixture.private_files(), 0);
}

#[test]
fn frozen_held_tun_on_is_rejected_without_stopping_the_current_child() {
    let fixture = Fixture::new("held-tun");
    fs::write(fixture.launch.home_dir.join("read-1"), "").unwrap();
    let process = MihomoProcess::spawn(fixture.launch.clone()).unwrap();
    fixture.wait("observed-1");
    let pid = process.snapshot().pid;
    let inputs = fixture.private_files();
    let lease = crate::data_coordinator::DataWriteLease::shared(process.write_scopes());
    let result =
        process.restart_payload_with_write_lease(&lease, None, Some("tun: {enable: true}\n"));
    let current = process.snapshot().pid;
    let remaining = fixture.private_files();
    process.stop().unwrap();
    assert!(matches!(result, Err(MihomoError::ServiceRequired)));
    assert_eq!(current, pid);
    assert_eq!(remaining, inputs);
    assert!(!fixture.launch.home_dir.join("checked").exists());
    assert_eq!(fixture.private_files(), 0);
}

#[test]
fn frozen_environment_cannot_replace_checked_startup_or_validation() {
    let executable = std::env::current_exe().unwrap();
    for name in [
        "frozen_spawn_reads_checked_bytes_after_original_changes_to_tun_on",
        "frozen_restart_reuses_prechecked_bytes_after_original_changes",
    ] {
        let test = format!("process::tests::frozen_launch_tests::{name}");
        let output = Command::new(&executable)
            .args(["--exact", &test, "--test-threads=1"])
            .env("CLASH_CONFIG_STRING", "dHVuOiB7ZW5hYmxlOiB0cnVlfQo=")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn frozen_cancelled_restart_keeps_pid_and_cleans_up_inputs() {
    let fixture = Fixture::new("cancel");
    fs::write(fixture.launch.home_dir.join("read-1"), "").unwrap();
    let process = MihomoProcess::spawn(fixture.launch.clone()).unwrap();
    fixture.wait("observed-1");
    let pid = process.snapshot().pid;
    let inputs = fixture.private_files();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let restarting = process.clone();
    let worker = std::thread::spawn(move || {
        let lease = crate::data_coordinator::DataWriteLease::shared(restarting.write_scopes());
        restarting.restart_with_write_lease(&lease, Some(&flag))
    });
    fixture.wait("checked");
    cancelled.store(true, Ordering::Release);
    fs::write(fixture.launch.home_dir.join("allow-check"), "").unwrap();
    assert!(worker.join().unwrap().is_err());
    assert_eq!(process.snapshot().pid, pid);
    assert_eq!(fixture.private_files(), inputs);
    process.stop().unwrap();
    assert_eq!(fixture.private_files(), 0);
}

#[test]
fn frozen_spawn_failure_removes_the_input_file() {
    let mut fixture = Fixture::new("spawn-failure");
    fixture.launch.binary = fixture.root.join("missing-executable");
    assert!(MihomoProcess::spawn(fixture.launch.clone()).is_err());
    assert_eq!(fixture.private_files(), 0);
    assert_eq!(
        fs::read_to_string(&fixture.launch.config_file).unwrap(),
        "tun: {enable: false}\nmode: direct\n"
    );
}

#[test]
fn frozen_large_input_does_not_wait_for_the_child_to_read_stdin() {
    let fixture = Fixture::new("large-input");
    let payload = format!(
        "tun: {{enable: false}}\n{}",
        "# held comment\n".repeat(32 * 1024)
    );
    fs::write(&fixture.launch.config_file, &payload).unwrap();
    let launch = fixture.launch.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || sender.send(MihomoProcess::spawn(launch)).unwrap());
    let early = receiver.recv_timeout(Duration::from_secs(2));
    let ready_before_read = early.is_ok();
    fs::write(fixture.launch.home_dir.join("read-1"), "").unwrap();
    let process = early
        .unwrap_or_else(|_| receiver.recv_timeout(Duration::from_secs(5)).unwrap())
        .unwrap();
    worker.join().unwrap();
    process.stop().unwrap();
    assert!(
        ready_before_read,
        "startup waited for the child to consume its input"
    );
    assert_eq!(fixture.private_files(), 0);
}
