use anyhow::{Context as _, Result, bail};
use std::path::Path;

/// Kept distinct so bundled-core fallback cannot swallow a labeling failure.
#[derive(Debug)]
pub(super) struct LabelError(anyhow::Error);

impl std::fmt::Display for LabelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SELinux executable labeling failed")
    }
}

impl std::error::Error for LabelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

pub(super) fn ensure_executable_label(path: &Path) -> Result<()> {
    label_executable(path)
        .map_err(|error| LabelError(error.context(format!("could not label {path:?} as bin_t"))).into())
}

fn label_executable(path: &Path) -> Result<()> {
    let enforce_path = Path::new("/sys/fs/selinux/enforce");
    match std::fs::read_to_string(enforce_path) {
        // Permissive also needs labels: switching to Enforcing must not break the installation.
        Ok(mode) if matches!(mode.trim(), "0" | "1") => {}
        Ok(mode) => bail!("unexpected SELinux enforcement mode {mode:?} at {enforce_path:?}"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("failed to detect SELinux enforcement mode"),
    }
    let directory = path.parent().context("executable has no parent directory")?;
    // Relabel the whole bin/ or cores/ directory, like the packaged `(/.*)?` rule does, so files
    // created before the directory was labeled (the repair lock) do not stay var_lib_t. Recursion
    // never leaves that directory. Do not search the elevated caller's PATH.
    let run = |command| {
        std::process::Command::new(command)
            .args(["--no-dereference", "--recursive", "--type=bin_t", "--"])
            .arg(directory)
            .output()
    };
    let output = run("/usr/bin/chcon")
        .or_else(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                run("/bin/chcon")
            } else {
                Err(error)
            }
        })
        .context(
            "failed to execute chcon from /usr/bin or /bin; check SELinux-capable coreutils and execution permissions",
        )?;
    if !output.status.success() {
        bail!(
            "chcon exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}
