use super::*;
use std::{
    future::Future,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    task::{Context, Poll},
};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};

fn occupied_pipe() -> (String, NamedPipeServer, NamedPipeClient) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let name = format!(
        r"\\.\pipe\ZenClash.LogBusy.{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .max_instances(1)
        .create(&name)
        .unwrap();
    let occupant = ClientOptions::new().open(&name).unwrap();
    assert_eq!(
        ClientOptions::new().open(&name).unwrap_err().raw_os_error(),
        Some(231)
    );
    (name, server, occupant)
}

fn release_and_recreate(
    name: &str,
    server: NamedPipeServer,
    occupant: NamedPipeClient,
) -> NamedPipeServer {
    // Disconnect alone leaves a non-listening instance. Match Mihomo's
    // accept loop by closing the old instance and creating its successor.
    drop(occupant);
    drop(server);
    ServerOptions::new()
        .first_pipe_instance(true)
        .max_instances(1)
        .create(name)
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn actual_busy_pipe_reopens_after_release_and_verifies_the_same_handle() {
    let (name, server, occupant) = occupied_pipe();
    let attempts = AtomicUsize::new(0);
    let mut opening = Box::pin(tokio::time::timeout(
        SUBSCRIPTION_TIMEOUT,
        open_kernel_pipe(|| {
            attempts.fetch_add(1, Ordering::Relaxed);
            ClientOptions::new().open(&name)
        }),
    ));
    let waker = futures_util::task::noop_waker();
    assert!(
        opening
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    assert_eq!(attempts.load(Ordering::Relaxed), 1);
    let _successor = release_and_recreate(&name, server, occupant);
    tokio::time::advance(Duration::from_millis(10)).await;
    let pipe = opening.await.unwrap().unwrap();
    verify_kernel_pipe(&pipe, std::process::id()).unwrap();
    assert_eq!(attempts.load(Ordering::Relaxed), 2);
}

#[tokio::test(start_paused = true)]
async fn actual_busy_pipe_expires_at_the_subscription_deadline_without_a_handle() {
    let (name, _server, _occupant) = occupied_pipe();
    let mut opening = Box::pin(tokio::time::timeout(
        SUBSCRIPTION_TIMEOUT,
        open_kernel_pipe(|| ClientOptions::new().open(&name)),
    ));
    let waker = futures_util::task::noop_waker();
    assert!(
        opening
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    tokio::time::advance(SUBSCRIPTION_TIMEOUT).await;
    assert!(opening.await.is_err());
}

#[tokio::test(start_paused = true)]
async fn cancelling_busy_open_never_connects_after_the_instance_is_released() {
    let (name, server, occupant) = occupied_pipe();
    let attempts = AtomicUsize::new(0);
    let mut opening = Box::pin(open_kernel_pipe(|| {
        attempts.fetch_add(1, Ordering::Relaxed);
        ClientOptions::new().open(&name)
    }));
    let waker = futures_util::task::noop_waker();
    assert!(matches!(
        opening.as_mut().poll(&mut Context::from_waker(&waker)),
        Poll::Pending
    ));
    drop(opening);
    let _successor = release_and_recreate(&name, server, occupant);
    tokio::time::advance(SUBSCRIPTION_TIMEOUT).await;
    assert_eq!(attempts.load(Ordering::Relaxed), 1);
    // No detached retry consumed the only available instance.
    let pipe = ClientOptions::new().open(&name).unwrap();
    verify_kernel_pipe(&pipe, std::process::id()).unwrap();
}

#[tokio::test(start_paused = true)]
async fn non_busy_native_errors_are_returned_once_without_waiting() {
    for code in [2, 5, 1224] {
        let attempts = AtomicUsize::new(0);
        let before = tokio::time::Instant::now();
        let result = open_kernel_pipe(|| {
            attempts.fetch_add(1, Ordering::Relaxed);
            Err(io::Error::from_raw_os_error(code))
        })
        .await;
        assert_eq!(result.unwrap_err().raw_os_error(), Some(code));
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        assert_eq!(tokio::time::Instant::now(), before);
    }
}

#[tokio::test(start_paused = true)]
async fn busy_open_and_stalled_upgrade_share_one_production_deadline() {
    use tokio::io::AsyncReadExt;
    let (name, server, occupant) = occupied_pipe();
    let controller = PathBuf::from(&name);
    let pid = std::process::id();
    let kernel = Kernel {
        child: None,
        pid,
        client: NativeController::new(controller.clone(), pid, "fixture-secret".into()),
        secret: "fixture-secret".into(),
        controller,
        logs: Default::default(),
        readers: Vec::new(),
        exit_reason: None,
        fixture_status: None,
    };
    let before = tokio::time::Instant::now();
    let mut subscription = Box::pin(kernel.subscribe_logs(LogStreamOptions::default()));
    let waker = futures_util::task::noop_waker();
    assert!(
        subscription
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    tokio::time::advance(Duration::from_millis(4990)).await;
    let mut server = release_and_recreate(&name, server, occupant);
    // The available handle is authenticated and a real upgrade begins, but
    // this peer intentionally supplies no HTTP response.
    assert!(
        subscription
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    tokio::time::timeout(Duration::from_secs(1), server.connect())
        .await
        .unwrap()
        .unwrap();
    let task = tokio::spawn(async move {
        let mut first = [0; 1];
        tokio::time::timeout(Duration::from_secs(1), server.read_exact(&mut first))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first[0], b'G');
        server
    });
    let (result, server) = tokio::join!(subscription, task);
    let _server = server.unwrap();
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::TimedOut));
    assert_eq!(tokio::time::Instant::now() - before, SUBSCRIPTION_TIMEOUT);
}
