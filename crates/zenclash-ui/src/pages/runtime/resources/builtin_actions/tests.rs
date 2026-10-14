use super::super::test_support::ResourceActionController;
use super::*;
use crate::pages::runtime::test_support::{
    RuntimePageFixture as Fixture, open_runtime_page_window as open,
};
use gpui_kit::{AppContext, test::TestWindowExt};
use zenclash_core::{CoreKind, CoreSession, MihomoClient, MihomoEndpoint};

#[tokio::test]
async fn completed_geodata_update_does_not_read_the_replacement_runtime() {
    let original = ResourceActionController::new("original");
    let replacement = ResourceActionController::new("replacement");
    let client = MihomoClient::new(MihomoEndpoint::new(&original.url, "")).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
    let receipt = session.update_geodata_with_receipt().await.unwrap();
    session
        .switch_to_direct(MihomoEndpoint::new(&replacement.url, ""))
        .await
        .unwrap();
    assert!(
        ObservedPage::load_for_runtime(session, receipt.client().clone(), Page::Resources)
            .await
            .is_err()
    );
    assert!(replacement.requests().is_empty());
    assert!(
        original
            .requests()
            .iter()
            .any(|request| request.starts_with("POST /configs/geo "))
    );
}

#[tokio::test]
async fn completed_external_ui_update_does_not_read_the_replacement_runtime() {
    let original = ResourceActionController::new("original");
    let replacement = ResourceActionController::new("replacement");
    let client = MihomoClient::new(MihomoEndpoint::new(&original.url, "")).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
    let receipt = session.update_external_ui_with_receipt().await.unwrap();
    session
        .switch_to_direct(MihomoEndpoint::new(&replacement.url, ""))
        .await
        .unwrap();
    assert!(
        ObservedPage::load_for_runtime(session, receipt.client().clone(), Page::Resources)
            .await
            .is_err()
    );
    assert!(replacement.requests().is_empty());
    assert!(
        original
            .requests()
            .iter()
            .any(|request| request.starts_with("POST /upgrade/ui "))
    );
}

#[gpui_kit::test]
fn a_completed_builtin_page_cannot_publish_after_configuration_change(
    cx: &mut gpui_kit::TestAppContext,
) {
    let controller = ResourceActionController::new("original");
    let fixture = Fixture::with_controller(controller.url.clone());
    let services = fixture.services();
    let runtime = services.runtime.clone();
    let session = services.core_session.clone();
    let store = services.controlled_config_store;
    let (window, page) = open(cx, &fixture, Page::Resources);
    fixture.settle(cx, &page, |page| !page.persistent_loading && !page.loading);
    let token =
        cx.update(|cx| page.update(cx, |page, _| page.begin_mutation(Page::Resources).unwrap()));
    let observed = runtime.block_on(async {
        let receipt = session.update_geodata_with_receipt().await.unwrap();
        ObservedPage::load_for_runtime(session.clone(), receipt.client().clone(), Page::Resources)
            .await
            .unwrap()
    });
    runtime
        .block_on(session.set_mode(&store, "global"))
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.finish_mutation(token);
            page.page_observation.replace_fixture_data(super::super::PageSnapshotData::Config(zenclash_core::RuntimeConfig { mixed_port: 9999, ..Default::default() }));
            assert!(!page.accept_completed_resource_page(token, observed, cx));
            assert!(matches!(page.page_observation.data(), super::super::PageSnapshotData::Config(config) if config.mixed_port == 9999));
            assert!(page.notice.is_none());
        });
        window.remove_window();
    }).unwrap();
}

#[gpui_kit::test]
fn builtin_update_buttons_publish_original_runtime_observations(cx: &mut gpui_kit::TestAppContext) {
    let controller = ResourceActionController::new("original");
    let fixture = Fixture::with_controller(controller.url.clone());
    let (window, page) = open(cx, &fixture, Page::Resources);
    fixture.settle(cx, &page, |page| !page.persistent_loading && !page.loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("update-geodata", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.core_busy() && page.notice.is_some());
    cx.update(|cx| {
        assert!(
            page.read(cx)
                .page_observation
                .source()
                .as_ref()
                .unwrap()
                .is_current()
        )
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("update-external-ui", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.core_busy() && page.notice.is_some());
    cx.update_window(window, |_, window, cx| {
        assert!(
            page.read(cx)
                .page_observation
                .source()
                .as_ref()
                .unwrap()
                .is_current()
        );
        assert!(
            controller
                .requests()
                .iter()
                .any(|request| request.starts_with("POST /configs/geo "))
        );
        assert!(
            controller
                .requests()
                .iter()
                .any(|request| request.starts_with("POST /upgrade/ui "))
        );
        window.remove_window();
    })
    .unwrap();
}
