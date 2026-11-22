use super::*;

#[tokio::test]
async fn download_metadata_preserves_missing_headers_and_keeps_yaml_edits_separate() {
    let root = test_root("download-metadata-policy");
    let store = ProfileStore::new(root.join("store")).unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let url = format!("http://{}/profile.yaml", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let replies = [
            (
                "Subscription-Userinfo: upload=10; download=20; total=1000\r\nProfile-Web-Page-Url: https://example.com/account\r\nProfile-Update-Interval: 6\r\n",
                "rules: ['MATCH,CREATE']\n",
            ),
            (
                "Subscription-Userinfo: upload=100; download=200; total=2000\r\n",
                "rules: ['MATCH,UPDATED']\n",
            ),
            ("Profile-Update-Interval: 2\r\n", "rules: ['MATCH,FINAL']\n"),
        ];
        for (headers, payload) in replies {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("GET /profile.yaml "));
            assert!(request.contains("authorization: Bearer metadata-secret\r\n"));
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/yaml\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{payload}", payload.len()).unwrap();
        }
    });
    let options = RemoteProfileOptions::new("Bearer metadata-secret", false)
        .unwrap()
        .with_route(RemoteProfileRoute::Direct);
    let record = store
        .add_remote_with_options("Remote", &url, "ZenClash-Test", options.clone(), None)
        .await
        .unwrap();
    assert_eq!(record.update_interval_minutes, 360);
    assert!(!record.auto_update);
    assert_eq!(record.size_bytes, "rules: ['MATCH,CREATE']\n".len() as u64);
    store.set_update_policy(&record.id, true, 45).unwrap();
    store
        .set_remote_request_settings(
            &record.id,
            "Remote",
            &url,
            "ZenClash-Test",
            options,
            Some("0 */3 * * *".into()),
        )
        .unwrap();
    let expected = store.load().unwrap().profiles[0].clone();

    let update = store.update_remote(&record.id).await.unwrap();
    assert_eq!(
        update.record.subscription.usage.as_ref().unwrap().used(),
        300
    );
    assert_eq!(
        update.record.subscription.home_url,
        expected.subscription.home_url
    );
    assert_eq!(
        update.record.subscription.suggested_update_interval_minutes,
        Some(360)
    );
    assert_eq!(update.record.update_interval_minutes, 360);
    assert_eq!(update.record.update_cron, expected.update_cron);
    assert!(update.record.auto_update);
    assert_eq!(update.record.source, expected.source);
    assert_eq!(
        update.record.size_bytes,
        "rules: ['MATCH,UPDATED']\n".len() as u64
    );
    assert!(update.record.updated_at >= expected.updated_at);
    assert!(update.record.updated_at <= unix_timestamp());

    let final_update = store.update_remote(&record.id).await.unwrap();
    server.join().unwrap();
    assert_eq!(
        final_update.record.subscription.usage,
        update.record.subscription.usage
    );
    assert_eq!(
        final_update.record.subscription.home_url,
        expected.subscription.home_url
    );
    assert_eq!(
        final_update
            .record
            .subscription
            .suggested_update_interval_minutes,
        Some(120)
    );
    assert_eq!(final_update.record.update_interval_minutes, 120);
    assert_eq!(final_update.record.update_cron, expected.update_cron);
    assert!(final_update.record.auto_update);
    assert_eq!(final_update.record.id, record.id);
    assert_eq!(final_update.record.file_name, record.file_name);

    store.set_update_policy(&record.id, false, 45).unwrap();
    let edit = store
        .replace_payload(
            &record.id,
            "rules: ['MATCH,FINAL']\n",
            "rules: ['MATCH,EDITED']\n",
        )
        .unwrap();
    assert_eq!(edit.record.subscription, final_update.record.subscription);
    assert_eq!(edit.record.update_interval_minutes, 45);
    assert!(!edit.record.auto_update);
    assert_eq!(
        edit.record.size_bytes,
        "rules: ['MATCH,EDITED']\n".len() as u64
    );
    assert_eq!(store.load().unwrap().profiles[0], edit.record);
    fs::remove_dir_all(root).unwrap();
}
