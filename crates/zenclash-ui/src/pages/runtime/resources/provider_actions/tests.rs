use super::super::test_support::ResourceActionController;
use super::*;
use crate::pages::runtime::test_support::{
    RuntimePageFixture as Fixture, open_runtime_page_window as open,
};
use crate::pages::runtime::{Page, page_observation::ObservedPage};
use gpui_kit::AppContext;
use gpui_kit::test::TestWindowExt;
use zenclash_core::{CoreKind, CoreSession, MihomoClient, MihomoEndpoint, ProviderOperations};

#[tokio::test]
async fn completed_provider_update_does_not_read_the_replacement_runtime() {
    let original = ResourceActionController::new("original provider");
    let replacement = ResourceActionController::new("replacement provider");
    let client = MihomoClient::new(MihomoEndpoint::new(&original.url, "")).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
    let receipt = ProviderOperations::new(client)
        .update_with_receipt(ProviderKind::Proxy, "airport")
        .await
        .unwrap();
    assert_eq!(
        receipt.catalog().providers["airport"].name,
        "original provider"
    );
    session
        .switch_to_direct(MihomoEndpoint::new(&replacement.url, ""))
        .await
        .unwrap();

    let result =
        ObservedPage::load_for_runtime(session, receipt.client().clone(), Page::Resources).await;

    assert!(
        result.is_err(),
        "a completed update must not start a new-runtime presentation read"
    );
    assert!(replacement.requests().is_empty());
    assert!(
        original
            .requests()
            .iter()
            .any(|request| request.starts_with("PUT /providers/proxies/airport "))
    );
}

#[tokio::test]
async fn completed_provider_healthcheck_does_not_read_the_replacement_runtime() {
    let original = ResourceActionController::new("original provider");
    let replacement = ResourceActionController::new("replacement provider");
    let client = MihomoClient::new(MihomoEndpoint::new(&original.url, "")).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
    let receipt = ProviderOperations::new(client)
        .healthcheck_proxy_with_receipt("airport")
        .await
        .unwrap();
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
            .any(|request| request.starts_with("GET /providers/proxies/airport/healthcheck "))
    );
}

#[gpui_kit::test]
fn a_completed_provider_page_cannot_publish_after_a_configuration_change(
    cx: &mut gpui_kit::TestAppContext,
) {
    let controller = ResourceActionController::new("original provider");
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
        let receipt = ProviderOperations::new(session.client().clone())
            .update_with_receipt(ProviderKind::Proxy, "airport")
            .await
            .unwrap();
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
            page.page_observation.replace_fixture_data(PageSnapshotData::Config(zenclash_core::RuntimeConfig {
                mixed_port: 9999,
                ..Default::default()
            }));
            assert!(!page.accept_completed_resource_page(token, observed, cx));
            assert!(matches!(page.page_observation.data(), PageSnapshotData::Config(config) if config.mixed_port == 9999));
            assert!(page.notice.is_none());
        });
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn provider_update_and_healthcheck_buttons_publish_observed_resource_facts(
    cx: &mut gpui_kit::TestAppContext,
) {
    let controller = ResourceActionController::new("original provider");
    let fixture = Fixture::with_controller(controller.url.clone());
    let (window, page) = open(cx, &fixture, Page::Resources);
    fixture.settle(cx, &page, |page| !page.persistent_loading && !page.loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("update-provider-Proxy-airport", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.core_busy() && page.notice.is_some());
    cx.update(|cx| {
        assert!(page.read(cx).page_observation.source().as_ref().unwrap().is_current());
        assert!(matches!(page.read(cx).page_observation.data(), PageSnapshotData::Resources { proxy, .. } if proxy.providers["airport"].name == "original provider"));
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("healthcheck-provider-airport", cx);
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
                .any(|request| request.starts_with("GET /providers/proxies/airport/healthcheck "))
        );
        window.remove_window();
    })
    .unwrap();
}
