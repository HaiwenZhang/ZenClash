use std::{fs, path::Path};

use super::{AutostartError, AutostartResult, AutostartStatus, home_dir, required_entry_path};
use crate::profiles::atomic_write;

pub(super) fn default_entry_path() -> AutostartResult<Option<std::path::PathBuf>> {
    Ok(Some(
        home_dir()?.join("Library/LaunchAgents/dev.zenclash.app.plist"),
    ))
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
    let expected = launch_agent(executable);
    Ok(AutostartStatus {
        enabled: true,
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
        atomic_write(entry_path, launch_agent(executable).as_bytes())?;
    } else if let Err(error) = fs::remove_file(entry_path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return Err(AutostartError::Io(error));
    }
    Ok(())
}

fn launch_agent(executable: &Path) -> String {
    let executable = xml_escape(&executable.display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>dev.zenclash.app</string>
  <key>ProgramArguments</key>
  <array>
    <string>{executable}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <false/>
</dict>
</plist>
"#
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use super::{launch_agent, xml_escape};
    use crate::AutostartManager;

    #[test]
    fn launch_agent_escapes_executable_path() {
        let payload = launch_agent(Path::new("/Applications/A&B <test>.app/ZenClash"));

        assert!(payload.contains("A&amp;B &lt;test&gt;.app"));
    }

    #[test]
    fn xml_escape_handles_all_plist_sensitive_characters() {
        assert_eq!(xml_escape("<&>\"'"), "&lt;&amp;&gt;&quot;&apos;");
    }

    #[test]
    fn manager_writes_reads_and_removes_launch_agent() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-autostart-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let entry = root.join("LaunchAgents/dev.zenclash.app.plist");
        let executable = PathBuf::from("/Applications/ZenClash.app/Contents/MacOS/zenclash");
        let manager = AutostartManager::with_entry_path(&executable, &entry);

        let enabled = manager.set_enabled(true).unwrap();
        assert!(enabled.enabled && enabled.matches_current_executable);
        let disabled = manager.set_enabled(false).unwrap();
        assert!(!disabled.enabled);

        fs::remove_dir_all(root).unwrap();
    }
}
