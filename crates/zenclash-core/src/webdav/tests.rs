use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use super::*;
use crate::{
    profiles::atomic_write, AppPreferences, AppPreferencesStore, AppearancePreference,
    BackupManager, ControlledConfigStore, ProfileStore,
};

const PROFILE: &str = "mixed-port: 7890\nproxies: []\nproxy-groups: []\nrules: []\n";
static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[test]
fn settings_debug_redacts_the_password() {
    let settings = WebDavSettings {
        password: "top-secret".into(),
        ..WebDavSettings::default()
    };

    let debug = format!("{settings:?}");

    assert!(
        !debug.contains("top-secret"),
        "debug leaked credentials: {debug}"
    );
}

#[test]
fn service_rejects_credentials_embedded_in_the_url() {
    let settings = WebDavSettings {
        url: "https://user:password@example.com/dav".into(),
        ..WebDavSettings::default()
    };

    let error = WebDavService::new(settings).unwrap_err();

    assert!(
        error.to_string().contains("凭据"),
        "unexpected error: {error}"
    );
}

#[test]
fn service_rejects_basic_credentials_over_remote_plain_http() {
    let settings = WebDavSettings {
        url: "http://dav.example.com/root".into(),
        username: "alice".into(),
        password: "secret".into(),
        ..WebDavSettings::default()
    };

    let error = WebDavService::new(settings).unwrap_err();

    assert!(
        error.to_string().contains("https"),
        "unexpected error: {error}"
    );
}

#[test]
fn five_field_cron_is_normalized_and_empty_cron_is_disabled() {
    let disabled = WebDavSettings::default();
    assert_eq!(disabled.next_backup_after(1_700_000_000).unwrap(), None);

    let settings = WebDavSettings {
        backup_cron: "30 3 * * *".into(),
        ..WebDavSettings::default()
    };
    let next = settings.next_backup_after(1_700_000_000).unwrap().unwrap();

    assert!(next > 1_700_000_000);
}

#[test]
fn malformed_cron_is_rejected_before_network_access() {
    let settings = WebDavSettings {
        url: "https://dav.example.com/root".into(),
        backup_cron: "every morning".into(),
        ..WebDavSettings::default()
    };

    let error = WebDavService::new(settings).unwrap_err();

    assert!(error.to_string().contains("Cron"));
}

#[test]
fn settings_store_round_trips_credentials_in_a_private_file() {
    let root = test_root("settings");
    let store = WebDavSettingsStore::new(root.join("webdav.json"));
    let settings = WebDavSettings {
        url: "https://example.com/dav".into(),
        username: "alice".into(),
        password: "secret".into(),
        max_backups: 5,
        ..WebDavSettings::default()
    };

    store.save(&settings).unwrap();
    let loaded = store.load().unwrap();

    assert_eq!(loaded, settings);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn webdav_round_trip_uses_real_methods_retention_and_transactional_restore() {
    let server = TestWebDavServer::start();
    let root = test_root("roundtrip");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, 17_890);
    create_snapshot(&target, AppearancePreference::Dark, 17_891);
    let settings = WebDavSettings {
        url: format!("http://{}/dav", server.address),
        directory: "nested/zenclash".into(),
        username: "alice".into(),
        password: "secret".into(),
        max_backups: 1,
        ..WebDavSettings::default()
    };
    let service = WebDavService::new(settings).unwrap();

    service
        .upload_snapshot(&BackupManager::new(&source))
        .await
        .unwrap();
    let second = service
        .upload_snapshot(&BackupManager::new(&source))
        .await
        .unwrap();
    let backups = service.list_backups().await.unwrap();
    let prepared = service
        .prepare_restore(&BackupManager::new(&target), &backups[0].filename)
        .await
        .unwrap();
    prepared.activate().unwrap().commit().unwrap();
    service.delete_backup(&backups[0].filename).await.unwrap();

    let restored = AppPreferencesStore::new(target.join("preferences.json"))
        .load()
        .unwrap();
    assert_eq!(second.removed_backups, 1);
    assert_eq!(backups.len(), 1);
    assert_eq!(restored.appearance, AppearancePreference::Light);
    assert!(service.list_backups().await.unwrap().is_empty());
    assert!(server.methods().iter().any(|method| method == "PROPFIND"));
    assert!(server.methods().iter().any(|method| method == "MKCOL"));
    assert!(server.methods().iter().any(|method| method == "PUT"));
    assert!(server.methods().iter().any(|method| method == "GET"));
    assert!(server.methods().iter().any(|method| method == "DELETE"));
    fs::remove_dir_all(root).unwrap();
}

fn create_snapshot(root: &Path, appearance: AppearancePreference, port: u16) {
    fs::create_dir_all(root).unwrap();
    AppPreferencesStore::new(root.join("preferences.json"))
        .save(&AppPreferences {
            appearance,
            ..AppPreferences::default()
        })
        .unwrap();
    atomic_write(
        &root.join("controlled-config/override.yaml"),
        format!("mixed-port: {port}\n").as_bytes(),
    )
    .unwrap();
    let source = root.join("import.yaml");
    fs::write(&source, PROFILE).unwrap();
    let profiles = ProfileStore::new(root.join("profiles")).unwrap();
    let record = profiles.import_local(source).unwrap();
    profiles.activate(&record.id).unwrap();
    ControlledConfigStore::new(root.join("controlled-config"))
        .materialize(profiles.active_path().unwrap().unwrap())
        .unwrap();
}

struct TestWebDavServer {
    address: std::net::SocketAddr,
    shutdown: Arc<AtomicBool>,
    methods: Arc<Mutex<Vec<String>>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl TestWebDavServer {
    fn start() -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let files = Arc::new(Mutex::new(HashMap::new()));
        let methods = Arc::new(Mutex::new(Vec::new()));
        let worker_shutdown = shutdown.clone();
        let worker_files = files;
        let worker_methods = methods.clone();
        let thread = thread::spawn(move || {
            while !worker_shutdown.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((_stream, _)) if worker_shutdown.load(Ordering::Acquire) => break,
                    Ok((stream, _)) => handle_connection(stream, &worker_files, &worker_methods),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("test WebDAV accept failed: {error}"),
                }
            }
        });
        Self {
            address,
            shutdown,
            methods,
            thread: Some(thread),
        }
    }

    fn methods(&self) -> Vec<String> {
        self.methods.lock().unwrap().clone()
    }
}

impl Drop for TestWebDavServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    files: &Mutex<HashMap<String, Vec<u8>>>,
    methods: &Mutex<Vec<String>>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let request = read_request(&mut stream);
    let header_end = find_bytes(&request, b"\r\n\r\n").unwrap();
    let headers = std::str::from_utf8(&request[..header_end]).unwrap();
    let mut request_line = headers.lines().next().unwrap().split_whitespace();
    let method = request_line.next().unwrap();
    let path = request_line.next().unwrap();
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("authorization: basic "),
        "missing Basic authentication: {headers}"
    );
    methods.lock().unwrap().push(method.into());
    let body = &request[header_end + 4..];
    let (status, content_type, response_body) = match method {
        "MKCOL" => ("201 Created", "text/plain", Vec::new()),
        "PUT" => {
            files.lock().unwrap().insert(path.into(), body.to_vec());
            ("201 Created", "text/plain", Vec::new())
        }
        "PROPFIND" => (
            "207 Multi-Status",
            "application/xml",
            directory_xml(path, &files.lock().unwrap()).into_bytes(),
        ),
        "GET" => files.lock().unwrap().get(path).map_or_else(
            || ("404 Not Found", "text/plain", b"missing".to_vec()),
            |bytes| ("200 OK", "application/zip", bytes.clone()),
        ),
        "DELETE" => {
            files.lock().unwrap().remove(path);
            ("204 No Content", "text/plain", Vec::new())
        }
        _ => ("405 Method Not Allowed", "text/plain", Vec::new()),
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response_body.len()
    )
    .unwrap();
    stream.write_all(&response_body).unwrap();
}

fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 8_192];
    let mut expected = None;
    loop {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0, "connection closed before request completed");
        request.extend_from_slice(&buffer[..read]);
        if expected.is_none() {
            if let Some(header_end) = find_bytes(&request, b"\r\n\r\n") {
                let headers = std::str::from_utf8(&request[..header_end]).unwrap();
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':').and_then(|(name, value)| {
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                    })
                    .unwrap_or(0);
                expected = Some(header_end + 4 + content_length);
            }
        }
        if expected.is_some_and(|expected| request.len() >= expected) {
            return request;
        }
    }
}

fn directory_xml(directory: &str, files: &HashMap<String, Vec<u8>>) -> String {
    use std::fmt::Write as _;

    let entries = files.iter().filter(|(path, _)| path.starts_with(directory)).fold(
        String::new(),
        |mut entries, (path, bytes)| {
            write!(
                entries,
                "<d:response><d:href>{path}</d:href><d:propstat><d:prop><d:getcontentlength>{}</d:getcontentlength><d:getlastmodified>Wed, 27 Aug 2026 00:00:00 GMT</d:getlastmodified><d:resourcetype/></d:prop></d:propstat></d:response>",
                bytes.len()
            )
            .unwrap();
            entries
        },
    );
    format!(
        "<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>{directory}/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>{entries}</d:multistatus>"
    )
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn test_root(name: &str) -> PathBuf {
    let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "zenclash-webdav-{name}-{}-{sequence}",
        std::process::id()
    ))
}
