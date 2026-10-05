// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
#![cfg(all(feature = "standalone", feature = "client"))]

#[test]
fn non_unicode_arguments_do_not_panic_before_parsing() -> std::io::Result<()> {
    use std::ffi::OsString;

    #[cfg(windows)]
    let argument = {
        use std::os::windows::ffi::OsStringExt as _;
        OsString::from_wide(&[0xd800])
    };
    #[cfg(unix)]
    let argument = {
        use std::os::unix::ffi::OsStringExt as _;
        OsString::from_vec(vec![0xff])
    };

    // Reject before entering the repair gate or changing any service state.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_zenclash-service-install"))
        .arg("--unknown-option")
        .arg(argument)
        .output()?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("unknown installer argument"), "{stderr}");
    Ok(())
}

#[test]
fn preparation_rejects_missing_cores_before_elevation() -> std::io::Result<()> {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_zenclash-service-install"))
        .args(["--prepare-install", "--ensure"])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no core executable"));
    Ok(())
}

#[test]
fn service_version_query_exits_without_starting_a_daemon() -> std::io::Result<()> {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_zenclash-service"))
        .arg("--version")
        .output()?;
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("zenclash-service {}", zenclash_service::VERSION)
    );
    Ok(())
}
