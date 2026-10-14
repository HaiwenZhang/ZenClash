use super::*;
use crate::pages::runtime::{
    resources::test_support::ResourceActionController,
    test_support::{RuntimePageFixture as Fixture, open_runtime_page_window as open},
};
use gpui_kit::{AppContext, TestAppContext};
use zenclash_core::{CoreResourceAction, ProviderKind};

#[gpui_kit::test]
fn builtin_completion_does_not_capture_replacement_configuration_for_presentation(
    cx: &mut TestAppContext,
) {
    let controller = ResourceActionController::new("original");
    let fixture = Fixture::with_controller(controller.url.clone());
    let services = fixture.services();
    let (window, page) = open(cx, &fixture, Page::Resources);
    fixture.settle(cx, &page, |page| !page.loading && !page.persistent_loading);
    let context = cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.begin_resource_action_context(false, cx).unwrap()
        })
    });
    let receipt = services
        .runtime
        .block_on(services.core_session.apply_observed_resource_action(
            context.observation.as_ref().unwrap(),
            &zenclash_core::ProviderOperations::new(services.core_session.client().clone()),
            CoreResourceAction::GeoData,
        ))
        .unwrap();
    services
        .runtime
        .block_on(
            services
                .core_session
                .set_mode(&services.controlled_config_store, "global"),
        )
        .unwrap();
    let before = controller.requests();
    let data = services
        .runtime
        .block_on(ObservedPage::load_from_observation(
            services.core_session.clone(),
            receipt.observation().clone(),
            Page::Resources,
        ));
    assert!(
        data.is_err(),
        "completed builtin recaptured the replacement configuration"
    );
    assert_eq!(
        controller.requests(),
        before,
        "a rejected original source still issued a Resources read"
    );
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.finish_mutation(context.token);
            page.notice = Some("current notice".into());
            page.error = Some("current error".into());
            complete_action(
                page,
                context,
                data.map(|data| (data, Vec::new())),
                Some("old success".into()),
                cx,
            );
            assert_eq!(page.notice.as_deref(), Some("current notice"));
            assert_eq!(page.error.as_deref(), Some("current error"));
        });
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn provider_completion_does_not_read_a_new_configuration_on_the_same_binding(
    cx: &mut TestAppContext,
) {
    let controller = ResourceActionController::new("original");
    let fixture = Fixture::with_controller(controller.url.clone());
    let services = fixture.services();
    let (window, page) = open(cx, &fixture, Page::Resources);
    fixture.settle(cx, &page, |page| !page.loading && !page.persistent_loading);
    let context = cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.begin_resource_action_context(false, cx).unwrap()
        })
    });
    let receipt = services
        .runtime
        .block_on(services.core_session.apply_observed_resource_action(
            context.observation.as_ref().unwrap(),
            &zenclash_core::ProviderOperations::new(services.core_session.client().clone()),
            CoreResourceAction::UpdateProvider {
                kind: ProviderKind::Proxy,
                key: "airport".into(),
            },
        ))
        .unwrap();
    services
        .runtime
        .block_on(
            services
                .core_session
                .set_mode(&services.controlled_config_store, "global"),
        )
        .unwrap();
    assert!(
        services
            .runtime
            .block_on(ObservedPage::load_from_observation(
                services.core_session.clone(),
                receipt.observation().clone(),
                Page::Resources
            ))
            .is_err()
    );
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, _| page.finish_mutation(context.token));
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_new_resources_query_rejects_old_success_data_and_notice_in_the_same_configuration(
    cx: &mut TestAppContext,
) {
    let controller = ResourceActionController::new("original");
    let fixture = Fixture::with_controller(controller.url.clone());
    let services = fixture.services();
    let (window, page) = open(cx, &fixture, Page::Resources);
    fixture.settle(cx, &page, |page| !page.loading && !page.persistent_loading);
    let context = cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.begin_resource_action_context(false, cx).unwrap()
        })
    });
    let receipt = services
        .runtime
        .block_on(services.core_session.apply_observed_resource_action(
            context.observation.as_ref().unwrap(),
            &zenclash_core::ProviderOperations::new(services.core_session.client().clone()),
            CoreResourceAction::ExternalUi,
        ))
        .unwrap();
    let old = services
        .runtime
        .block_on(ObservedPage::load_from_observation(
            services.core_session.clone(),
            receipt.observation().clone(),
            Page::Resources,
        ))
        .unwrap();
    let current = services
        .runtime
        .block_on(ObservedPage::load(
            services.core_session.clone(),
            Page::Resources,
        ))
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.finish_mutation(context.token);
            assert!(page.accept_page_observation(current, cx));
            let current_identity = page.page_observation.source().clone().unwrap();
            assert!(context.observation.as_ref().unwrap().is_current());
            page.notice = Some("new query".into());
            complete_action(
                page,
                context,
                Ok((old, Vec::new())),
                Some("old success".into()),
                cx,
            );
            assert!(
                page.page_observation
                    .source()
                    .as_ref()
                    .unwrap()
                    .is_same_observation(&current_identity)
            );
            assert_eq!(page.notice.as_deref(), Some("new query"));
        });
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_new_resources_query_rejects_an_old_action_error(cx: &mut TestAppContext) {
    let controller = ResourceActionController::new("original");
    let fixture = Fixture::with_controller(controller.url.clone());
    let services = fixture.services();
    let (window, page) = open(cx, &fixture, Page::Resources);
    fixture.settle(cx, &page, |page| !page.loading && !page.persistent_loading);
    let context = cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.begin_resource_action_context(false, cx).unwrap()
        })
    });
    let current = services
        .runtime
        .block_on(ObservedPage::load(
            services.core_session.clone(),
            Page::Resources,
        ))
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.finish_mutation(context.token);
            assert!(page.accept_page_observation(current, cx));
            page.error = Some("new query feedback".into());
            complete_action(page, context, Err("old update failed".into()), None, cx);
            assert_eq!(page.error.as_deref(), Some("new query feedback"));
        });
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn starting_a_new_resources_read_rejects_old_completion_before_the_new_read_is_adopted(
    cx: &mut TestAppContext,
) {
    let controller = ResourceActionController::new("original");
    let fixture = Fixture::with_controller(controller.url.clone());
    let (window, page) = open(cx, &fixture, Page::Resources);
    fixture.settle(cx, &page, |page| !page.loading && !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            let context = page.begin_resource_action_context(false, cx).unwrap();
            page.finish_mutation(context.token);
            page.page_observation
                .begin(Page::Resources, page.navigation_generation);
            page.error = Some("new read".into());
            complete_action(page, context, Err("old completion".into()), None, cx);
            assert_eq!(page.error.as_deref(), Some("new read"));
        });
        window.remove_window();
    })
    .unwrap();
}

fn complete_action(
    page: &mut RuntimePage,
    context: ResourceActionContext,
    result: Result<(ObservedPage, Vec<String>), String>,
    notice: Option<String>,
    cx: &mut Context<RuntimePage>,
) {
    let (observed, failures) = match result {
        Ok((observed, failures)) => (Ok(observed), failures),
        Err(error) => (Err(error), Vec::new()),
    };
    let completion = ResourceActionCompletion {
        observation: context.observation.clone(),
        page: observed,
        failures,
    };
    page.complete_resource_action(context, completion, notice, cx);
}

#[gpui_kit::test]
fn builtin_updates_remain_available_after_the_initial_resources_query_fails(
    cx: &mut TestAppContext,
) {
    let controller = ResourceActionController::new("original");
    controller.fail_catalog_reads();
    let fixture = Fixture::with_controller(controller.url.clone());
    let (window, page) = open(cx, &fixture, Page::Resources);
    fixture.settle(cx, &page, |page| !page.loading && !page.persistent_loading);
    cx.update(|cx| {
        page.update(cx, |page, cx| {
            assert!(page.page_observation.source().is_none());
            assert!(page.error.is_some());
            assert!(page.begin_resource_action_context(false, cx).is_none());
            page.update_builtin_resource(
                super::super::builtin_actions::BuiltinResource::GeoData,
                cx,
            );
        })
    });
    fixture.settle(cx, &page, |page| !page.core_busy());
    assert!(
        controller
            .requests()
            .iter()
            .any(|request| request.starts_with("POST /configs/geo "))
    );
    cx.update_window(window, |_, window, cx| {
        assert!(page.read(cx).page_observation.source().is_none());
        assert!(
            page.read(cx).error.is_some(),
            "read failure was disguised as successful presentation"
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn resource_readback_failure_keeps_batch_target_diagnostics(cx: &mut TestAppContext) {
    let controller = ResourceActionController::new("original");
    let fixture = Fixture::with_controller(controller.url.clone());
    let services = fixture.services();
    let (window, page) = open(cx, &fixture, Page::Resources);
    fixture.settle(cx, &page, |page| !page.loading && !page.persistent_loading);
    let context = cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.begin_resource_action_context(false, cx).unwrap()
        })
    });
    controller.fail_catalog_reads();
    let completion = services.runtime.block_on(ResourceActionCompletion::execute(
        services.core_session.clone(),
        ProviderOperations::new(services.core_session.client().clone()),
        context.observation.clone(),
        CoreResourceAction::UpdateProviders(vec![zenclash_core::ProviderUpdateTarget::new(
            ProviderKind::Proxy,
            "airport",
        )]),
    ));
    assert_eq!(completion.failures.len(), 1);
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.finish_mutation(context.token);
            page.complete_resource_action(context, completion, None, cx);
            let error = page.error.as_deref().unwrap();
            assert!(error.contains("airport:"));
            assert!(
                error.lines().count() >= 2,
                "page read error hid the target failure"
            );
            assert!(error.contains("503"));
        });
        window.remove_window();
    })
    .unwrap();
}
