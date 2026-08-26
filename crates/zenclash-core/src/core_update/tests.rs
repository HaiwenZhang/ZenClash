use std::{
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use flate2::{write::GzEncoder, Compression};
use sha2::{Digest, Sha256};

use super::{
    service::{
        parse_digest, platform_asset_name, validate_asset_url, verify_sha256, RawAsset, RawRelease,
    },
    transaction::{sibling_path, PreparedCoreUpdate},
    workflow::versions_match,
    CoreUpdateError, MihomoReleaseService,
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
    assert!(String::from_utf8_lossy(
        &std::process::Command::new(&target)
            .arg("-v")
            .output()
            .unwrap()
            .stdout
    )
    .contains("Mihomo Meta v9.9.9"));
    transaction.rollback().unwrap();
    assert!(
        String::from_utf8_lossy(&std::process::Command::new(&target).output().unwrap().stdout)
            .contains("old core")
    );

    server.join().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
