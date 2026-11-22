use super::*;

fn scratch(label: &str) -> anyhow::Result<PathBuf> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!("install-core-{label}-{}-{timestamp}", std::process::id()));
    std::fs::create_dir_all(root.join("cores"))?;
    Ok(root)
}

fn combined_options(sources: &[PathBuf]) -> anyhow::Result<InstallOptions> {
    let mut arguments = vec![OsString::from("--install-service")];
    for source in sources {
        let digest: String = sha256(source)?.iter().map(|byte| format!("{byte:02x}")).collect();
        arguments.extend([
            "--install-core".into(),
            source.as_os_str().to_owned(),
            "--sha256".into(),
            digest.into(),
        ]);
    }
    parse_install_arguments(arguments.into_iter())
}

#[test]
fn stopped_service_can_be_reused_only_with_matching_service_and_cores() -> anyhow::Result<()> {
    let root = scratch("ensure-payload")?;
    let source = root.join("bundled-service");
    let target = root.join("installed-service");
    let core = root.join(core_file_name("mihomo"));
    std::fs::write(&source, b"service")?;
    std::fs::write(&target, b"service")?;
    std::fs::write(&core, b"core")?;
    let options = combined_options(std::slice::from_ref(&core))?;
    assert!(installed_payload_matches(&source, &target, &options.cores, &root)?);
    std::fs::write(&core, b"different core")?;
    assert!(!installed_payload_matches(&source, &target, &options.cores, &root)?);
    std::fs::write(&core, b"core")?;
    std::fs::write(&target, b"old service")?;
    assert!(!installed_payload_matches(&source, &target, &options.cores, &root)?);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn combined_install_publishes_both_attested_cores_from_a_writable_directory() -> anyhow::Result<()> {
    let root = scratch("combined-install")?;
    let stable = root.join(core_file_name("mihomo"));
    let alpha = root.join(core_file_name("mihomo-alpha"));
    std::fs::write(&stable, b"stable core")?;
    std::fs::write(&alpha, b"alpha core")?;
    let mut options = combined_options(&[stable, alpha])?;
    assert!(options.install_service);
    prepare_requested_cores(&mut options.cores)?;
    let installed = publish_requested_cores(&root.join("cores"), &options.cores)?;
    assert_eq!(installed.len(), 2);
    assert_eq!(std::fs::read(&installed[0])?, b"stable core");
    assert_eq!(std::fs::read(&installed[1])?, b"alpha core");
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn combined_install_preflight_rejects_a_missing_or_mismatched_second_core() -> anyhow::Result<()> {
    let root = scratch("preflight")?;
    let stable = root.join(core_file_name("mihomo"));
    let alpha = root.join(core_file_name("mihomo-alpha"));
    std::fs::write(&stable, b"stable core")?;
    std::fs::write(&alpha, b"alpha core")?;
    let mut options = combined_options(&[stable, alpha.clone()])?;
    std::fs::write(&alpha, b"changed alpha")?;
    let error = prepare_requested_cores(&mut options.cores).expect_err("must reject before stopping the service");
    assert!(error.to_string().contains("attested sha256"), "got {error:#}");
    std::fs::remove_file(alpha)?;
    assert!(prepare_requested_cores(&mut options.cores).is_err());
    assert!(std::fs::read_dir(root.join("cores"))?.next().is_none());
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn publishes_the_bytes_and_stamps_the_source_modified_time() -> anyhow::Result<()> {
    let root = scratch("stamp")?;
    let source = root.join("mihomo");
    std::fs::write(&source, b"core bytes")?;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&source)?
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000))?;

    let target = install_core(&source, &root.join("cores"), OsStr::new("mihomo"), None)?;
    assert_eq!(std::fs::read(&target)?, b"core bytes");
    assert_eq!(
        std::fs::metadata(&source)?.modified()?,
        std::fs::metadata(&target)?.modified()?,
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&target)?.permissions().mode() & 0o777, 0o550);
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn wrong_attestation_and_bookkeeping_suffixes_are_refused() -> anyhow::Result<()> {
    let root = scratch("attestation")?;
    let source = root.join("mihomo");
    std::fs::write(&source, b"core bytes")?;
    let error =
        install_core(&source, &root.join("cores"), OsStr::new("mihomo"), Some(&[0u8; 32])).expect_err("must refuse");
    assert!(error.to_string().contains("attested sha256"), "got {error:#}");

    for name in ["mihomo.next", "mihomo.old"] {
        let bad = root.join(name);
        std::fs::write(&bad, b"core bytes")?;
        let error = install_core(&bad, &root.join("cores"), OsStr::new(name), None).expect_err("must refuse");
        assert!(error.to_string().contains("bookkeeping suffix"), "got {error:#}");
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn install_argument_parser_handles_service_and_core_options() -> anyhow::Result<()> {
    let digest = "ab".repeat(32);
    let options = parse_install_arguments(
        ["--install-service", "--install-core", "/dl/mihomo", "--sha256", &digest]
            .into_iter()
            .map(OsString::from),
    )?;
    assert!(options.install_service);
    assert_eq!(options.cores.len(), 1);
    assert_eq!(options.cores[0].sha256, Some([0xab; 32]));

    assert!(parse_install_arguments(std::iter::empty())?.install_service);
    assert!(!parse_install_arguments(["--install-core", "/dl/core"].into_iter().map(OsString::from))?.install_service);
    assert!(parse_install_arguments(["--unknown"].into_iter().map(OsString::from)).is_err());
    assert!(parse_sha256_hex(&digest).is_ok());
    assert!(parse_sha256_hex("short").is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn group_lookup_grows_a_buffer_too_small_for_the_record() {
    let (gid, name) = if cfg!(target_os = "macos") {
        (20, "staff")
    } else {
        (0, "root")
    };
    assert_eq!(
        lookup_group_name(gid, 8)
            .expect("group lookup should succeed")
            .as_deref(),
        Some(name)
    );
}
