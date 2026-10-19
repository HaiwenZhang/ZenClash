use super::*;
use crate::{CoreKind, MihomoClient, MihomoEndpoint, MihomoError};
use std::{future::Future, task::Context, time::Duration};
use tokio::{io::AsyncWriteExt, net::TcpListener};

async fn session() -> (CoreSession, TcpListener) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = MihomoClient::new(MihomoEndpoint::new(
        format!("http://{}", listener.local_addr().unwrap()),
        "",
    ))
    .unwrap();
    (
        CoreSession::open(CoreKind::Mihomo, client).unwrap(),
        listener,
    )
}

async fn delete(listener: &TcpListener, expected: &str) -> tokio::net::TcpStream {
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let request = crate::test_support::read_controller_request(&mut stream)
        .await
        .unwrap();
    assert!(request.into_parts().0.starts_with(expected));
    stream
}

#[tokio::test]
async fn connection_closure_rejects_replaced_configuration_and_runtime_before_sending() {
    for id in [None, Some("old-connection".to_owned())] {
        let (session, old) = session().await;
        let source = session.begin_observation().await.unwrap();
        let transition = session.transition.lock().await;
        let mut queued =
            Box::pin(session.close_observed_connections(source.identity(), id.clone()));
        assert!(
            queued
                .as_mut()
                .poll(&mut Context::from_waker(std::task::Waker::noop()))
                .is_pending()
        );
        session.invalidate_runtime_observations();
        drop(transition);
        assert!(matches!(
            queued.await,
            Err(CoreSessionError::Process(MihomoError::StaleBinding))
        ));

        let source = session.begin_observation().await.unwrap();
        let replacement = TcpListener::bind("127.0.0.1:0").await.unwrap();
        session
            .switch_to_direct(MihomoEndpoint::new(
                format!("http://{}", replacement.local_addr().unwrap()),
                "",
            ))
            .await
            .unwrap();
        assert!(matches!(
            session
                .close_observed_connections(source.identity(), id)
                .await,
            Err(CoreSessionError::Process(MihomoError::StaleBinding))
        ));
        for listener in [&old, &replacement] {
            assert!(
                tokio::time::timeout(Duration::from_millis(30), listener.accept())
                    .await
                    .is_err()
            );
        }
    }
}

#[tokio::test]
async fn connection_closure_keeps_original_readback_source_and_rejects_foreign_owner() {
    for id in [None, Some("one".to_owned())] {
        let (session, listener) = session().await;
        let foreign = CoreSession::open(CoreKind::Mihomo, session.client().clone()).unwrap();
        let foreign_source = foreign.begin_observation().await.unwrap();
        assert!(
            session
                .close_observed_connections(foreign_source.identity(), id.clone())
                .await
                .is_err()
        );
        let source = session.begin_observation().await.unwrap();
        let path = id.as_ref().map_or("DELETE /connections ".to_owned(), |id| {
            format!("DELETE /connections/{id} ")
        });
        let server = tokio::spawn(async move {
            delete(&listener, &path)
                .await
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });
        let readback = session
            .close_observed_connections(source.identity(), id)
            .await
            .unwrap();
        assert!(readback.identity().is_same_observation(source.identity()));
        assert!(session.transition.try_lock().is_ok());
        session.invalidate_runtime_observations();
        assert!(readback.wait_for_read_eligibility().await.is_err());
        server.await.unwrap();
    }
}

#[tokio::test]
async fn admitted_connection_closure_survives_cancellation_and_shutdown_waits_for_acknowledgement()
{
    let (session, listener) = session().await;
    let source = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut stream = delete(&listener, "DELETE /connections ").await;
        entered.send(()).unwrap();
        released.await.unwrap();
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    });
    let closing = session.clone();
    let waiter =
        tokio::spawn(async move { closing.close_observed_connections(&source, None).await });
    tokio::time::timeout(Duration::from_secs(2), started)
        .await
        .unwrap()
        .unwrap();
    waiter.abort();
    assert!(waiter.await.is_err_and(|error| error.is_cancelled()));
    assert!(session.transition.try_lock().is_err());
    let stopping = session.clone();
    let shutdown = tokio::spawn(async move { stopping.shutdown().await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !session.is_shutting_down() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!shutdown.is_finished());
    release.send(()).unwrap();
    server.await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
