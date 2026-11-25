// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! macOS `launchd` privileged helper installation and uninstallation.

#[cfg(target_os = "macos")]
use anyhow::Context as _;
use anyhow::Error;
#[cfg(target_os = "macos")]
use std::fs::File;
#[cfg(target_os = "macos")]
use std::io::Write as _;
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use super::{
    InstallOptions, bundled_service_binary, enter_repair_gate, install_service_cores, prepare_requested_cores,
    publish_staged_binary, remove_installed_cores, repair_active_owner_state, resolve_service_group_name, run_command,
    stage_binary, wait_for_service_ready,
};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum LaunchdInstallPlan {
    SkipBootout,
    Bootout,
}

pub(super) fn classify_launchd_service_probe(
    exit_code: Option<i32>,
    diagnostic: &str,
) -> Result<LaunchdInstallPlan, Error> {
    match exit_code {
        Some(0) => Ok(LaunchdInstallPlan::Bootout),
        Some(113) if diagnostic.contains("Could not find service") => Ok(LaunchdInstallPlan::SkipBootout),
        _ => Err(anyhow::anyhow!(
            "Unexpected launchctl service probe result (exit code: {:?}): {}",
            exit_code,
            diagnostic
        )),
    }
}

#[cfg(target_os = "macos")]
fn launchd_service_target() -> String {
    format!("system/{}", crate::MACOS_SERVICE_ID)
}

#[cfg(target_os = "macos")]
fn probe_launchd_service(debug: bool) -> Result<LaunchdInstallPlan, Error> {
    if debug {
        println!("Executing: launchctl print {}", launchd_service_target());
    }

    let output = std::process::Command::new("launchctl")
        .args(["print", &launchd_service_target()])
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to probe launchd service: {}", e))?;
    let diagnostic = format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    classify_launchd_service_probe(output.status.code(), &diagnostic)
}

#[cfg(target_os = "macos")]
fn set_macos_owner(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::lchown;

    lchown(path, Some(0), Some(0)).with_context(|| format!("failed to set root:wheel owner on {path:?}"))
}

#[cfg(target_os = "macos")]
fn set_macos_owner_recursive(path: &Path) -> Result<(), Error> {
    set_macos_owner(path)?;
    if !std::fs::symlink_metadata(path)?.file_type().is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(path)? {
        set_macos_owner_recursive(&entry?.path())?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn set_macos_permissions(path: &Path, mode: u32) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("failed to set mode {mode:o} on {path:?}"))
}

#[cfg(target_os = "macos")]
pub(super) fn install(mut options: InstallOptions) -> Result<(), Error> {
    let _gate = enter_repair_gate()?;
    prepare_requested_cores(&mut options.cores)?;
    if !options.install_service {
        return install_service_cores(&options.cores);
    }
    let group_name = resolve_service_group_name()?;
    let debug = options.debug;
    let launchd_install_plan = probe_launchd_service(debug)?;
    let service_binary_path = bundled_service_binary()?;

    let bundle_path =
        PathBuf::from("/Library/PrivilegedHelperTools").join(format!("{}.bundle", crate::MACOS_SERVICE_ID));
    let contents_path = bundle_path.join("Contents");
    let macos_path = contents_path.join("MacOS");

    std::fs::create_dir_all(&macos_path).map_err(|e| anyhow::anyhow!("Failed to create bundle directories: {}", e))?;

    let target_binary_path = macos_path.join("zenclash-service");
    let staged = stage_binary(&service_binary_path, &target_binary_path)?;
    let info_plist_path = contents_path.join("Info.plist");

    let plist_dir = PathBuf::from("/Library/LaunchDaemons");
    if !plist_dir.exists() {
        std::fs::create_dir(&plist_dir).map_err(|e| anyhow::anyhow!("Failed to create plist directory: {}", e))?;
    }

    let plist_file = plist_dir.join(format!("{}.plist", crate::MACOS_SERVICE_ID));

    let launchd_plist_content = format!(
        include_str!("../../resources/launchd.plist.tmpl"),
        group_name = group_name,
        service_id = crate::MACOS_SERVICE_ID,
        app_bundle_id = crate::MACOS_APP_BUNDLE_ID,
        service_binary = target_binary_path.to_string_lossy(),
    );
    let info_plist_content = format!(
        include_str!("../../resources/info.plist.tmpl"),
        display_name = crate::SERVICE_DISPLAY_NAME,
        service_id = crate::MACOS_SERVICE_ID,
    );
    let plist_path = plist_file.to_string_lossy().into_owned();

    if launchd_install_plan == LaunchdInstallPlan::Bootout {
        // A loaded job may have lost its plist; only the label still reaches it.
        run_command("launchctl", &["bootout", &launchd_service_target()], debug)?;
    }
    repair_active_owner_state()?;
    // Staged where the service is already down, so a core it was running no longer holds its file.
    install_service_cores(&options.cores)?;
    publish_staged_binary(&staged, &target_binary_path)?;
    std::fs::write(&info_plist_path, info_plist_content)
        .with_context(|| format!("failed to write Info.plist {info_plist_path:?}"))?;
    File::create(&plist_file)
        .and_then(|mut file| file.write_all(launchd_plist_content.as_bytes()))
        .map_err(|e| anyhow::anyhow!("Failed to write plist file: {}", e))?;

    set_macos_permissions(&plist_file, 0o644)?;
    set_macos_owner(&plist_file)?;
    set_macos_permissions(&target_binary_path, 0o544)?;
    set_macos_owner(&target_binary_path)?;
    set_macos_permissions(&bundle_path, 0o755)?;
    set_macos_owner_recursive(&bundle_path)?;

    let launchd_target = launchd_service_target();
    run_command("launchctl", &["enable", &launchd_target], debug)?;
    run_command("launchctl", &["bootstrap", "system", &plist_path], debug)?;
    run_command("launchctl", &["start", crate::MACOS_SERVICE_ID], debug)?;
    wait_for_service_ready()?;

    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) fn uninstall(debug: bool) -> Result<(), Error> {
    let _gate = enter_repair_gate()?;

    let bundle_path = format!("/Library/PrivilegedHelperTools/{}.bundle", crate::MACOS_SERVICE_ID);
    let plist_file = format!("/Library/LaunchDaemons/{}.plist", crate::MACOS_SERVICE_ID);
    let service_id = crate::MACOS_SERVICE_ID;

    let _ = run_command("launchctl", &["stop", service_id], debug);
    let _ = run_command("launchctl", &["disable", &format!("system/{service_id}")], debug);
    let _ = run_command("launchctl", &["bootout", &format!("system/{service_id}")], debug);

    if Path::new(&plist_file).exists() {
        std::fs::remove_file(&plist_file).map_err(|e| anyhow::anyhow!("Failed to remove plist file: {}", e))?;
    }

    if Path::new(&bundle_path).exists() {
        std::fs::remove_dir_all(&bundle_path)
            .map_err(|e| anyhow::anyhow!("Failed to remove bundle directory: {}", e))?;
    }

    if let Err(error) = repair_active_owner_state() {
        eprintln!("Warning: failed to repair active owner state during uninstall: {error:#}");
    }
    remove_installed_cores();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{LaunchdInstallPlan, classify_launchd_service_probe};

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_service_uses_on_demand_activation() -> anyhow::Result<()> {
        use std::io::Write as _;

        let content = format!(
            include_str!("../../resources/launchd.plist.tmpl"),
            group_name = "staff",
            service_id = crate::MACOS_SERVICE_ID,
            app_bundle_id = crate::MACOS_APP_BUNDLE_ID,
            service_binary = "/Library/PrivilegedHelperTools/fixture/zenclash-service",
        );
        for key in ["KeepAlive", "RunAtLoad"] {
            let mut child = std::process::Command::new("/usr/bin/plutil")
                .args(["-extract", key, "raw", "-o", "-", "-"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()?;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(content.as_bytes())?;
            }
            let output = child.wait_with_output()?;
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8(output.stdout)?.trim(),
                "false",
                "{key} must not keep an unused service running"
            );
        }
        Ok(())
    }

    #[test]
    fn missing_launchd_service_skips_bootout() {
        let plan = classify_launchd_service_probe(
            Some(113),
            "Could not find service \"org.zenclash.app.service\" in domain for system",
        )
        .expect("missing service should skip bootout");
        assert_eq!(plan, LaunchdInstallPlan::SkipBootout);
    }

    #[test]
    fn loaded_launchd_service_runs_bootout() {
        let plan = classify_launchd_service_probe(Some(0), "").expect("loaded service should run bootout");
        assert_eq!(plan, LaunchdInstallPlan::Bootout);
    }

    #[test]
    fn unexpected_launchd_exit_or_diagnostic_is_an_error() {
        assert!(classify_launchd_service_probe(Some(5), "Could not find service").is_err());
        assert!(classify_launchd_service_probe(Some(113), "Operation not permitted").is_err());
    }
}
