//! Fixed on-disk launchd template gate; this does not identify a loaded job.

use std::{
    fs,
    io::{self, Read},
    path::Path,
};

use super::launchd_probe::{self, MaintenanceAction, MaintenanceEffect, ProbeOutput};

pub(super) const PLIST: &str =
    include_str!("../../../../../platforms/macos/org.zenclash.service.plist");
const MAX_PLIST_BYTES: u64 = 16 * 1024;

pub(super) fn validate_registration(
    path: &Path,
    validate: &impl Fn(&Path, bool) -> io::Result<()>,
) -> io::Result<()> {
    let before = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            validate(path.parent().ok_or_else(unknown_registration)?, true)?;
            return match fs::symlink_metadata(path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
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
    if opened.len() > MAX_PLIST_BYTES || !same_file(&before, &opened) {
        return Err(unknown_registration());
    }
    let mut bytes = Vec::new();
    (&file).take(MAX_PLIST_BYTES + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let leaf = fs::symlink_metadata(path)?;
    validate(path, false)?;
    if bytes.len() as u64 > MAX_PLIST_BYTES
        || !same_file(&opened, &after)
        || !same_file(&after, &leaf)
        || bytes.len() as u64 != after.len()
    {
        return Err(unknown_registration());
    }
    let lf = PLIST.replace("\r\n", "\n");
    if bytes != lf.as_bytes() && bytes != lf.replace('\n', "\r\n").as_bytes() {
        return Err(unknown_registration());
    }
    Ok(())
}

fn unknown_registration() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "fixed launchd registration is not an approved template",
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

pub(super) fn with_known_registration(
    path: &Path,
    validate: &impl Fn(&Path, bool) -> io::Result<()>,
    effect: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    validate_registration(path, validate)?;
    effect()
}

pub(super) fn maintain_registration(
    path: &Path,
    validate: &impl Fn(&Path, bool) -> io::Result<()>,
    action: MaintenanceAction,
    probe: impl FnOnce() -> io::Result<ProbeOutput>,
    mut effect: impl FnMut(MaintenanceEffect) -> io::Result<()>,
) -> io::Result<()> {
    with_known_registration(path, validate, || {
        launchd_probe::maintain(action, probe, |operation| {
            with_known_registration(path, validate, || effect(operation))
        })
    })
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, path::PathBuf};

    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let mut nonce = [0; 8];
            getrandom::fill(&mut nonce).unwrap();
            let directory = std::env::temp_dir().join(format!(
                "zenclash-plist-{}-{:x}",
                std::process::id(),
                u64::from_ne_bytes(nonce)
            ));
            fs::create_dir(&directory).unwrap();
            Self(directory)
        }

        fn path(&self) -> PathBuf {
            self.0.join("org.zenclash.service.plist")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    // Ordinary user-owned fixtures exercise content dispatch, not root ownership.
    fn fixture_protection(_: &Path, _: bool) -> io::Result<()> {
        Ok(())
    }

    fn loaded() -> io::Result<ProbeOutput> {
        Ok(ProbeOutput {
            code: Some(0),
            diagnostic: String::new(),
        })
    }

    #[test]
    fn foreign_registration_is_not_replaced() {
        let fixture = Fixture::new();
        let path = fixture.path();
        let foreign = b"foreign registration";
        fs::write(&path, foreign).unwrap();
        let calls = Cell::new(0);
        let result = with_known_registration(&path, &fixture_protection, || {
            calls.set(calls.get() + 1);
            fs::write(&path, PLIST)
        });
        assert!(result.is_err(), "foreign template was accepted");
        assert_eq!(calls.get(), 0);
        assert_eq!(fs::read(path).unwrap(), foreign);
    }

    #[test]
    fn foreign_registration_prevents_start_stop_and_unlink() {
        for action in [
            MaintenanceAction::Start,
            MaintenanceAction::Stop,
            MaintenanceAction::Unregister,
        ] {
            let fixture = Fixture::new();
            let path = fixture.path();
            let foreign = b"foreign registration";
            fs::write(&path, foreign).unwrap();
            let mut effects = Vec::new();
            let result =
                maintain_registration(&path, &fixture_protection, action, loaded, |effect| {
                    effects.push(effect);
                    if effect == MaintenanceEffect::RemoveRegistration {
                        fs::remove_file(&path)?;
                    }
                    Ok(())
                });
            assert!(
                result.is_err(),
                "foreign registration reached native dispatch"
            );
            assert!(effects.is_empty());
            assert_eq!(fs::read(path).unwrap(), foreign);
        }
    }

    #[test]
    fn early_preflight_preserves_pending_journal_and_approved_artifacts() {
        let fixture = Fixture::new();
        let path = fixture.path();
        fs::write(&path, b"customized plist").unwrap();
        let journal = fixture.0.join("maintenance.json");
        let helper = fixture.0.join("zenclash-service");
        fs::write(&journal, b"pending recovery evidence").unwrap();
        fs::write(&helper, b"previous approved helper").unwrap();
        let result = with_known_registration(&path, &fixture_protection, || {
            fs::remove_file(&journal)?;
            fs::write(&helper, b"new helper")
        });
        assert!(result.is_err());
        assert_eq!(fs::read(journal).unwrap(), b"pending recovery evidence");
        assert_eq!(fs::read(helper).unwrap(), b"previous approved helper");
    }

    #[test]
    fn registration_changed_during_probe_prevents_every_native_effect() {
        let fixture = Fixture::new();
        let path = fixture.path();
        fs::write(&path, PLIST).unwrap();
        let calls = Cell::new(0);
        let result = maintain_registration(
            &path,
            &fixture_protection,
            MaintenanceAction::Unregister,
            || {
                fs::write(&path, b"foreign replacement")?;
                loaded()
            },
            |_| {
                calls.set(calls.get() + 1);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(calls.get(), 0);
        assert_eq!(fs::read(path).unwrap(), b"foreign replacement");
    }

    #[test]
    fn customized_whitespace_payload_and_oversized_files_are_preserved() {
        let lf = PLIST.replace("\r\n", "\n");
        for bytes in [
            format!("{lf}extra").into_bytes(),
            lf.replacen('\n', "\r\n", 1).into_bytes(),
            vec![b'x'; 16 * 1024 + 1],
        ] {
            let fixture = Fixture::new();
            let path = fixture.path();
            fs::write(&path, &bytes).unwrap();
            let calls = Cell::new(0);
            let result = with_known_registration(&path, &fixture_protection, || {
                calls.set(1);
                Ok(())
            });
            assert!(result.is_err());
            assert_eq!(calls.get(), 0);
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }

    #[test]
    fn complete_lf_and_crlf_templates_allow_existing_dispatch() {
        let lf = PLIST.replace("\r\n", "\n");
        for bytes in [lf.as_bytes(), lf.replace('\n', "\r\n").as_bytes()] {
            let fixture = Fixture::new();
            let path = fixture.path();
            fs::write(&path, bytes).unwrap();
            let mut effects = Vec::new();
            maintain_registration(
                &path,
                &fixture_protection,
                MaintenanceAction::Start,
                loaded,
                |effect| {
                    effects.push(effect);
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(effects, [MaintenanceEffect::Kickstart]);
        }
    }

    #[test]
    fn missing_registration_retains_bootstrap_behavior() {
        let fixture = Fixture::new();
        let mut effects = Vec::new();
        maintain_registration(
            &fixture.path(),
            &fixture_protection,
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
        assert_eq!(effects, [MaintenanceEffect::Bootstrap]);
    }

    #[test]
    fn protection_error_does_not_reach_effects() {
        let fixture = Fixture::new();
        fs::write(fixture.path(), PLIST).unwrap();
        let calls = Cell::new(0);
        let result = with_known_registration(
            &fixture.path(),
            &|_, _| Err(io::ErrorKind::PermissionDenied.into()),
            || {
                calls.set(1);
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn existing_file_disappearing_at_open_is_not_treated_as_absent() {
        let fixture = Fixture::new();
        let path = fixture.path();
        fs::write(&path, PLIST).unwrap();
        let calls = Cell::new(0);
        let result = with_known_registration(&path, &|path, _| fs::remove_file(path), || {
            calls.set(1);
            Ok(())
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn directory_is_not_a_missing_registration() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.path()).unwrap();
        let calls = Cell::new(0);
        let result = with_known_registration(&fixture.path(), &fixture_protection, || {
            calls.set(1);
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(calls.get(), 0);
        assert!(fixture.path().is_dir());
    }

    #[test]
    fn failed_bootout_preserves_approved_registration() {
        let fixture = Fixture::new();
        let path = fixture.path();
        fs::write(&path, PLIST).unwrap();
        let mut effects = Vec::new();
        let result = maintain_registration(
            &path,
            &fixture_protection,
            MaintenanceAction::Unregister,
            loaded,
            |effect| {
                effects.push(effect);
                if effect == MaintenanceEffect::Bootout {
                    return Err(io::Error::other("fixture bootout failure"));
                }
                fs::remove_file(&path)
            },
        );
        assert!(result.is_err());
        assert_eq!(effects, [MaintenanceEffect::Bootout]);
        assert_eq!(fs::read(path).unwrap(), PLIST.as_bytes());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_replacement_during_validation_is_rejected_without_effects() {
        let fixture = Fixture::new();
        let path = fixture.path();
        let target = fixture.0.join("foreign.plist");
        fs::write(&path, PLIST).unwrap();
        fs::write(&target, PLIST).unwrap();
        let calls = Cell::new(0);
        let result = with_known_registration(
            &path,
            &|path, _| {
                fs::remove_file(path)?;
                std::os::unix::fs::symlink(&target, path)
            },
            || {
                calls.set(1);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(calls.get(), 0);
        assert!(fs::symlink_metadata(path).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(target).unwrap(), PLIST.as_bytes());
    }

    #[cfg(unix)]
    #[test]
    fn fifo_replacement_during_validation_is_rejected_without_waiting() {
        use std::os::unix::ffi::OsStrExt;
        let fixture = Fixture::new();
        let path = fixture.path();
        fs::write(&path, PLIST).unwrap();
        let calls = Cell::new(0);
        let began = std::time::Instant::now();
        let result = with_known_registration(
            &path,
            &|path, _| {
                fs::remove_file(path)?;
                let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                // SAFETY: the new user-owned fixture path is a valid NUL-terminated string.
                if unsafe { libc::mkfifo(path.as_ptr(), 0o600) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            },
            || {
                calls.set(1);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(calls.get(), 0);
        assert!(began.elapsed() < std::time::Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_file_is_not_absent_for_an_ordinary_user() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let fixture = Fixture::new();
        let path = fixture.path();
        fs::write(&path, PLIST).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o0)).unwrap();
        let calls = Cell::new(0);
        let result = with_known_registration(&path, &fixture_protection, || {
            calls.set(1);
            Ok(())
        });
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(calls.get(), 0);
        assert_eq!(fs::read(path).unwrap(), PLIST.as_bytes());
    }
}
