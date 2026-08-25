use std::{collections::VecDeque, path::PathBuf, process::Command, sync::Arc};

use parking_lot::{Mutex, RwLock};

use super::*;

#[test]
fn readiness_attempt_timeout_never_exceeds_remaining_deadline() {
    assert_eq!(
        readiness_attempt_timeout(Duration::from_millis(25)),
        Duration::from_millis(25)
    );
}

#[test]
fn readiness_attempt_timeout_caps_long_controller_attempts() {
    assert_eq!(
        readiness_attempt_timeout(Duration::from_secs(5)),
        Duration::from_millis(750)
    );
}

#[cfg(unix)]
#[test]
fn exited_process_snapshot_does_not_expose_a_stale_pid() {
    let mut child = Command::new("/usr/bin/true").spawn().unwrap();
    child.wait().unwrap();
    let process = MihomoProcess {
        child: Mutex::new(Some(child)),
        logs: Arc::new(RwLock::new(VecDeque::new())),
        config: MihomoLaunchConfig {
            binary: PathBuf::from("/usr/bin/true"),
            config_file: PathBuf::from("profile.yaml"),
            home_dir: PathBuf::new(),
            endpoint: MihomoEndpoint::default(),
            controller_override: None,
        },
    };

    let snapshot = process.snapshot();

    assert_eq!((snapshot.running, snapshot.pid), (false, None));
}
