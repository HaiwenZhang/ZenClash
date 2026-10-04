//! Real Mihomo IPC coverage; this does not install or launch a protected service.

use std::{
    fs,
    io::{self, Read},
    net::SocketAddr,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, FileTypeExt};
#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpSocket, TcpStream},
};

use super::{Kernel, KernelSubscription, NativeController};
use crate::protocol::{ApiRequest, LogStreamOptions, ServiceLogFormat, ServiceLogLevel};

const DEADLINE: Duration = Duration::from_secs(10);

struct OrdinaryMihomo {
    child: Option<Child>,
    directory: PathBuf,
}

impl OrdinaryMihomo {
    fn diagnostics(&self, kernel: &Kernel) -> String {
        let mut output = Vec::new();
        let result = fs::File::open(self.directory.join("process-output.txt"))
            .and_then(|file| file.take(8192).read_to_end(&mut output));
        if result.is_err() {
            return "bounded child diagnostics unavailable".into();
        }
        String::from_utf8_lossy(&output)
            .replace(&kernel.secret, "[redacted]")
            .replace(
                kernel.controller.to_string_lossy().as_ref(),
                "[private-controller]",
            )
            .replace(
                self.directory.to_string_lossy().as_ref(),
                "[test-directory]",
            )
    }

    fn stop(&mut self) -> io::Result<()> {
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        if child.try_wait()?.is_none() {
            child.kill()?;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_wait()?.is_some() {
                self.child = None;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Mihomo did not exit",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for OrdinaryMihomo {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("ordinary Mihomo cleanup failed: {error}");
            return;
        }
        if let Err(error) = fs::remove_dir_all(&self.directory) {
            eprintln!("ordinary Mihomo temporary directory cleanup failed: {error}");
        }
    }
}

fn descriptor(pid: u32, controller: PathBuf, secret: String) -> Kernel {
    // The fixture owns the real ordinary child separately. All controller I/O,
    // including the same-handle native PID check, uses production methods.
    Kernel {
        child: None,
        execution: None,
        pid,
        client: NativeController::new(controller.clone(), pid, secret.clone()),
        secret,
        controller,
        logs: Default::default(),
        readers: Vec::new(),
        exit_reason: None,
        fixture_status: None,
    }
}

async fn delay(kernel: &Kernel, target: SocketAddr) -> io::Result<crate::protocol::ApiResponse> {
    kernel
        .api(&ApiRequest {
            method: "GET".into(),
            path: format!("/proxies/loopback/delay?url=http://{target}/&timeout=1500"),
            body: None,
        })
        .await
        .map_err(|error| io::Error::other(format!("real delay API failed: {error:?}")))
}

async fn event_for(
    events: &mut KernelSubscription,
    field: &str,
    target: SocketAddr,
) -> io::Result<Value> {
    tokio::time::timeout(DEADLINE, async {
        for _ in 0..64 {
            let event = events.next().await?.ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "real log stream closed")
            })?;
            if event[field]
                .as_str()
                .is_some_and(|message| message.contains(&target.to_string()))
            {
                return Ok(event);
            }
        }
        Err(io::Error::other(
            "real log event search exceeded its budget",
        ))
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("real log event timed out: field={field}, target={target}"),
        )
    })?
}

async fn successful_delay(kernel: &Kernel) -> io::Result<SocketAddr> {
    let server = TcpListener::bind("127.0.0.1:0").await?;
    let target = server.local_addr()?;
    // This is only an HTTP destination; Mihomo generates the log and carries
    // the request through its real inbound listener and DIRECT tunnel.
    let destination = async {
        let (mut connection, _) = server.accept().await?;
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let byte = connection.read_u8().await?;
            request.push(byte);
            if request.len() > 8192 {
                return Err(io::Error::other(
                    "loopback HEAD request exceeded its budget",
                ));
            }
        }
        assert!(request.starts_with(b"HEAD / HTTP/1.1\r\n"));
        tokio::time::sleep(Duration::from_millis(20)).await;
        connection
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
    };
    let response = tokio::time::timeout(DEADLINE, async {
        let (response, ()) = tokio::try_join!(delay(kernel, target), destination)?;
        Ok::<_, io::Error>(response)
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "real loopback delay HTTP destination/API timed out",
        )
    })??;
    assert_eq!(
        response.status, 200,
        "loopback delay failed: {}",
        response.body
    );
    assert!(
        response.body["delay"]
            .as_u64()
            .is_some_and(|delay| delay > 0)
    );
    Ok(target)
}

fn assert_structured_info(event: &Value) {
    let object = event.as_object().expect("structured log must be an object");
    assert_eq!(object.len(), 4, "unexpected structured shape: {event}");
    assert_eq!(event["level"], "info");
    assert!(event["message"].as_str().unwrap().starts_with("[TCP]"));
    assert_eq!(event["fields"], json!([]));
    let time = event["time"].as_str().unwrap().as_bytes();
    assert_eq!(time.len(), 8);
    assert_eq!((time[2], time[5]), (b':', b':'));
    assert!(
        time.iter()
            .enumerate()
            .all(|(index, byte)| { matches!(index, 2 | 5) || byte.is_ascii_digit() })
    );
}

async fn start_fixture() -> io::Result<(OrdinaryMihomo, Kernel, SocketAddr)> {
    let binary = PathBuf::from(std::env::var_os("ZENCLASH_MIHOMO_BINARY").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "ZENCLASH_MIHOMO_BINARY is required",
        )
    })?);
    if !fs::metadata(&binary)?.is_file() {
        return Err(io::Error::other(
            "ZENCLASH_MIHOMO_BINARY must be a real executable file",
        ));
    }
    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce).map_err(|error| io::Error::other(error.to_string()))?;
    let unique: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    #[cfg(windows)]
    let directory = std::env::temp_dir().join(format!("zenclash-real-logs-{unique}"));
    #[cfg(unix)]
    let directory = PathBuf::from("/tmp").join(format!("zc-log-{unique}"));
    #[cfg(windows)]
    fs::create_dir(&directory)?;
    #[cfg(unix)]
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let mut fixture = OrdinaryMihomo {
        child: None,
        directory,
    };
    let home = fixture.directory.join("home");
    fs::create_dir(&home)?;
    #[cfg(windows)]
    let controller = PathBuf::from(format!(r"\\.\pipe\zenclash-real-logs-{unique}"));
    #[cfg(unix)]
    let controller = fixture.directory.join("c.sock");
    #[cfg(windows)]
    let (pipe, socket) = (controller.to_string_lossy().into_owned(), String::new());
    #[cfg(unix)]
    let (pipe, socket) = (String::new(), controller.to_string_lossy().into_owned());
    let secret = format!("log-test-{unique}");
    let reservation = TcpListener::bind("127.0.0.1:0").await?;
    let inbound = reservation.local_addr()?;
    let config = fixture.directory.join("runtime.yaml");
    fs::write(
        &config,
        serde_json::to_vec(&json!({
            "mixed-port": 0, "port": 0, "socks-port": 0,
            "redir-port": 0, "tproxy-port": 0, "allow-lan": false,
            "mode": "direct", "log-level": "debug", "ipv6": false,
            "geo-auto-update": false, "tun": {"enable": false},
            "dns": {"enable": false}, "external-controller": "",
            "external-controller-tls": "", "external-controller-pipe": pipe,
            "external-controller-unix": socket,
            "secret": secret,
            "profile": {"store-selected": false, "store-fake-ip": false},
            "listeners": [{"name": "loopback-inbound", "type": "http",
                           "listen": "127.0.0.1", "port": inbound.port().to_string()}],
            "proxies": [{"name": "loopback", "type": "http",
                         "server": "127.0.0.1", "port": inbound.port()}],
            "rules": ["MATCH,DIRECT"]
        }))?,
    )?;
    drop(reservation);
    let output = fs::File::create(fixture.directory.join("process-output.txt"))?;
    let mut command = Command::new(&binary);
    command
        .arg("-d")
        .arg(&home)
        .arg("-f")
        .arg(&config)
        .env_clear();
    #[cfg(windows)]
    command
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .env(
            "SYSTEMROOT",
            std::env::var_os("SYSTEMROOT")
                .ok_or_else(|| io::Error::other("Windows SYSTEMROOT is unavailable"))?,
        );
    fixture.child = Some(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::from(output.try_clone()?))
            .stderr(Stdio::from(output))
            .spawn()?,
    );
    let pid = fixture.child.as_ref().unwrap().id();
    let kernel = descriptor(pid, controller.clone(), secret.clone());
    Ok((fixture, kernel, inbound))
}

async fn verify_real_logs() -> io::Result<()> {
    let (mut fixture, kernel, inbound) = start_fixture().await?;
    let pid = kernel.pid;
    let mut readiness = "no native observation".to_owned();
    tokio::time::timeout(DEADLINE, async {
        loop {
            if fixture.child.as_mut().unwrap().try_wait()?.is_some() {
                return Err(io::Error::other(
                    "ordinary Mihomo exited before IPC readiness",
                ));
            }
            let api = kernel
                .api(&ApiRequest {
                    method: "GET".into(),
                    path: "/version".into(),
                    body: None,
                })
                .await;
            match api {
                Ok(response) if response.status == 200 => {
                    if TcpStream::connect(inbound).await.is_ok() {
                        return Ok::<(), io::Error>(());
                    }
                    readiness = "native controller ready; loopback inbound pending".into();
                }
                Ok(response) => {
                    readiness = format!("native version HTTP status={}", response.status)
                }
                Err(error) => readiness = format!("native version request={error:?}"),
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "real native IPC readiness timed out: {readiness}; {}",
                fixture.diagnostics(&kernel)
            ),
        )
    })??;

    let wrong_peer = descriptor(
        pid.wrapping_add(1),
        kernel.controller.clone(),
        kernel.secret.clone(),
    );
    assert_eq!(
        wrong_peer
            .subscribe_logs(LogStreamOptions::default())
            .await
            .err()
            .expect("wrong native server PID must be rejected")
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    let mut info = kernel.subscribe_logs(LogStreamOptions::default()).await?;
    let mut warnings = kernel
        .subscribe_logs(LogStreamOptions {
            level: ServiceLogLevel::Warning,
            format: ServiceLogFormat::Plain,
        })
        .await?;
    assert_eq!((info.pid, warnings.pid), (pid, pid));
    let target = successful_delay(&kernel).await?;
    assert_structured_info(&event_for(&mut info, "message", target).await?);
    assert!(
        tokio::time::timeout(Duration::from_millis(250), warnings.next())
            .await
            .is_err(),
        "warning subscription incorrectly received the real successful TCP info event"
    );

    // Keep the socket bound and non-listening throughout the assertion instead
    // of releasing the reservation before Mihomo attempts its loopback dial.
    let closed = TcpSocket::new_v4()?;
    closed.bind("127.0.0.1:0".parse().unwrap())?;
    let refused = closed.local_addr()?;
    assert!(delay(&kernel, refused).await?.status >= 400);
    let warning = event_for(&mut warnings, "payload", refused).await?;
    assert_eq!(
        warning.as_object().unwrap().len(),
        2,
        "unexpected plain shape: {warning}"
    );
    assert_eq!(warning["type"], "warning");
    assert!(
        warning["payload"]
            .as_str()
            .unwrap()
            .starts_with("[TCP] dial")
    );
    assert!(warning["payload"].as_str().unwrap().contains("error:"));
    assert!(!warning.to_string().contains(&kernel.secret));

    drop(info);
    drop(warnings);
    let mut resubscribed = kernel.subscribe_logs(LogStreamOptions::default()).await?;
    let target = successful_delay(&kernel).await?;
    assert_structured_info(&event_for(&mut resubscribed, "message", target).await?);
    drop(resubscribed);
    fixture.stop()?;
    assert!(
        fixture.child.is_none(),
        "real Mihomo must be reaped before completion"
    );
    Ok(())
}

#[cfg(windows)]
#[tokio::test]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY"]
async fn real_mihomo_pipe_logs_preserve_options_filter_warning_and_cancel_cleanly() -> io::Result<()>
{
    verify_real_logs().await
}

#[cfg(unix)]
fn require_unix_identity(root: bool) -> io::Result<()> {
    // SAFETY: geteuid has no arguments or pointer preconditions.
    let uid = unsafe { libc::geteuid() };
    if (uid == 0) != root {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            if root {
                "positive native log validation requires a UID 0 test runner; no elevation is performed"
            } else {
                "negative native log validation requires a non-root test runner"
            },
        ));
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY and a UID 0 runner; does not elevate"]
async fn real_root_mihomo_socket_logs_preserve_options_filter_warning_and_cancel_cleanly()
-> io::Result<()> {
    require_unix_identity(true)?;
    verify_real_logs().await
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY and a non-root runner"]
async fn real_nonroot_mihomo_socket_is_rejected_before_log_subscription() -> io::Result<()> {
    require_unix_identity(false)?;
    let (mut fixture, kernel, _) = start_fixture().await?;
    let stream = tokio::time::timeout(DEADLINE, async {
        loop {
            if fixture.child.as_mut().unwrap().try_wait()?.is_some() {
                return Err(io::Error::other(
                    "real Mihomo exited before socket readiness",
                ));
            }
            if let Ok(stream) = tokio::net::UnixStream::connect(&kernel.controller).await {
                return Ok::<_, io::Error>(stream);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "real non-root socket readiness timed out; {}",
                fixture.diagnostics(&kernel)
            ),
        )
    })??;
    assert!(
        fs::symlink_metadata(&kernel.controller)?
            .file_type()
            .is_socket()
    );
    let peer = stream.peer_cred()?;
    // SAFETY: geteuid has no arguments or pointer preconditions.
    assert_eq!(peer.uid(), unsafe { libc::geteuid() });
    assert_ne!(
        peer.uid(),
        0,
        "negative fixture must have a real non-root peer"
    );
    #[cfg(target_os = "linux")]
    assert_eq!(peer.pid(), Some(i32::try_from(kernel.pid).unwrap()));
    assert_eq!(
        crate::platform::verify_kernel_peer(&stream, kernel.pid)
            .expect_err("production authentication must reject a non-root Mihomo")
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    drop(stream);
    let error = kernel
        .client
        .request("GET", "/version", None, DEADLINE)
        .await
        .err()
        .expect("native HTTP must reject a non-root Mihomo before sending");
    let super::NativeHttpError::BeforeSend(error) = error else {
        return Err(io::Error::other(
            "non-root HTTP rejection occurred after sending",
        ));
    };
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        kernel
            .subscribe_logs(LogStreamOptions::default())
            .await
            .err()
            .expect("production log subscription must reject a non-root Mihomo")
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    fixture.stop()?;
    assert!(
        fixture.child.is_none(),
        "real non-root Mihomo must be reaped"
    );
    Ok(())
}
