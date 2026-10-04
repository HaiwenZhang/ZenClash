//! Fixed on-disk systemd template gate; this does not identify a loaded unit.

use std::{
    fs,
    io::{self, Read},
    path::Path,
};

pub(super) const UNIT: &str =
    include_str!("../../../../../platforms/linux/zenclash-service.service");

#[derive(Clone, Copy)]
pub(super) enum MaintenanceAction {
    Register,
    Start,
    Stop,
    Unregister,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MaintenanceEffect {
    ObserveAbsence,
    WriteRegistration,
    Reload,
    Enable,
    Start,
    Stop,
    Disable,
    RemoveRegistration,
}

const MAX_UNIT_BYTES: u64 = 16 * 1024;

pub(super) fn require_absent_service(
    status: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> io::Result<()> {
    let unknown = || io::Error::other("systemd service absence could not be established");
    if status != Some(0) || !stderr.is_empty() || stdout.len() > 64 * 1024 {
        return Err(unknown());
    }
    let mut load = None;
    let mut active = None;
    let mut pid = None;
    for line in stdout.lines() {
        let (name, value) = line.split_once('=').ok_or_else(unknown)?;
        let slot = match name {
            "LoadState" => &mut load,
            "ActiveState" => &mut active,
            "MainPID" => &mut pid,
            _ => return Err(unknown()),
        };
        if slot.replace(value).is_some() {
            return Err(unknown());
        }
    }
    if load != Some("not-found") || active != Some("inactive") || pid != Some("0") {
        return Err(unknown());
    }
    Ok(())
}

pub(super) fn require_running_pid(
    status: Option<i32>,
    stdout: &str,
    stderr: &str,
    expected: u32,
) -> io::Result<()> {
    let pid = stdout
        .strip_prefix("MainPID=")
        .and_then(|text| text.strip_suffix('\n'))
        .filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|text| text.parse::<u32>().ok());
    if status != Some(0) || !stderr.is_empty() || expected == 0 || pid != Some(expected) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "loaded service host identity is unconfirmed",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn observe_running_pid(expected: u32) -> io::Result<()> {
    let mut command = tokio::process::Command::new("/usr/bin/systemctl");
    command.env_clear().env("LC_ALL", "C").args([
        "show",
        "--system",
        "--no-pager",
        "--all",
        "--property=MainPID",
        "zenclash-service.service",
    ]);
    let (code, stdout, stderr) = capture_absence(command, std::time::Duration::from_secs(30))?;
    require_running_pid(code, &stdout, &stderr, expected)
}

#[cfg(target_os = "linux")]
pub(super) fn observe_absence() -> io::Result<()> {
    let mut command = tokio::process::Command::new("/usr/bin/systemctl");
    command.env_clear().env("LC_ALL", "C").args([
        "show",
        "--system",
        "--no-pager",
        "--all",
        "--property=LoadState",
        "--property=ActiveState",
        "--property=MainPID",
        "zenclash-service.service",
    ]);
    let (code, stdout, stderr) = capture_absence(command, std::time::Duration::from_secs(30))?;
    require_absent_service(code, &stdout, &stderr)
}

#[cfg(all(unix, any(target_os = "linux", test)))]
fn capture_absence(
    mut command: tokio::process::Command,
    limit: std::time::Duration,
) -> io::Result<(Option<i32>, String, String)> {
    use std::{
        process::Stdio,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tokio::io::AsyncReadExt;

    async fn read_pipe(
        mut reader: impl tokio::io::AsyncRead + Unpin,
        budget: Arc<AtomicUsize>,
    ) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut chunk = [0; 4096];
        loop {
            let read = reader.read(&mut chunk).await?;
            if read == 0 {
                return Ok(bytes);
            }
            if budget.fetch_add(read, Ordering::Relaxed) + read > 64 * 1024 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "systemd probe output exceeded its limit",
                ));
            }
            bytes.extend_from_slice(&chunk[..read]);
        }
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
            .ok_or_else(|| io::Error::other("systemd probe stdout unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("systemd probe stderr unavailable"))?;
        let budget = Arc::new(AtomicUsize::new(0));
        let result = tokio::time::timeout(limit, async {
            let (stdout, stderr, status) = tokio::try_join!(
                read_pipe(stdout, Arc::clone(&budget)),
                read_pipe(stderr, budget),
                child.wait()
            )?;
            let utf8 = |bytes| {
                String::from_utf8(bytes).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "systemd probe output was not UTF-8",
                    )
                })
            };
            Ok::<_, io::Error>((status.code(), utf8(stdout)?, utf8(stderr)?))
        })
        .await;
        let error = match result {
            Ok(Ok(output)) => return Ok(output),
            Ok(Err(error)) => error,
            Err(_) => io::Error::new(io::ErrorKind::TimedOut, "systemd service probe timed out"),
        };
        child.start_kill()?;
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "systemd probe termination timed out",
                )
            })??;
        Err(error)
    })
}

pub(super) fn validate_registration(
    app: &Path,
    package: &Path,
    validate: &impl Fn(&Path, bool) -> io::Result<()>,
) -> io::Result<(bool, bool)> {
    Ok((
        validate_file(app, false, validate)?,
        validate_file(package, false, validate)?,
    ))
}

fn validate_file(
    path: &Path,
    required: bool,
    validate: &impl Fn(&Path, bool) -> io::Result<()>,
) -> io::Result<bool> {
    let before = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if required {
                return Err(error);
            }
            validate(path.parent().ok_or_else(unknown_registration)?, true)?;
            return match fs::symlink_metadata(path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error),
                Ok(_) => Err(unknown_registration()),
            };
        }
        Err(error) => return Err(error),
    };
    if !before.is_file() || before.file_type().is_symlink() {
        return Err(unknown_registration());
    }
    validate(path, false)?;
    // The existing Unix opener uses O_NOFOLLOW | O_NONBLOCK and rejects non-files:
    // a replacement FIFO cannot make privileged maintenance wait for a writer.
    let file = super::open_pinned_file(path)?;
    let opened = file.metadata()?;
    if opened.len() > MAX_UNIT_BYTES || !same_file(&before, &opened) {
        return Err(unknown_registration());
    }
    let mut bytes = Vec::new();
    (&file).take(MAX_UNIT_BYTES + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let leaf = fs::symlink_metadata(path)?;
    validate(path, false)?;
    if bytes.len() as u64 > MAX_UNIT_BYTES
        || !same_file(&opened, &after)
        || !same_file(&after, &leaf)
        || bytes.len() as u64 != after.len()
    {
        return Err(unknown_registration());
    }
    let lf = UNIT.replace("\r\n", "\n");
    if bytes != lf.as_bytes() && bytes != lf.replace('\n', "\r\n").as_bytes() {
        return Err(unknown_registration());
    }
    Ok(true)
}

fn unknown_registration() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "fixed systemd registration is not an approved template",
    )
}

fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        left.dev() == right.dev()
            && left.ino() == right.ino()
            && left.uid() == right.uid()
            && left.gid() == right.gid()
            && left.mode() == right.mode()
            && left.len() == right.len()
            && left.mtime() == right.mtime()
            && left.mtime_nsec() == right.mtime_nsec()
            && left.ctime() == right.ctime()
            && left.ctime_nsec() == right.ctime_nsec()
    }
    #[cfg(windows)]
    {
        // Portable tests use the existing no-reparse, no-write/delete-share handle.
        use std::os::windows::fs::MetadataExt;
        left.file_size() == right.file_size()
            && left.file_attributes() == right.file_attributes()
            && left.creation_time() == right.creation_time()
            && left.last_write_time() == right.last_write_time()
    }
}

pub(super) fn maintain_registration(
    app: &Path,
    package: &Path,
    validate: &impl Fn(&Path, bool) -> io::Result<()>,
    action: MaintenanceAction,
    mut effect: impl FnMut(MaintenanceEffect) -> io::Result<()>,
) -> io::Result<()> {
    let (app_present, package_present) = validate_registration(app, package, validate)?;
    let mut app_required = app_present;
    let mut checked = |operation| {
        validate_file(app, app_required, validate)?;
        validate_file(package, package_present, validate)?;
        effect(operation)?;
        match operation {
            MaintenanceEffect::WriteRegistration => app_required = true,
            MaintenanceEffect::RemoveRegistration => app_required = false,
            _ => (),
        }
        Ok(())
    };
    match action {
        MaintenanceAction::Register => {
            checked(MaintenanceEffect::WriteRegistration)?;
            checked(MaintenanceEffect::Reload)?;
            checked(MaintenanceEffect::Enable)
        }
        MaintenanceAction::Start => checked(MaintenanceEffect::Start),
        MaintenanceAction::Stop if app_present || package_present => {
            checked(MaintenanceEffect::Stop)
        }
        MaintenanceAction::Unregister => {
            if app_present || package_present {
                checked(MaintenanceEffect::Stop)?;
                checked(MaintenanceEffect::Disable)?;
            } else {
                checked(MaintenanceEffect::ObserveAbsence)?;
            }
            if app_present {
                checked(MaintenanceEffect::RemoveRegistration)?;
                checked(MaintenanceEffect::Reload)?;
            }
            Ok(())
        }
        MaintenanceAction::Stop => checked(MaintenanceEffect::ObserveAbsence),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let mut nonce = [0; 8];
            getrandom::fill(&mut nonce).unwrap();
            let path = std::env::temp_dir().join(format!(
                "zenclash-unit-{}-{:x}",
                std::process::id(),
                u64::from_ne_bytes(nonce)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn app(&self) -> PathBuf {
            self.0.join("app.service")
        }
        fn package(&self) -> PathBuf {
            self.0.join("package.service")
        }
        fn maintain(
            &self,
            action: MaintenanceAction,
            effect: impl FnMut(MaintenanceEffect) -> io::Result<()>,
        ) -> io::Result<()> {
            maintain_registration(&self.app(), &self.package(), &|_, _| Ok(()), action, effect)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn foreign_units_prevent_all_maintenance_effects() {
        for action in [
            MaintenanceAction::Register,
            MaintenanceAction::Start,
            MaintenanceAction::Stop,
            MaintenanceAction::Unregister,
        ] {
            for package in [false, true] {
                let fixture = Fixture::new();
                let path = if package {
                    fixture.package()
                } else {
                    fixture.app()
                };
                fs::write(&path, b"foreign unit").unwrap();
                let mut calls = Vec::new();
                let result = fixture.maintain(action, |operation| {
                    calls.push(operation);
                    Ok(())
                });
                assert!(result.is_err(), "foreign unit reached dispatch");
                assert!(calls.is_empty());
                assert_eq!(fs::read(path).unwrap(), b"foreign unit");
            }
        }
    }

    #[test]
    fn complete_templates_allow_start_with_lf_or_crlf() {
        let lf = UNIT.replace("\r\n", "\n");
        for content in [lf.clone(), lf.replace('\n', "\r\n")] {
            let fixture = Fixture::new();
            fs::write(fixture.app(), content).unwrap();
            let mut calls = Vec::new();
            fixture
                .maintain(MaintenanceAction::Start, |operation| {
                    calls.push(operation);
                    Ok(())
                })
                .unwrap();
            assert_eq!(calls, [MaintenanceEffect::Start]);
        }
    }

    #[test]
    fn package_registration_is_never_removed_by_application_uninstall() {
        let fixture = Fixture::new();
        fs::write(fixture.package(), UNIT).unwrap();
        let mut calls = Vec::new();
        fixture
            .maintain(MaintenanceAction::Unregister, |operation| {
                calls.push(operation);
                Ok(())
            })
            .unwrap();
        assert_eq!(calls, [MaintenanceEffect::Stop, MaintenanceEffect::Disable]);
        assert_eq!(fs::read(fixture.package()).unwrap(), UNIT.as_bytes());
    }

    #[test]
    fn registration_changed_after_stop_blocks_disable_and_unlink() {
        let fixture = Fixture::new();
        fs::write(fixture.app(), UNIT).unwrap();
        let mut calls = Vec::new();
        let result = fixture.maintain(MaintenanceAction::Unregister, |operation| {
            calls.push(operation);
            fs::write(fixture.app(), b"foreign replacement")
        });
        assert!(result.is_err());
        assert_eq!(calls, [MaintenanceEffect::Stop]);
        assert_eq!(fs::read(fixture.app()).unwrap(), b"foreign replacement");
    }

    #[test]
    fn directories_and_oversized_units_do_not_reach_dispatch() {
        for directory in [false, true] {
            let fixture = Fixture::new();
            if directory {
                fs::create_dir(fixture.app()).unwrap();
            } else {
                fs::write(fixture.app(), vec![b'x'; 16 * 1024 + 1]).unwrap();
            }
            let mut calls = Vec::new();
            assert!(
                fixture
                    .maintain(MaintenanceAction::Start, |operation| {
                        calls.push(operation);
                        Ok(())
                    })
                    .is_err()
            );
            assert!(calls.is_empty());
        }
    }

    #[test]
    fn protection_failure_is_not_treated_as_missing_registration() {
        let fixture = Fixture::new();
        let mut calls = Vec::new();
        let result = maintain_registration(
            &fixture.app(),
            &fixture.package(),
            &|_, _| Err(io::ErrorKind::PermissionDenied.into()),
            MaintenanceAction::Register,
            |operation| {
                calls.push(operation);
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(calls.is_empty());
    }

    #[test]
    fn missing_registration_retains_install_dispatch() {
        let fixture = Fixture::new();
        let mut calls = Vec::new();
        fixture
            .maintain(MaintenanceAction::Register, |operation| {
                calls.push(operation);
                if operation == MaintenanceEffect::WriteRegistration {
                    fs::write(fixture.app(), UNIT)?;
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(
            calls,
            [
                MaintenanceEffect::WriteRegistration,
                MaintenanceEffect::Reload,
                MaintenanceEffect::Enable
            ]
        );
    }

    #[test]
    fn disappearing_registration_after_stop_blocks_disable() {
        let fixture = Fixture::new();
        fs::write(fixture.app(), UNIT).unwrap();
        let mut calls = Vec::new();
        let result = fixture.maintain(MaintenanceAction::Unregister, |operation| {
            calls.push(operation);
            fs::remove_file(fixture.app())
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(calls, [MaintenanceEffect::Stop]);
    }

    #[test]
    fn missing_disk_registration_cannot_release_deployment_when_presence_is_unknown() {
        for action in [MaintenanceAction::Stop, MaintenanceAction::Unregister] {
            let fixture = Fixture::new();
            let deployment = fixture.0.join("helper");
            fs::write(&deployment, b"previous helper").unwrap();
            let result = fixture.maintain(action, |_| Err(io::Error::other("loaded unit remains")));
            if result.is_ok() {
                fs::remove_file(&deployment).unwrap();
            }
            assert!(result.is_err());
            assert_eq!(fs::read(deployment).unwrap(), b"previous helper");
        }
    }

    #[test]
    fn missing_inactive_zero_pid_service_permits_deployment() {
        let fixture = Fixture::new();
        let mut calls = Vec::new();
        fixture
            .maintain(MaintenanceAction::Stop, |operation| {
                calls.push(operation);
                require_absent_service(
                    Some(0),
                    "LoadState=not-found\nActiveState=inactive\nMainPID=0\n",
                    "",
                )
            })
            .unwrap();
        assert_eq!(calls, [MaintenanceEffect::ObserveAbsence]);
    }

    #[test]
    fn loaded_active_or_malformed_properties_never_prove_absence() {
        for output in [
            "LoadState=loaded\nActiveState=inactive\nMainPID=0\n",
            "LoadState=not-found\nActiveState=active\nMainPID=0\n",
            "LoadState=not-found\nActiveState=inactive\nMainPID=23\n",
            "LoadState=not-found\nActiveState=inactive\nMainPID=0\nMainPID=0\n",
            "LoadState=not-found\nActiveState=inactive\nMainPID=0\nUnknown=value\n",
            "LoadState=not-found\nMainPID=0\n",
            "LoadState=not-found\nActiveState=inactive\nMainPID=00\n",
        ] {
            assert!(
                require_absent_service(Some(0), output, "").is_err(),
                "{output:?}"
            );
        }
    }

    #[test]
    fn failed_or_diagnostic_query_does_not_prove_absence() {
        let output = "LoadState=not-found\nActiveState=inactive\nMainPID=0\n";
        assert!(require_absent_service(Some(1), output, "").is_err());
        assert!(require_absent_service(None, output, "").is_err());
        assert!(require_absent_service(Some(0), output, "warning").is_err());
    }

    #[cfg(unix)]
    fn shell_probe(
        script: &str,
        limit: std::time::Duration,
    ) -> io::Result<(Option<i32>, String, String)> {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.env_clear().args(["-c", script]);
        capture_absence(command, limit)
    }

    #[cfg(unix)]
    #[test]
    fn native_probe_captures_complete_properties_without_service_changes() {
        let (code, stdout, stderr) = shell_probe(
            "printf 'MainPID=0\\nLoadState=not-found\\nActiveState=inactive\\n'",
            std::time::Duration::from_secs(2),
        )
        .unwrap();
        require_absent_service(code, &stdout, &stderr).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_probe_enforces_combined_stdout_and_stderr_budget() {
        let result = shell_probe(
            "printf '%040000d' 0; printf '%040000d' 0 >&2",
            std::time::Duration::from_secs(2),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[cfg(unix)]
    #[test]
    fn native_probe_rejects_invalid_utf8() {
        let result = shell_probe("printf '\\377'", std::time::Duration::from_secs(2));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[cfg(unix)]
    #[test]
    fn native_probe_timeout_kills_and_reaps_its_owned_child() {
        let fixture = Fixture::new();
        let pid_file = fixture.0.join("probe.pid");
        // The fixture path consists only of the fixed temp prefix and numeric nonce.
        let script = format!(
            "printf '%s' $$ > '{}'; exec /bin/sleep 10",
            pid_file.display()
        );
        let began = std::time::Instant::now();
        let result = shell_probe(&script, std::time::Duration::from_millis(100));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(began.elapsed() < std::time::Duration::from_secs(2));
        let pid: libc::pid_t = fs::read_to_string(pid_file).unwrap().parse().unwrap();
        // SAFETY: kill with signal zero only checks this fixture's recorded PID.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }

    #[test]
    fn failed_stop_preserves_registration_and_skips_disable() {
        let fixture = Fixture::new();
        fs::write(fixture.app(), UNIT).unwrap();
        let mut calls = Vec::new();
        let result = fixture.maintain(MaintenanceAction::Unregister, |operation| {
            calls.push(operation);
            Err(io::Error::other("fixture stop failed"))
        });
        assert!(result.is_err());
        assert_eq!(calls, [MaintenanceEffect::Stop]);
        assert_eq!(fs::read(fixture.app()).unwrap(), UNIT.as_bytes());
    }

    #[test]
    fn approved_app_uninstall_removes_only_override_and_reloads() {
        let fixture = Fixture::new();
        fs::write(fixture.app(), UNIT).unwrap();
        fs::write(fixture.package(), UNIT).unwrap();
        let mut calls = Vec::new();
        fixture
            .maintain(MaintenanceAction::Unregister, |operation| {
                calls.push(operation);
                if operation == MaintenanceEffect::RemoveRegistration {
                    fs::remove_file(fixture.app())?;
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(
            calls,
            [
                MaintenanceEffect::Stop,
                MaintenanceEffect::Disable,
                MaintenanceEffect::RemoveRegistration,
                MaintenanceEffect::Reload
            ]
        );
        assert!(!fixture.app().try_exists().unwrap());
        assert_eq!(fs::read(fixture.package()).unwrap(), UNIT.as_bytes());
    }

    #[test]
    fn leaf_disappearing_during_open_is_not_missing_registration() {
        let fixture = Fixture::new();
        fs::write(fixture.app(), UNIT).unwrap();
        let result = validate_registration(&fixture.app(), &fixture.package(), &|path, _| {
            fs::remove_file(path)
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_unit_is_not_missing_for_ordinary_user() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid has no arguments or pointer preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let fixture = Fixture::new();
        fs::write(fixture.app(), UNIT).unwrap();
        fs::set_permissions(fixture.app(), fs::Permissions::from_mode(0o0)).unwrap();
        let mut calls = Vec::new();
        let result = fixture.maintain(MaintenanceAction::Start, |operation| {
            calls.push(operation);
            Ok(())
        });
        fs::set_permissions(fixture.app(), fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(calls.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_does_not_authorize_install() {
        let fixture = Fixture::new();
        std::os::unix::fs::symlink(fixture.0.join("missing-target"), fixture.app()).unwrap();
        let mut calls = Vec::new();
        assert!(
            fixture
                .maintain(MaintenanceAction::Register, |operation| {
                    calls.push(operation);
                    Ok(())
                })
                .is_err()
        );
        assert!(calls.is_empty());
        assert!(
            fs::symlink_metadata(fixture.app())
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    #[test]
    fn maintenance_running_pid_must_match_exactly_before_stop() {
        require_running_pid(Some(0), "MainPID=123\n", "", 123).unwrap();
        for output in [
            "MainPID=0\n",
            "MainPID=124\n",
            " MainPID=123\n",
            "MainPID=123\nMainPID=123\n",
            "MainPID=123",
            "MainPID=123\nignored=true\n",
            "MainPID=+123\n",
        ] {
            assert!(
                require_running_pid(Some(0), output, "", 123).is_err(),
                "unexpected acceptance: {output:?}"
            );
        }
        assert!(require_running_pid(Some(1), "MainPID=123\n", "", 123).is_err());
        assert!(require_running_pid(Some(0), "MainPID=123\n", "warning", 123).is_err());
        assert!(require_running_pid(Some(0), "MainPID=0\n", "", 0).is_err());
    }
}
