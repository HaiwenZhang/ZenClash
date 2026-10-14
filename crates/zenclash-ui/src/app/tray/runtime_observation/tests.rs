use super::super::test_support::TrayController;
use super::*;
use std::time::Duration;

#[test]
fn tray_compound_read_rejects_a_config_change_while_catalog_response_is_delayed() {
    let owned_runtime = tokio::runtime::Runtime::new().unwrap();
    let runtime = owned_runtime.handle().clone();
    let mut controller = TrayController::new(&runtime, Some("GET /proxies "));
    let session = controller.session();
    owned_runtime.block_on(async {
        let reading = session.clone();
        let task = tokio::spawn(async move { TrayRuntimeObservation::load(&reading).await });
        tokio::time::timeout(Duration::from_secs(2), controller.started.take().unwrap())
            .await
            .unwrap()
            .unwrap();
        session.set_mode(&controller.store, "global").await.unwrap();
        controller.release.take().unwrap().send(()).unwrap();
        assert!(task.await.unwrap().is_err());
    });
}

#[test]
fn tray_compound_read_never_follows_a_controller_replaced_during_catalog_io() {
    let owned_runtime = tokio::runtime::Runtime::new().unwrap();
    let runtime = owned_runtime.handle().clone();
    let mut original = TrayController::new(&runtime, Some("GET /proxies "));
    let replacement = TrayController::new(&runtime, None);
    let session = original.session();
    owned_runtime.block_on(async {
        let reading = session.clone();
        let task = tokio::spawn(async move { TrayRuntimeObservation::load(&reading).await });
        tokio::time::timeout(Duration::from_secs(2), original.started.take().unwrap())
            .await
            .unwrap()
            .unwrap();
        session
            .switch_to_direct(zenclash_core::MihomoEndpoint::new(
                &replacement.endpoint,
                "",
            ))
            .await
            .unwrap();
        original.release.take().unwrap().send(()).unwrap();
        assert!(task.await.unwrap().is_err());
        assert!(replacement.requests.lock().unwrap().is_empty());
    });
}
