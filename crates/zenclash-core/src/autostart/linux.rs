use std::{fs, path::Path};

use super::{AutostartError, AutostartResult, AutostartStatus, required_entry_path};
use crate::profiles::atomic_write;

pub(super) fn default_entry_path() -> AutostartResult<Option<std::path::PathBuf>> {
    let config = crate::application_paths::user_configuration_directory().ok_or_else(|| {
        AutostartError::Path(zenclash_i18n::text("autostart.details.home_directory"))
    })?;
    Ok(Some(config.join("autostart/zenclash.desktop")))
}

pub(super) fn status(
    executable: &Path,
    entry_path: Option<&Path>,
) -> AutostartResult<AutostartStatus> {
    let entry_path = required_entry_path(entry_path)?;
    if !entry_path.exists() {
        return Ok(AutostartStatus {
            location: entry_path.display().to_string(),
            ..AutostartStatus::default()
        });
    }
    let current = fs::read(entry_path)?;
    let expected = desktop_entry(executable);
    Ok(AutostartStatus {
        enabled: !String::from_utf8_lossy(&current)
            .lines()
            .any(|line| line.trim().eq_ignore_ascii_case("Hidden=true")),
        matches_current_executable: current == expected.as_bytes(),
        location: entry_path.display().to_string(),
    })
}

pub(super) fn set_enabled(
    executable: &Path,
    entry_path: Option<&Path>,
    enabled: bool,
) -> AutostartResult<()> {
    let entry_path = required_entry_path(entry_path)?;
    if enabled {
        atomic_write(entry_path, desktop_entry(executable).as_bytes())?;
    } else if let Err(error) = fs::remove_file(entry_path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return Err(AutostartError::Io(error));
    }
    Ok(())
}

fn desktop_entry(executable: &Path) -> String {
    let executable = desktop_exec_quote(&executable.display().to_string());
    format!(
        "[Desktop Entry]\nType=Application\nVersion=1.0\nName=ZenClash\nComment=Native Mihomo client\nExec={executable}\nTerminal=false\nIcon=zenclash\nStartupNotify=false\nX-GNOME-Autostart-enabled=true\n"
    )
}

fn desktop_exec_quote(value: &str) -> String {
    let quoted = format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('`', "\\`")
            .replace('$', "\\$")
            .replace('%', "%%")
    );
    // Desktop string escapes are decoded before Exec argument quoting.
    quoted
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{desktop_entry, desktop_exec_quote};

    #[test]
    fn desktop_exec_escapes_shell_sensitive_path_characters() {
        assert_eq!(
            desktop_exec_quote(r#"/opt/Zen $Clash/"native"`bin`"#),
            r#""/opt/Zen \\$Clash/\\"native\\"\\`bin\\`""#
        );
    }

    #[test]
    fn desktop_exec_escapes_literal_backslashes_percent_and_line_breaks() {
        for (path, expected) in [
            (
                r"/opt/Zen\Clash/zenclash",
                r#""/opt/Zen\\\\Clash/zenclash""#,
            ),
            ("/opt/100%/zenclash", r#""/opt/100%%/zenclash""#),
            ("/opt/Zen\nClash/zenclash", r#""/opt/Zen\nClash/zenclash""#),
        ] {
            assert_eq!(desktop_exec_quote(path), expected);
        }
    }

    #[test]
    fn desktop_entry_contains_a_native_autostart_contract() {
        let payload = desktop_entry(Path::new("/usr/bin/zenclash"));

        assert!(payload.contains("Type=Application\n"));
        assert!(payload.contains("Exec=\"/usr/bin/zenclash\"\n"));
    }
}
