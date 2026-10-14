use super::*;

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_previous_launch_conflict_does_not_reject_a_delayed_ready_restart() {
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::AsyncWriteExt;

    let root = std::env::temp_dir().join(format!(
        "zenclash-launch-diagnostics-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let binary = root.join("mihomo");
    std::fs::write(&binary, concat!(
        "#!/bin/sh\nif [ \"$1\" = '-t' ]; then exit 0; fi\n",
        "cd \"$2\" || exit 2\ncat >/dev/null\n",
        "count=0; [ ! -f launch-count ] || count=$(cat launch-count)\n",
        "count=$((count + 1)); echo \"$count\" > launch-count\n",
        "if [ \"$count\" = 1 ]; then echo 'External controller listen error: address already in use'; else echo 'fresh restart output'; fi\n",
        "trap 'exit 0' TERM\nwhile true; do sleep 0.05; done\n",
    )).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let profile = root.join("profile.yaml");
    std::fs::write(&profile, "rules:\n  - MATCH,DIRECT\n").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    let process = ManagedCoreProcess::spawn(CoreLaunchConfig {
        kind: CoreKind::Mihomo,
        binary,
        config_file: profile,
        home_dir: root.join("home"),
        endpoint: MihomoEndpoint::new(format!("http://{endpoint}"), ""),
        controller_override: None,
    })
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !process
            .recent_logs()
            .iter()
            .any(|line| line.contains("address already in use"))
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let first_pid = process.snapshot().pid.unwrap();
    process.restart().unwrap();
    assert_ne!(process.snapshot().pid, Some(first_pid));
    let probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let served = probes.clone();
    let server = tokio::spawn(async move {
        for status in [503, 200] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = crate::test_support::read_controller_request(&mut stream)
                .await
                .unwrap()
                .into_bytes();
            assert!(
                String::from_utf8(request)
                    .unwrap()
                    .starts_with("GET /version ")
            );
            served.fetch_add(1, Ordering::SeqCst);
            let body = if status == 200 {
                r#"{"version":"ready","meta":true}"#
            } else {
                r#"{"message":"starting"}"#
            };
            stream.write_all(format!("HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });

    let ready = process.wait_until_ready(Duration::from_secs(3)).await;
    let history = process.recent_logs();
    process.stop_async().await.unwrap();
    server.abort();
    let _ = server.await;
    std::fs::remove_dir_all(root).unwrap();

    assert!(
        ready.is_ok(),
        "old diagnostics rejected the new launch: {ready:?}"
    );
    assert_eq!(probes.load(Ordering::SeqCst), 2);
    assert!(
        history
            .iter()
            .any(|line| line.contains("address already in use"))
    );
}

#[tokio::test]
async fn a_late_previous_collector_cannot_publish_a_current_launch_failure() {
    struct DelayedOutput {
        release: Option<std::sync::mpsc::Receiver<()>>,
        bytes: std::io::Cursor<Vec<u8>>,
    }
    impl std::io::Read for DelayedOutput {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if let Some(release) = self.release.take() {
                release
                    .recv()
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
            }
            std::io::Read::read(&mut self.bytes, buffer)
        }
    }
    let logs = std::sync::Arc::new(parking_lot::RwLock::new(LaunchLogHistory::default()));
    let previous = logs.write().begin_launch();
    let (release, receiver) = std::sync::mpsc::channel();
    collect_output(
        DelayedOutput {
            release: Some(receiver),
            bytes: std::io::Cursor::new(
                b"External controller listen error: address already in use\n".to_vec(),
            ),
        },
        "CORE",
        logs.clone(),
        previous,
    )
    .unwrap();
    logs.write().begin_launch();

    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while logs.read().recent_messages().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();

    let logs = logs.read();
    assert!(logs.current_listener_error().is_none());
    assert!(logs.current_last_line().is_none());
    assert_eq!(
        logs.recent_messages(),
        ["[CORE] External controller listen error: address already in use"]
    );
}
