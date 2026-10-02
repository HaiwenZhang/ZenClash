#[cfg(unix)]
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use flate2::{Compression, write::GzEncoder};
#[cfg(unix)]
use sha2::{Digest, Sha256};

use super::{
    CoreUpdateError, MihomoReleaseService,
    service::{
        RawAsset, RawRelease, parse_digest, platform_asset_name, validate_asset_url, verify_sha256,
    },
    transaction::{PreparedCoreUpdate, sibling_path},
    workflow::versions_match,
};

fn unique_directory(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "zenclash-core-update-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos()
    ))
}

#[cfg(unix)]
fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn validates_release_digest_and_platform_asset_name() {
    let tag = "v1.19.30";
    let name = platform_asset_name(tag).expect("supported CI platform");

    assert!(name.contains(tag));
    assert_eq!(
        parse_digest(&format!("sha256:{}", "A1".repeat(32))).unwrap(),
        "a1".repeat(32)
    );
    assert!(parse_digest("").is_err());
    assert!(parse_digest("sha256:abcd").is_err());
    assert!(platform_asset_name("../candidate").is_err());
}

#[test]
fn rejects_checksum_mismatch() {
    let error = verify_sha256(b"download", &"00".repeat(32)).unwrap_err();

    assert!(matches!(error, CoreUpdateError::Checksum { .. }));
}

#[test]
fn version_comparison_ignores_only_the_conventional_v_prefix() {
    assert!(versions_match("v1.19.30", "1.19.30"));
    assert!(versions_match(" 1.19.30 ", "v1.19.30"));
    assert!(!versions_match("1.19.3", "v1.19.30"));
}

#[test]
fn releases_without_github_digest_are_not_offered() {
    let service = MihomoReleaseService::with_base("http://127.0.0.1/", true).unwrap();
    let release = RawRelease {
        tag_name: "v1.19.30".into(),
        published_at: None,
        prerelease: false,
        draft: false,
        assets: vec![RawAsset {
            name: platform_asset_name("v1.19.30").unwrap(),
            browser_download_url: "http://127.0.0.1/mihomo.gz".into(),
            size: 1,
            digest: None,
        }],
    };

    assert!(service.select_release(release).unwrap().is_none());
}

#[test]
fn asset_urls_reject_downgrades_credentials_and_untrusted_redirect_hosts() {
    let trusted = reqwest::Url::parse(
        "https://release-assets.githubusercontent.com/github-production-release-asset/file?token=1",
    )
    .unwrap();
    assert!(validate_asset_url(&trusted, false).is_ok());

    for value in [
        "http://github.com/MetaCubeX/mihomo/releases/download/v1/mihomo.gz",
        "https://user:secret@github.com/MetaCubeX/mihomo/releases/download/v1/mihomo.gz",
        "https://github.example/MetaCubeX/mihomo/releases/download/v1/mihomo.gz",
        "https://github.com/MetaCubeX/mihomo/releases/download/v1/mihomo.gz#fragment",
    ] {
        assert!(validate_asset_url(&reqwest::Url::parse(value).unwrap(), false).is_err());
    }
}

#[tokio::test]
#[ignore = "requires the live GitHub Releases API"]
async fn official_release_catalog_has_a_verified_platform_asset() {
    let releases = MihomoReleaseService::new()
        .unwrap()
        .releases(3)
        .await
        .unwrap();

    assert!(!releases.is_empty());
    assert!(releases.iter().all(|release| {
        release.asset.download_url.scheme() == "https"
            && release.asset.download_url.host_str() == Some("github.com")
            && release.asset.sha256.len() == 64
    }));
}

#[tokio::test]
#[ignore = "downloads and validates the current official Mihomo archive"]
async fn official_release_archive_prepares_a_real_candidate_without_activation() {
    let target = std::env::var_os("ZENCLASH_MIHOMO_BINARY")
        .map(PathBuf::from)
        .expect("ZENCLASH_MIHOMO_BINARY must point to an existing executable");
    let service = MihomoReleaseService::new().unwrap();
    let release = service
        .releases(5)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("current platform release");

    let prepared = service.prepare(&release, target).await.unwrap();

    assert_eq!(prepared.tag(), release.tag);
}

#[test]
fn transaction_commit_keeps_candidate() {
    let directory = unique_directory("commit");
    std::fs::create_dir_all(&directory).unwrap();
    let target = directory.join(if cfg!(windows) {
        "mihomo.exe"
    } else {
        "mihomo"
    });
    let staging = sibling_path(&target, "test-staging").unwrap();
    std::fs::write(&target, b"old").unwrap();
    std::fs::write(&staging, b"new").unwrap();
    let prepared = PreparedCoreUpdate {
        staging: Some(staging),
        target: target.clone(),
        tag: "v9.9.9".into(),
    };

    prepared.activate().unwrap().commit().unwrap();

    assert_eq!(std::fs::read(&target).unwrap(), b"new");
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn abandoned_staging_and_failed_activation_leave_the_old_core_intact() {
    let directory = unique_directory("activation-failure");
    std::fs::create_dir_all(&directory).unwrap();
    let target = directory.join(if cfg!(windows) {
        "mihomo.exe"
    } else {
        "mihomo"
    });
    let abandoned = sibling_path(&target, "abandoned").unwrap();
    std::fs::write(&target, b"old").unwrap();
    std::fs::write(&abandoned, b"unused").unwrap();
    drop(PreparedCoreUpdate {
        staging: Some(abandoned.clone()),
        target: target.clone(),
        tag: "v9.9.9".into(),
    });
    assert!(!abandoned.exists());

    let missing = sibling_path(&target, "missing").unwrap();
    let prepared = PreparedCoreUpdate {
        staging: Some(missing.clone()),
        target: target.clone(),
        tag: "v9.9.9".into(),
    };

    assert!(prepared.activate().is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    assert!(!missing.exists());
    assert_eq!(
        std::fs::read_dir(&directory).unwrap().count(),
        1,
        "failed activation must not leave backup or staging files"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn application_shutdown_during_release_download_never_restarts_the_owned_core() {
    use crate::{
        CoreKind, CoreSession, MihomoClient, MihomoEndpoint, MihomoLaunchConfig, MihomoProcess,
    };
    use std::os::unix::fs::PermissionsExt;

    let directory = unique_directory("shutdown-download");
    std::fs::create_dir_all(&directory).unwrap();
    let target = directory.join("mihomo");
    let old = b"#!/bin/sh\nif [ \"$1\" = '-t' ]; then exit 0; fi\nexec sleep 60\n";
    std::fs::write(&target, old).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    let profile = directory.join("profile.yaml");
    std::fs::write(&profile, "mode: rule\nrules: [MATCH,DIRECT]\n").unwrap();
    let candidate = b"#!/bin/sh\nif [ \"$1\" = '-v' ]; then printf 'Mihomo Meta v9.9.9 test\\n'; exit 0; fi\nif [ \"$1\" = '-t' ]; then exit 0; fi\nexec sleep 60\n";
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(candidate).unwrap();
    let archive = encoder.finish().unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let (requested, request_received) = tokio::sync::oneshot::channel();
    let (release_download, download_released) = std::sync::mpsc::channel();
    let (stop, stopped) = std::sync::mpsc::channel();
    let release = super::MihomoRelease {
        tag: "v9.9.9".into(),
        published_at: String::new(),
        prerelease: false,
        asset: super::MihomoReleaseAsset {
            name: platform_asset_name("v9.9.9").unwrap(),
            download_url: format!("http://{address}/asset.gz").parse().unwrap(),
            size: archive.len() as u64,
            sha256: sha256(&archive),
        },
    };
    let server = thread::spawn(move || {
        let mut requested = Some(requested);
        while stopped.try_recv().is_err() {
            let (mut stream, _) = match listener.accept() {
                Ok(stream) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(std::time::Duration::from_millis(2));
                    continue;
                }
                Err(error) => panic!("controller accept failed: {error}"),
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut request = [0_u8; 8192];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            let body = if request.starts_with("GET /asset.gz ") {
                requested.take().unwrap().send(()).unwrap();
                download_released
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                archive.as_slice()
            } else {
                assert!(request.starts_with("GET /version "), "{request}");
                br#"{"meta":true,"version":"9.9.9"}"#.as_slice()
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream
                .write_all(response.as_bytes())
                .and_then(|()| stream.write_all(body));
        }
    });
    let endpoint = MihomoEndpoint::new(format!("http://{address}"), "");
    let process = MihomoProcess::spawn(MihomoLaunchConfig {
        kind: CoreKind::Mihomo,
        binary: target.clone(),
        config_file: profile,
        home_dir: directory.join("data"),
        endpoint: endpoint.clone(),
        controller_override: None,
    })
    .unwrap();
    let client = MihomoClient::from_process(process.clone()).unwrap();
    assert_eq!(client.endpoint(), Some(endpoint));
    let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
    let service = MihomoReleaseService::with_base(&format!("http://{address}/"), true).unwrap();
    let installing = {
        let session = session.clone();
        tokio::spawn(async move { session.install_release(&service, &release).await })
    };
    request_received.await.unwrap();
    session.shutdown().await.unwrap();
    release_download.send(()).unwrap();
    let result = installing.await.unwrap();
    let running = process.is_running();
    let executable = std::fs::read(&target).unwrap();
    process.stop_async().await.unwrap();
    stop.send(()).unwrap();
    server.join().unwrap();
    assert!(
        !running,
        "release installation restarted the core after application shutdown"
    );
    assert_eq!(
        executable, old,
        "shutdown changed the active core executable"
    );
    assert!(
        result.is_err(),
        "shutdown did not cancel release installation"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn release_download_activation_and_rollback_are_transactional() {
    use std::os::unix::fs::PermissionsExt;

    let candidate = b"#!/bin/sh\nprintf 'Mihomo Meta v9.9.9 test\\n'\n";
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(candidate).unwrap();
    let archive = encoder.finish().unwrap();
    let digest = sha256(&archive);
    let asset_name = platform_asset_name("v9.9.9").unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let asset_url = format!("http://{address}/asset.gz");
    let metadata = serde_json::json!([{
        "tag_name": "v9.9.9",
        "published_at": "2026-08-26T00:00:00Z",
        "prerelease": false,
        "draft": false,
        "assets": [{
            "name": asset_name,
            "browser_download_url": asset_url,
            "size": archive.len(),
            "digest": format!("sha256:{digest}")
        }]
    }])
    .to_string();
    let archive_for_server = archive.clone();
    let server = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4_096];
            let length = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            let (content_type, body) = if request.starts_with("GET /releases?") {
                ("application/json", metadata.as_bytes())
            } else {
                ("application/gzip", archive_for_server.as_slice())
            };
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        }
    });
    let directory = unique_directory("rollback");
    std::fs::create_dir_all(&directory).unwrap();
    let target = directory.join("mihomo");
    std::fs::write(&target, b"#!/bin/sh\nprintf 'old core\\n'\n").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    let service = MihomoReleaseService::with_base(&format!("http://{address}/"), true).unwrap();

    let release = service.releases(1).await.unwrap().remove(0);
    let prepared = service.prepare(&release, &target).await.unwrap();
    assert!(
        String::from_utf8_lossy(&std::process::Command::new(&target).output().unwrap().stdout)
            .contains("old core")
    );
    let transaction = prepared.activate().unwrap();
    assert!(
        String::from_utf8_lossy(
            &std::process::Command::new(&target)
                .arg("-v")
                .output()
                .unwrap()
                .stdout
        )
        .contains("Mihomo Meta v9.9.9")
    );
    transaction.rollback().unwrap();
    assert!(
        String::from_utf8_lossy(&std::process::Command::new(&target).output().unwrap().stdout)
            .contains("old core")
    );

    server.join().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ManagedInstallCase {
    Success,
    SuccessSymlink,
    RejectConfig,
    ReadyFailure,
    ReadyFailureStopped,
    VersionMismatch,
    ShutdownReady,
    CallerDropped,
    CurrentStartup,
}

#[cfg(unix)]
#[tokio::test]
async fn accepted_release_advances_generation_and_preserves_exact_runtime_payload() {
    exercise_managed_install(ManagedInstallCase::Success).await;
}

#[cfg(unix)]
#[tokio::test]
async fn accepted_release_supports_a_binary_symlink_to_an_external_directory() {
    exercise_managed_install(ManagedInstallCase::SuccessSymlink).await;
}

#[cfg(unix)]
#[tokio::test]
async fn rejected_release_config_leaves_the_old_process_and_payload_untouched() {
    exercise_managed_install(ManagedInstallCase::RejectConfig).await;
}

#[cfg(unix)]
#[tokio::test]
async fn unready_release_restores_the_old_binary_and_exact_startup_payload() {
    exercise_managed_install(ManagedInstallCase::ReadyFailure).await;
}

#[cfg(unix)]
#[tokio::test]
async fn unready_release_keeps_an_intentionally_stopped_old_core_stopped() {
    exercise_managed_install(ManagedInstallCase::ReadyFailureStopped).await;
}

#[cfg(unix)]
#[tokio::test]
async fn mismatched_release_restores_the_old_binary_and_exact_startup_payload() {
    exercise_managed_install(ManagedInstallCase::VersionMismatch).await;
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_during_candidate_readiness_restores_files_without_restarting_the_old_core() {
    exercise_managed_install(ManagedInstallCase::ShutdownReady).await;
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_the_install_caller_does_not_abandon_the_owned_transaction() {
    exercise_managed_install(ManagedInstallCase::CallerDropped).await;
}

#[cfg(unix)]
#[tokio::test]
async fn release_precheck_uses_the_profile_committed_during_download() {
    exercise_managed_install(ManagedInstallCase::CurrentStartup).await;
}

#[cfg(unix)]
async fn wait_for_old_startup(home: &std::path::Path, pid: u32) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while std::fs::read_to_string(home.join("old-startup.pid"))
            .ok()
            .and_then(|published| published.trim().parse::<u32>().ok())
            != Some(pid)
        {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("old core did not publish its complete startup payload for the current PID");
}

#[cfg(unix)]
async fn exercise_managed_install(case: ManagedInstallCase) {
    let _ports = crate::core_session::fixed_listener_ports_guard().await;
    use crate::{
        ControlledConfigStore, CoreKind, CoreSession, CoreSessionError, EffectiveConfigIntent,
        MihomoClient, MihomoEndpoint, MihomoLaunchConfig, MihomoProcess,
    };
    use std::{os::unix::fs::PermissionsExt, time::Duration};

    let directory = unique_directory("managed-lifecycle");
    std::fs::create_dir_all(&directory).unwrap();
    let target = directory.join("mihomo");
    let binary_directory = if case == ManagedInstallCase::SuccessSymlink {
        unique_directory("external-binary")
    } else {
        directory.clone()
    };
    std::fs::create_dir_all(&binary_directory).unwrap();
    let physical_target = binary_directory.join("mihomo");
    let old = concat!(
        "#!/bin/sh\nset -e\nif [ \"$1\" = '-t' ]; then exit 0; fi\n",
        "printf 'old\\n' >> \"$2/runs\"\n",
        "cat \"$4\" > \"$2/old-startup.$$.tmp\"\n",
        "mv \"$2/old-startup.$$.tmp\" \"$2/old-startup.yaml\"\n",
        "printf '%s\\n' \"$$\" > \"$2/old-startup-pid.$$.tmp\"\n",
        "mv \"$2/old-startup-pid.$$.tmp\" \"$2/old-startup.pid\"\n",
        "exec sleep 60\n",
    )
    .as_bytes();
    std::fs::write(&physical_target, old).unwrap();
    std::fs::set_permissions(&physical_target, std::fs::Permissions::from_mode(0o755)).unwrap();
    if physical_target != target {
        std::os::unix::fs::symlink(&physical_target, &target).unwrap();
    }
    let source_a = directory.join("a.yaml");
    let source_b = directory.join("b.yaml");
    std::fs::write(
        &source_a,
        "mixed-port: 8011\nmode: rule\nrules: [MATCH,DIRECT]\n",
    )
    .unwrap();
    std::fs::write(
        &source_b,
        "mixed-port: 8012\nmode: rule\nrules: [MATCH,DIRECT]\n",
    )
    .unwrap();
    let store = ControlledConfigStore::new(directory.join("controlled"));
    store
        .materialize_with_overrides_for_core(&source_a, &[], CoreKind::Mihomo)
        .unwrap();
    let previous_payload = std::fs::read(store.runtime_path()).unwrap();
    let validation = match case {
        ManagedInstallCase::RejectConfig => "exit 1",
        ManagedInstallCase::CurrentStartup => "grep -q '8012' \"$5\"",
        _ => "exit 0",
    };
    let launch = if matches!(
        case,
        ManagedInstallCase::ReadyFailure | ManagedInstallCase::ReadyFailureStopped
    ) {
        "exit 7"
    } else {
        "exec sleep 60"
    };
    let candidate = format!("#!/bin/sh\nif [ \"$1\" = '-v' ]; then printf 'Mihomo Meta v9.9.9 test\\n'; exit 0; fi\nif [ \"$1\" = '-t' ]; then {validation}; exit $?; fi\nprintf 'candidate\\n' >> \"$2/runs\"\n{launch}\n").into_bytes();
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&candidate).unwrap();
    let archive = encoder.finish().unwrap();
    let archive_size = archive.len() as u64;
    let archive_digest = sha256(&archive);
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let (asset_seen, asset_received) = tokio::sync::oneshot::channel();
    let (release_asset, asset_released) = std::sync::mpsc::channel();
    let (ready_seen, ready_received) = tokio::sync::oneshot::channel();
    let (release_ready, ready_released) = std::sync::mpsc::channel();
    let (stop, stopped) = std::sync::mpsc::channel();
    let server_target = target.clone();
    let server = thread::spawn(move || {
        let mut asset_seen = Some(asset_seen);
        let mut asset_released = Some(asset_released);
        let mut ready_seen = Some(ready_seen);
        let mut workers = Vec::new();
        while stopped.try_recv().is_err() {
            let (mut stream, _) = match listener.accept() {
                Ok(stream) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(error) => panic!("controller accept failed: {error}"),
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut bytes = [0; 8192];
            let length = stream.read(&mut bytes).unwrap();
            let request = String::from_utf8_lossy(&bytes[..length]);
            if request.starts_with("GET /asset.gz ") {
                asset_seen.take().unwrap().send(()).unwrap();
                let released = asset_released.take().unwrap();
                let archive = archive.clone();
                workers.push(thread::spawn(move || {
                    released.recv_timeout(Duration::from_secs(8)).unwrap();
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        archive.len()
                    );
                    let _ = stream
                        .write_all(header.as_bytes())
                        .and_then(|()| stream.write_all(&archive));
                }));
                continue;
            }
            if request.starts_with("PUT /configs") {
                stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                    .unwrap();
                continue;
            }
            assert!(request.starts_with("GET /version "), "{request}");
            let candidate_active = std::fs::read(&server_target).unwrap() != old;
            if candidate_active && case == ManagedInstallCase::ShutdownReady {
                ready_seen.take().unwrap().send(()).unwrap();
                ready_released.recv_timeout(Duration::from_secs(8)).unwrap();
            }
            let (status, body) = if candidate_active
                && matches!(
                    case,
                    ManagedInstallCase::ReadyFailure | ManagedInstallCase::ReadyFailureStopped
                ) {
                ("503 Service Unavailable", "unready")
            } else if candidate_active && case == ManagedInstallCase::VersionMismatch {
                ("200 OK", r#"{"meta":true,"version":"0.0.0"}"#)
            } else {
                ("200 OK", r#"{"meta":true,"version":"9.9.9"}"#)
            };
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream
                .write_all(header.as_bytes())
                .and_then(|()| stream.write_all(body.as_bytes()));
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });
    let endpoint = MihomoEndpoint::new(format!("http://{address}"), "");
    let process = MihomoProcess::spawn(MihomoLaunchConfig {
        kind: CoreKind::Mihomo,
        binary: target.clone(),
        config_file: store.runtime_path(),
        home_dir: directory.join("data"),
        endpoint: endpoint.clone(),
        controller_override: None,
    })
    .unwrap();
    let session = CoreSession::open_with_config(
        CoreKind::Mihomo,
        MihomoClient::from_process(process.clone()).unwrap(),
        Some(source_a),
        vec![],
    )
    .unwrap();
    wait_for_old_startup(&directory.join("data"), process.snapshot().pid.unwrap()).await;
    if case == ManagedInstallCase::ReadyFailureStopped {
        session
            .maintain(crate::CoreMaintenanceIntent::Stop)
            .await
            .unwrap();
    }
    let previous_pid = process.snapshot().pid;
    let service = MihomoReleaseService::with_base(&format!("http://{address}/"), true).unwrap();
    let release = super::MihomoRelease {
        tag: "v9.9.9".into(),
        published_at: String::new(),
        prerelease: false,
        asset: super::MihomoReleaseAsset {
            name: platform_asset_name("v9.9.9").unwrap(),
            download_url: format!("http://{address}/asset.gz").parse().unwrap(),
            size: archive_size,
            sha256: archive_digest,
        },
    };
    let installing = {
        let session = session.clone();
        tokio::spawn(async move { session.install_release(&service, &release).await })
    };
    tokio::time::timeout(Duration::from_secs(5), asset_received)
        .await
        .unwrap()
        .unwrap();
    if case == ManagedInstallCase::CurrentStartup {
        session
            .apply(
                &store,
                EffectiveConfigIntent::ActivateProfile {
                    profile: source_b,
                    overrides: vec![],
                },
            )
            .await
            .unwrap();
    }
    if case == ManagedInstallCase::CallerDropped {
        installing.abort();
    }
    release_asset.send(()).unwrap();
    if case == ManagedInstallCase::ShutdownReady {
        tokio::time::timeout(Duration::from_secs(5), ready_received)
            .await
            .unwrap()
            .unwrap();
        session.request_shutdown();
        release_ready.send(()).unwrap();
    }
    let result = if case == ManagedInstallCase::CallerDropped {
        assert!(installing.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(5), async {
            while session.generation() == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        None
    } else {
        Some(installing.await.unwrap())
    };
    let snapshot = process.snapshot();
    let executable = std::fs::read(&target).unwrap();
    if snapshot.running && executable == old {
        wait_for_old_startup(&directory.join("data"), snapshot.pid.unwrap()).await;
    }
    let payload = std::fs::read(store.runtime_path()).unwrap();
    let old_startup = std::fs::read(directory.join("data/old-startup.yaml")).unwrap();
    let runs = std::fs::read_to_string(directory.join("data/runs")).unwrap();
    let generation = session.generation();
    let phase = session.lifecycle_snapshot().phase;
    session.shutdown().await.unwrap();
    stop.send(()).unwrap();
    server.join().unwrap();
    match case {
        ManagedInstallCase::Success
        | ManagedInstallCase::SuccessSymlink
        | ManagedInstallCase::CallerDropped
        | ManagedInstallCase::CurrentStartup => {
            if let Some(result) = result {
                let receipt = result.unwrap();
                assert_eq!(receipt.version.version, "9.9.9");
                assert_eq!(receipt.generation, generation);
                assert!(receipt.cleanup_error.is_none());
            }
            assert!(snapshot.running);
            assert_eq!(executable, candidate);
            assert_eq!(
                generation,
                if case == ManagedInstallCase::CurrentStartup {
                    2
                } else {
                    1
                }
            );
        }
        ManagedInstallCase::ShutdownReady => {
            assert!(matches!(
                result.unwrap(),
                Err(CoreSessionError::Update(CoreUpdateError::Cancelled))
            ));
            assert!(!snapshot.running);
            assert_eq!(
                runs.lines().filter(|line| *line == "old").count(),
                1,
                "shutdown restarted the old core"
            );
            assert_eq!(executable, old);
        }
        ManagedInstallCase::RejectConfig
        | ManagedInstallCase::ReadyFailure
        | ManagedInstallCase::ReadyFailureStopped
        | ManagedInstallCase::VersionMismatch => {
            assert!(result.unwrap().is_err());
            assert_eq!(
                snapshot.running,
                case != ManagedInstallCase::ReadyFailureStopped
            );
            if case == ManagedInstallCase::ReadyFailureStopped {
                assert_eq!(phase, crate::CoreLifecyclePhase::Stopped);
            }
            assert_eq!(executable, old);
            assert_eq!(
                old_startup, previous_payload,
                "rollback lost the exact startup payload"
            );
            if case == ManagedInstallCase::RejectConfig {
                assert_eq!(snapshot.pid, previous_pid);
                assert_eq!(generation, 0);
            } else {
                assert_eq!(generation, 1);
            }
        }
    }
    if case == ManagedInstallCase::CurrentStartup {
        assert!(String::from_utf8(payload).unwrap().contains("8012"));
    } else {
        assert_eq!(payload, previous_payload);
    }
    if case == ManagedInstallCase::SuccessSymlink {
        assert!(
            std::fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        std::fs::remove_dir_all(binary_directory).unwrap();
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn external_and_experimental_sessions_reject_release_installation_before_download() {
    use crate::{CoreKind, CoreSession, CoreSessionError, MihomoClient, MihomoEndpoint};
    let service = MihomoReleaseService::with_base("http://127.0.0.1:1/", true).unwrap();
    let release = super::MihomoRelease {
        tag: "v9.9.9".into(),
        published_at: String::new(),
        prerelease: false,
        asset: super::MihomoReleaseAsset {
            name: platform_asset_name("v9.9.9").unwrap(),
            download_url: "http://127.0.0.1:1/asset.gz".parse().unwrap(),
            size: 1,
            sha256: "00".repeat(32),
        },
    };
    for kind in [CoreKind::Mihomo, CoreKind::Meow] {
        let session = CoreSession::open(
            kind,
            MihomoClient::new(MihomoEndpoint::new("http://127.0.0.1:1", ""))
                .unwrap()
                .with_core_kind(kind)
                .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            session.install_release(&service, &release).await,
            Err(CoreSessionError::ExternalRestartUnsupported { .. })
                | Err(CoreSessionError::ReleaseUnsupported { .. })
        ));
    }
}
