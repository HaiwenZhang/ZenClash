// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! Linux `systemd` privileged helper installation and uninstallation.

#[cfg(target_os = "linux")]
use anyhow::Context as _;
use anyhow::{Error, ensure};
#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::io::Write as _;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use super::{
    InstallOptions, bundled_service_binary, enter_repair_gate, install_service_cores, prepare_requested_cores,
    publish_staged_binary, remove_installed_cores, repair_active_owner_state, resolve_service_group_name, run_command,
    stage_binary, wait_for_service_ready,
};

/// Validates Unix owner/mode constraints for a protected root-only path (`uid == 0 && mode & 0o022 == 0`).
pub(super) fn validate_root_owned_mode(uid: u32, mode: u32, label: &str) -> Result<(), Error> {
    ensure!(uid == 0, "{label} must be owned by root (uid 0), found uid {uid}");
    ensure!(
        mode & 0o022 == 0,
        "{label} must not be group- or world-writable (mode {:o})",
        mode & 0o7777
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_root_owned_unwritable(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = std::fs::symlink_metadata(path).with_context(|| format!("failed to inspect {path:?}"))?;
    validate_root_owned_mode(metadata.uid(), metadata.mode(), &path.display().to_string())?;
    if let Some(parent) = path.parent() {
        let parent_meta = std::fs::symlink_metadata(parent)
            .with_context(|| format!("failed to inspect parent directory {parent:?}"))?;
        validate_root_owned_mode(parent_meta.uid(), parent_meta.mode(), &parent.display().to_string())?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn install(mut options: InstallOptions) -> Result<(), Error> {
    let _gate = enter_repair_gate()?;
    prepare_requested_cores(&mut options.cores)?;
    if !options.install_service {
        return install_service_cores(&options.cores);
    }
    let group = resolve_service_group_name()?;
    let debug = options.debug;
    let source = bundled_service_binary()?;
    let install_dir = crate::prepare_service_install_directory()?;
    let target = install_dir.join("zenclash-service");
    let staged = stage_binary(&source, &target)?;
    let unit_name = format!("{}.service", crate::SERVICE_SLUG);
    let unit_path = PathBuf::from("/etc/systemd/system").join(&unit_name);

    let _ = run_command("systemctl", &["stop", &unit_name], debug);
    repair_active_owner_state()?;
    // Staged where the service is already down, so a core it was running no longer holds its file.
    install_service_cores(&options.cores)?;
    publish_staged_binary(&staged, &target)?;
    verify_root_owned_unwritable(&target)?;

    let unit_file_content = format!(
        include_str!("../../resources/systemd_service_unit.tmpl"),
        exec_start = target.to_string_lossy(),
        group = group,
        runtime_directory = crate::SERVICE_SLUG,
    );

    let mut unit_file =
        File::create(&unit_path).with_context(|| format!("failed to create systemd unit {unit_path:?}"))?;
    unit_file
        .write_all(unit_file_content.as_bytes())
        .with_context(|| format!("failed to write systemd unit {unit_path:?}"))?;
    unit_file
        .sync_all()
        .with_context(|| format!("failed to sync systemd unit {unit_path:?}"))?;

    run_command("systemctl", &["daemon-reload"], debug)?;
    run_command("systemctl", &["enable", &unit_name], debug)?;
    run_command("systemctl", &["start", &unit_name], debug)?;
    wait_for_service_ready()?;

    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn uninstall(debug: bool) -> Result<(), Error> {
    let _gate = enter_repair_gate()?;
    let service_name = crate::SERVICE_SLUG;

    let _ = run_command("systemctl", &["stop", &format!("{service_name}.service")], debug);
    let _ = run_command("systemctl", &["disable", &format!("{service_name}.service")], debug);

    let unit_file = format!("/etc/systemd/system/{service_name}.service");
    if Path::new(&unit_file).exists() {
        std::fs::remove_file(&unit_file).map_err(|e| anyhow::anyhow!("Failed to remove service file: {}", e))?;
    }

    let _ = run_command("systemctl", &["daemon-reload"], debug);
    let target = crate::prepare_service_install_directory()?.join("zenclash-service");
    if target.exists() {
        std::fs::remove_file(&target)
            .map_err(|error| anyhow::anyhow!("Failed to remove service binary {target:?}: {error}"))?;
    }
    // A fallback publish may have displaced a locked service image aside; best-effort.
    let _ = std::fs::remove_file(target.with_extension(crate::CORE_DISPLACED_EXTENSION));

    if let Err(error) = repair_active_owner_state() {
        eprintln!("Warning: failed to repair active owner state during uninstall: {error:#}");
    }
    remove_installed_cores();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_root_owned_mode;

    #[test]
    fn root_owned_unwritable_check_accepts_hardened_and_rejects_writable_paths() {
        assert!(validate_root_owned_mode(0, 0o100550, "/var/lib/zenclash-service/bin/zenclash-service").is_ok());
        assert!(validate_root_owned_mode(0, 0o040755, "/var/lib/zenclash-service/bin").is_ok());
        assert!(validate_root_owned_mode(1000, 0o100550, "/tmp/zenclash-service").is_err());
        assert!(validate_root_owned_mode(0, 0o100775, "/var/lib/zenclash-service/bin/zenclash-service").is_err());
        assert!(validate_root_owned_mode(0, 0o100757, "/var/lib/zenclash-service/bin/zenclash-service").is_err());
    }
}
