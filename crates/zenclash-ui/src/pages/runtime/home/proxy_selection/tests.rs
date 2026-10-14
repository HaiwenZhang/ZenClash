use super::*;
use crate::pages::runtime::{
    page_observation::ObservedPage,
    test_support::{
        ControllerFixture, RuntimePageFixture as Fixture, open_runtime_page_window as open,
    },
};
use gpui_kit::{AppContext, TestAppContext};

#[gpui_kit::test]
fn a_home_selection_cannot_publish_into_a_new_dashboard_query_in_the_same_configuration(
    cx: &mut TestAppContext,
) {
    let controller = ControllerFixture::new(false);
    controller.set_proxy_catalog(serde_json::json!({"proxies":{
        "group":{"type":"Selector","now":"node","all":["node","Requested"]},
        "node":{"type":"Shadowsocks"},"Requested":{"type":"Shadowsocks"}
    }}));
    let fixture = Fixture::with_controller(controller.endpoint_url().to_owned());
    let services = fixture.services();
    let (window, page) = open(cx, &fixture, Page::Home);
    fixture.settle(cx, &page, |page| {
        !page.loading && !page.persistent_loading && page.page_observation.source().is_some()
    });
    let (token, identity, request) = cx.update(|cx| {
        page.update(cx, |page, _| {
            page.invalidate_page_load();
            let request = page
                .home
                .proxy_selection
                .start("group".into(), "Requested".into())
                .unwrap();
            (
                page.page_task_token_for(Page::Home),
                page.page_observation.source().clone().unwrap(),
                request,
            )
        })
    });
    let accepted = services
        .runtime
        .block_on(services.core_session.accept_observed_proxy_selection(
            &identity,
            "group",
            "Requested",
            ConnectionPolicy::KeepExisting,
        ))
        .unwrap();
    let next = services
        .runtime
        .block_on(ObservedPage::load(
            services.core_session.clone(),
            Page::Home,
        ))
        .unwrap();
    cx.update(|cx| {
        page.update(cx, |page, cx| {
            assert!(page.accept_page_observation(next, cx));
            assert!(
                !page
                    .page_observation
                    .source()
                    .as_ref()
                    .unwrap()
                    .is_same_observation(&identity)
            );
            assert!(identity.is_current());
            assert!(page.home.proxy_selection.acknowledge(&request));
            page.notice = Some("current dashboard feedback".into());
            let context = HomeProxySelectionContext {
                token,
                observation: identity,
                query_revision: page.page_observation.query_revision(),
            };
            page.reconcile_home_proxy_selection(context, request, accepted, cx);
        })
    });
    fixture.settle(cx, &page, |page| {
        page.home.proxy_selection.status_text("group").is_none()
    });
    cx.update_window(window, |_, window, cx| {
        assert_eq!(
            page.read(cx).notice.as_deref(),
            Some("current dashboard feedback")
        );
        assert!(
            page.read(cx)
                .home
                .proxy_selection
                .status_text("group")
                .is_none()
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_home_proxy_request_requires_the_dashboard_observation_source(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Home);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.invalidate_page_load();
            page.page_observation.set_fixture_source(None);
            page.change_home_proxy("group".into(), "Requested".into(), cx);
            assert!(!page.home.proxy_selection.any_pending());
            assert!(page.home.proxy_error.is_some());
        });
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_home_selection_error_cannot_replace_feedback_after_configuration_change(
    cx: &mut TestAppContext,
) {
    let controller = ControllerFixture::new(false);
    let fixture = Fixture::with_controller(controller.endpoint_url().to_owned());
    let services = fixture.services();
    let (window, page) = open(cx, &fixture, Page::Home);
    fixture.settle(cx, &page, |page| {
        !page.loading && !page.persistent_loading && page.page_observation.source().is_some()
    });
    cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.change_home_proxy("group".into(), String::new(), cx);
        })
    });
    services
        .runtime
        .block_on(
            services
                .core_session
                .set_mode(&services.controlled_config_store, "global"),
        )
        .unwrap();
    cx.update(|cx| {
        page.update(cx, |page, _| {
            page.home.proxy_error = Some("current configuration feedback".into())
        })
    });
    fixture.settle(cx, &page, |page| !page.home.proxy_selection.any_pending());
    cx.update_window(window, |_, window, cx| {
        assert_eq!(
            page.read(cx).home.proxy_error.as_deref(),
            Some("current configuration feedback")
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_pending_dashboard_query_rejects_selection_confirmation_from_the_previous_query(
    cx: &mut TestAppContext,
) {
    let controller = ControllerFixture::new(false);
    controller.set_proxy_catalog(serde_json::json!({"proxies":{
        "group":{"type":"Selector","now":"node","all":["node","Requested"]},
        "node":{"type":"Shadowsocks"},"Requested":{"type":"Shadowsocks"}
    }}));
    let fixture = Fixture::with_controller(controller.endpoint_url().to_owned());
    let services = fixture.services();
    let (window, page) = open(cx, &fixture, Page::Home);
    fixture.settle(cx, &page, |page| {
        !page.loading && !page.persistent_loading && page.page_observation.source().is_some()
    });
    let (context, request) = cx.update(|cx| {
        page.update(cx, |page, _| {
            let request = page
                .home
                .proxy_selection
                .start("group".into(), "Requested".into())
                .unwrap();
            (
                HomeProxySelectionContext {
                    token: page.page_task_token_for(Page::Home),
                    observation: page.page_observation.source().clone().unwrap(),
                    query_revision: page.page_observation.query_revision(),
                },
                request,
            )
        })
    });
    let accepted = services
        .runtime
        .block_on(services.core_session.accept_observed_proxy_selection(
            &context.observation,
            "group",
            "Requested",
            ConnectionPolicy::KeepExisting,
        ))
        .unwrap();
    cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.page_observation
                .begin(Page::Home, page.navigation_generation);
            page.notice = Some("new query feedback".into());
            assert!(page.home.proxy_selection.acknowledge(&request));
            page.reconcile_home_proxy_selection(context, request, accepted, cx);
        })
    });
    fixture.settle(cx, &page, |page| {
        page.home.proxy_selection.status_text("group").is_none()
    });
    cx.update_window(window, |_, window, cx| {
        assert_eq!(page.read(cx).notice.as_deref(), Some("new query feedback"));
        window.remove_window();
    })
    .unwrap();
}
