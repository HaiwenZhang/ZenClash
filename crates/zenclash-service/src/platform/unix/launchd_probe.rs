//! launchd probe classification and fail-closed maintenance dispatch.

use std::io;
#[cfg(all(unix, any(target_os = "macos", test)))]
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LaunchdServiceState {
    Loaded,
    Absent,
}

pub(super) struct ProbeOutput {
    pub code: Option<i32>,
    pub diagnostic: String,
}

#[derive(Clone, Copy)]
pub(super) enum MaintenanceAction {
    Start,
    Stop,
    Unregister,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MaintenanceEffect {
    Kickstart,
    Enable,
    Bootstrap,
    Bootout,
    RemoveRegistration,
}

pub(super) fn classify_launchd_service_probe(
    code: Option<i32>,
    diagnostic: &str,
) -> io::Result<LaunchdServiceState> {
    match code {
        Some(0) => Ok(LaunchdServiceState::Loaded),
        Some(113) if diagnostic.contains("Could not find service") => {
            Ok(LaunchdServiceState::Absent)
        }
        _ => Err(io::Error::other(
            "launchd service presence could not be established",
        )),
    }
}

#[cfg(target_os = "macos")]
pub(super) fn probe_service(label: &str) -> io::Result<ProbeOutput> {
    let mut command = tokio::process::Command::new("/bin/launchctl");
    command.args(["print", label]);
    capture_probe(command, Duration::from_secs(30))
}

// A local runtime owns both nonblocking pipe readers until completion. Dropping the
// timed-out read future closes them directly; no reader thread can outlive the probe
// or wait indefinitely for a descendant that inherited stdout/stderr.
#[cfg(all(unix, any(target_os = "macos", test)))]
fn capture_probe(command: tokio::process::Command, limit: Duration) -> io::Result<ProbeOutput> {
    let output = capture_probe_output(command, limit)?;
    Ok(ProbeOutput {
        code: output.code,
        diagnostic: format!("{}\n{}", output.stdout, output.stderr),
    })
}

#[cfg(all(unix, any(target_os = "macos", test)))]
struct NativeProbeOutput {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

#[cfg(all(unix, any(target_os = "macos", test)))]
pub(super) fn capture_host_pid(
    command: tokio::process::Command,
    limit: Duration,
) -> io::Result<u32> {
    let output = capture_probe_output(command, limit)?;
    super::macos_job::parse_host_pid(output.code, &output.stdout, &output.stderr)
}

#[cfg(all(unix, any(target_os = "macos", test)))]
fn capture_probe_output(
    mut command: tokio::process::Command,
    limit: Duration,
) -> io::Result<NativeProbeOutput> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;

    const PIPE_LIMIT: usize = 64 * 1024;
    async fn read_pipe(reader: impl tokio::io::AsyncRead + Unpin) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        reader
            .take((PIPE_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > PIPE_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "launchd probe output exceeded its limit",
            ));
        }
        Ok(bytes)
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("launchd probe stdout unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("launchd probe stderr unavailable"))?;
        let result = tokio::time::timeout(limit, async {
            let (stdout, stderr, status) =
                tokio::try_join!(read_pipe(stdout), read_pipe(stderr), child.wait())?;
            // Decode each complete stream strictly, without permitting invalid bytes
            // or a diagnostic split across stdout/stderr to manufacture absence.
            let stdout = String::from_utf8(stdout).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "launchd probe output was not UTF-8",
                )
            })?;
            let stderr = String::from_utf8(stderr).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "launchd probe output was not UTF-8",
                )
            })?;
            Ok::<_, io::Error>(NativeProbeOutput {
                code: status.code(),
                stdout,
                stderr,
            })
        })
        .await;
        let error = match result {
            Ok(Ok(output)) => return Ok(output),
            Ok(Err(error)) => error,
            Err(_) => io::Error::new(io::ErrorKind::TimedOut, "launchd service probe timed out"),
        };
        // Reap only the owned child. Cancellation of the pipe futures above
        // has already closed their handles, even if descendants retain copies.
        child.start_kill()?;
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "launchd probe termination timed out",
                )
            })??;
        Err(error)
    })
}

pub(super) fn maintain(
    action: MaintenanceAction,
    probe: impl FnOnce() -> io::Result<ProbeOutput>,
    mut effect: impl FnMut(MaintenanceEffect) -> io::Result<()>,
) -> io::Result<()> {
    let output = probe()?;
    let state = classify_launchd_service_probe(output.code, &output.diagnostic)?;
    match action {
        MaintenanceAction::Start => match state {
            LaunchdServiceState::Loaded => effect(MaintenanceEffect::Kickstart),
            LaunchdServiceState::Absent => {
                effect(MaintenanceEffect::Enable)?;
                effect(MaintenanceEffect::Bootstrap)
            }
        },
        MaintenanceAction::Stop | MaintenanceAction::Unregister => {
            if state == LaunchdServiceState::Loaded {
                effect(MaintenanceEffect::Bootout)?;
            }
            if matches!(action, MaintenanceAction::Unregister) {
                effect(MaintenanceEffect::RemoveRegistration)?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_launchd_service_is_absent() {
        assert_eq!(
            classify_launchd_service_probe(Some(113), "Could not find service").unwrap(),
            LaunchdServiceState::Absent
        );
    }

    #[test]
    fn loaded_launchd_service_is_loaded() {
        assert_eq!(
            classify_launchd_service_probe(Some(0), "").unwrap(),
            LaunchdServiceState::Loaded
        );
    }

    #[test]
    fn unexpected_launchd_exit_is_an_error() {
        assert!(classify_launchd_service_probe(Some(5), "Could not find service").is_err());
    }

    #[test]
    fn unexpected_launchd_diagnostic_is_an_error() {
        assert!(classify_launchd_service_probe(Some(113), "Operation not permitted").is_err());
    }

    #[test]
    fn unknown_start_performs_no_platform_effect() {
        let mut effects = Vec::new();
        let result = maintain(
            MaintenanceAction::Start,
            || {
                Ok(ProbeOutput {
                    code: Some(5),
                    diagnostic: "Operation not permitted".into(),
                })
            },
            |effect| {
                effects.push(effect);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(effects.is_empty());
    }

    #[test]
    fn unknown_stop_performs_no_platform_effect() {
        let mut effects = Vec::new();
        let result = maintain(
            MaintenanceAction::Stop,
            || {
                Ok(ProbeOutput {
                    code: None,
                    diagnostic: String::new(),
                })
            },
            |effect| {
                effects.push(effect);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(effects.is_empty());
    }

    #[test]
    fn unknown_unregister_does_not_bootout_or_remove_registration() {
        let mut effects = Vec::new();
        let result = maintain(
            MaintenanceAction::Unregister,
            || {
                Ok(ProbeOutput {
                    code: Some(113),
                    diagnostic: "Operation not permitted".into(),
                })
            },
            |effect| {
                effects.push(effect);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(effects.is_empty());
    }

    #[test]
    fn explicit_absence_enables_then_bootstraps_without_kickstart() {
        let mut effects = Vec::new();
        maintain(
            MaintenanceAction::Start,
            || {
                Ok(ProbeOutput {
                    code: Some(113),
                    diagnostic: "Could not find service".into(),
                })
            },
            |effect| {
                effects.push(effect);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            effects,
            [MaintenanceEffect::Enable, MaintenanceEffect::Bootstrap]
        );
    }

    #[test]
    fn loaded_start_kickstarts_without_bootstrap() {
        let mut effects = Vec::new();
        maintain(
            MaintenanceAction::Start,
            || {
                Ok(ProbeOutput {
                    code: Some(0),
                    diagnostic: String::new(),
                })
            },
            |effect| {
                effects.push(effect);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(effects, [MaintenanceEffect::Kickstart]);
    }

    #[test]
    fn failed_bootout_preserves_registration() {
        let mut effects = Vec::new();
        let result = maintain(
            MaintenanceAction::Unregister,
            || {
                Ok(ProbeOutput {
                    code: Some(0),
                    diagnostic: String::new(),
                })
            },
            |effect| {
                effects.push(effect);
                Err(io::Error::other("fixture bootout failed"))
            },
        );
        assert!(result.is_err());
        assert_eq!(effects, [MaintenanceEffect::Bootout]);
    }

    #[cfg(unix)]
    fn fixture_command(kind: &str) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "platform::launchd_probe::tests::probe_child_fixture",
            "--nocapture",
        ]);
        command.env("ZENCLASH_LAUNCHD_PROBE_FIXTURE", kind);
        command
    }

    #[test]
    #[cfg(unix)]
    fn probe_child_fixture() {
        use std::io::Write;
        let Ok(kind) = std::env::var("ZENCLASH_LAUNCHD_PROBE_FIXTURE") else {
            return;
        };
        match kind.as_str() {
            "dual" => {
                let stderr = std::thread::spawn(|| {
                    std::io::stderr().write_all(&vec![b'e'; 48 * 1024]).unwrap();
                    eprintln!("Could not find service");
                });
                std::io::stdout().write_all(&vec![b'o'; 48 * 1024]).unwrap();
                stderr.join().unwrap();
                std::process::exit(113);
            }
            "excess" => {
                std::io::stdout()
                    .write_all(&vec![b'x'; 128 * 1024])
                    .unwrap();
            }
            "invalid" => {
                std::io::stdout().write_all(&[255]).unwrap();
            }
            "hang" => std::thread::sleep(Duration::from_secs(10)),
            "inherited" => {
                fixture_command("descendant").as_std_mut().spawn().unwrap();
                std::process::exit(113);
            }
            "descendant" => std::thread::sleep(Duration::from_millis(400)),
            _ => panic!("unexpected fixture kind"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn concurrent_bounded_pipes_preserve_explicit_absence() {
        let output = capture_probe(fixture_command("dual"), Duration::from_secs(5)).unwrap();
        assert_eq!(
            classify_launchd_service_probe(output.code, &output.diagnostic).unwrap(),
            LaunchdServiceState::Absent
        );
    }

    #[test]
    #[cfg(unix)]
    fn oversized_output_blocks_every_maintenance_effect() {
        let mut effects = Vec::new();
        let result = maintain(
            MaintenanceAction::Unregister,
            || capture_probe(fixture_command("excess"), Duration::from_secs(5)),
            |effect| {
                effects.push(effect);
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert!(effects.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn invalid_utf8_blocks_every_maintenance_effect() {
        let mut effects = Vec::new();
        let result = maintain(
            MaintenanceAction::Start,
            || capture_probe(fixture_command("invalid"), Duration::from_secs(5)),
            |effect| {
                effects.push(effect);
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert!(effects.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn timeout_blocks_every_maintenance_effect() {
        let mut effects = Vec::new();
        let result = maintain(
            MaintenanceAction::Stop,
            || capture_probe(fixture_command("hang"), Duration::from_millis(100)),
            |effect| {
                effects.push(effect);
                Ok(())
            },
        );
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::TimedOut);
        assert!(effects.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn inherited_pipes_do_not_extend_the_probe_deadline() {
        let started = std::time::Instant::now();
        let result = capture_probe(fixture_command("inherited"), Duration::from_millis(100));
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_millis(350));
        // The ordinary descendant fixture has its own finite lifetime; leave no
        // test child after completing this case, without manipulating system services.
        std::thread::sleep(Duration::from_millis(500));
    }
}
