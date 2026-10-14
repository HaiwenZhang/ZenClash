//! Ordinary isolated process and readiness responder; no scenario assertions.
use super::read_controller_request;
use crate::{CoreKind, CoreLaunchConfig, ManagedCoreProcess, MihomoEndpoint};
use std::{path::PathBuf, process::Command, sync::Arc, time::Duration};
use tokio::{io::AsyncWriteExt, task::JoinHandle};
pub(crate) struct OwnedCoreFixture {
    pub(crate) process: Arc<ManagedCoreProcess>,
    pub(in crate::core_session) directory: PathBuf,
    pub(in crate::core_session) responder: JoinHandle<()>,
}

impl OwnedCoreFixture {
    pub(crate) fn abort_responder(&self) {
        self.responder.abort();
    }

    pub(crate) async fn new(label: &str) -> Self {
        Self::with_config_path(label, "profile.yaml").await
    }

    pub(crate) async fn with_config_path(label: &str, relative: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "zenclash-current-child-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(directory.join("home")).unwrap();
        let directory = std::fs::canonicalize(directory).unwrap();
        let source = directory.join("child.rs");
        std::fs::write(
            &source,
            r#"fn main() {
            if std::env::args().any(|arg| arg == "-t") { return; }
            std::thread::sleep(std::time::Duration::from_secs(30));
        }"#,
        )
        .unwrap();
        let binary = directory.join(if cfg!(windows) {
            "mihomo.exe"
        } else {
            "mihomo"
        });
        let compilation = Command::new("rustc")
            .arg("--edition=2024")
            .arg(source)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compilation.status.success(),
            "{}",
            String::from_utf8_lossy(&compilation.stderr)
        );
        let config = directory.join(relative);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "rules:\n  - MATCH,DIRECT\n").unwrap();
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let responder = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                match read_controller_request(&mut stream).await {
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => continue,
                    Err(error) => panic!("owned controller request failed: {error}"),
                }
                let body = r#"{"meta":true,"version":"owned-child-fixture"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let process = ManagedCoreProcess::spawn(CoreLaunchConfig {
            kind: CoreKind::Mihomo,
            binary,
            config_file: config,
            home_dir: directory.join("home"),
            endpoint: MihomoEndpoint::new(format!("http://{address}"), ""),
            controller_override: None,
        })
        .unwrap();
        Self {
            process,
            directory,
            responder,
        }
    }
}

impl Drop for OwnedCoreFixture {
    fn drop(&mut self) {
        self.responder.abort();
        self.process.stop().unwrap();
        for attempt in 0..20 {
            match std::fs::remove_dir_all(&self.directory) {
                Ok(()) => return,
                Err(error) if attempt == 19 => panic!("child fixture cleanup failed: {error}"),
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}
