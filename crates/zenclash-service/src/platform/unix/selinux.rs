//! SELinux labeling of approved executable files before service activation.
// Adapted from clash_verge_service_ipc 2.7.5, src/bin/installer/selinux.rs.
// Upstream package author: Tunglies; license: GPL-3.0. See crates/zenclash-service/UPSTREAM.md.

use std::io;

#[cfg(target_os = "linux")]
pub(super) fn ensure_executable_label(path: &std::path::Path) -> io::Result<()> {
    super::unix::validate_protected_path(path, false)?;
    label_if_enabled(std::fs::read_to_string("/sys/fs/selinux/enforce"), || {
        let file = path.to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid executable path")
        })?;
        run_chcon(|program| {
            let status = super::unix::run_native_status(
                program,
                &["--no-dereference", "--type=bin_t", "--", file],
            )?;
            if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!("chcon exited with {status}")))
            }
        })
    })
    .map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "SELinux executable labeling failed for {}: {error}",
                path.display()
            ),
        )
    })
}

fn label_if_enabled(
    mode: io::Result<String>,
    label: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    match mode {
        // Permissive needs labels too: switching to enforcing must preserve executability.
        Ok(mode) if matches!(mode.trim(), "0" | "1") => label(),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected SELinux enforcement mode",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn run_chcon(mut run: impl FnMut(&str) -> io::Result<()>) -> io::Result<()> {
    // Never resolve privileged commands through the caller's PATH.
    run("/usr/bin/chcon").or_else(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            run("/bin/chcon")
        } else {
            Err(error)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforcing_runs_label_and_propagates_failure() {
        let error = label_if_enabled(Ok("1\n".into()), || Err(io::Error::other("label failed")))
            .unwrap_err();
        assert_eq!(error.to_string(), "label failed");
    }

    #[test]
    fn permissive_still_labels_executable() {
        let mut labeled = false;
        label_if_enabled(Ok("0\n".into()), || {
            labeled = true;
            Ok(())
        })
        .unwrap();
        assert!(labeled);
    }

    #[test]
    fn absent_selinux_requires_no_command() {
        label_if_enabled(Err(io::ErrorKind::NotFound.into()), || {
            panic!("must not label")
        })
        .unwrap();
    }

    #[test]
    fn detection_error_is_not_treated_as_disabled() {
        let error = label_if_enabled(Err(io::ErrorKind::PermissionDenied.into()), || {
            panic!("must not label")
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn invalid_mode_rejects_before_command() {
        let error =
            label_if_enabled(Ok("unknown".into()), || panic!("must not label")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn missing_primary_command_uses_fixed_fallback() {
        let mut calls = Vec::new();
        run_chcon(|program| {
            calls.push(program.to_owned());
            if calls.len() == 1 {
                Err(io::ErrorKind::NotFound.into())
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(calls, ["/usr/bin/chcon", "/bin/chcon"]);
    }

    #[test]
    fn permission_denied_does_not_try_alternative_command() {
        let mut calls = Vec::new();
        let error = run_chcon(|program| {
            calls.push(program.to_owned());
            Err(io::ErrorKind::PermissionDenied.into())
        })
        .unwrap_err();
        assert_eq!(
            (error.kind(), calls),
            (
                io::ErrorKind::PermissionDenied,
                vec!["/usr/bin/chcon".to_owned()]
            )
        );
    }

    #[test]
    fn command_failure_is_not_hidden_by_fallback() {
        let mut calls = Vec::new();
        let error = run_chcon(|program| {
            calls.push(program.to_owned());
            Err(io::Error::other("chcon exited unsuccessfully"))
        })
        .unwrap_err();
        assert_eq!(
            (error.to_string(), calls),
            (
                "chcon exited unsuccessfully".to_owned(),
                vec!["/usr/bin/chcon".to_owned()]
            )
        );
    }
}
