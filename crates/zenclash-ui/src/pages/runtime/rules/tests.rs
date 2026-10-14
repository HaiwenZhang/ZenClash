use super::super::test_support::{RuntimePageFixture as Fixture, open_runtime_page_window as open};
use super::super::{Page, PageSnapshotData, lifecycle};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{TestAppContext, VisualTestContext};
use zenclash_core::RuleCatalog;

use super::*;

#[test]
fn filter_matches_all_visible_rule_fields_case_insensitively() {
    let rule = zenclash_core::Rule {
        kind: "DomainSuffix".into(),
        payload: "Example.COM".into(),
        proxy: "Auto Select".into(),
        ..Default::default()
    };

    assert!(rule_matches(&rule, &normalize_rule_query("DOMAIN")));
    assert!(rule_matches(&rule, &normalize_rule_query("example.com")));
    assert!(rule_matches(&rule, &normalize_rule_query("auto select")));
    assert!(!rule_matches(&rule, &normalize_rule_query("DIRECT")));
}

#[gpui_kit::test]
fn actual_rule_filter_input_publishes_the_matching_projection(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Rules);
    fixture.settle_with_context(cx, &page, |page, _| !page.persistent_loading);
    cx.update_window(window, |_, _, cx| {
        page.update(cx, |page, cx| {
            page.ui_visibility = lifecycle::UiVisibility::new(true);
            page.invalidate_page_load();
            let data = PageSnapshotData::Rules {
                proxies: None,
                config: None,
                catalog: Arc::new(RuleCatalog {
                    rules: vec![
                        zenclash_core::Rule {
                            payload: "alpha.example".into(),
                            ..Default::default()
                        },
                        zenclash_core::Rule {
                            payload: "beta.example".into(),
                            index: Some(1),
                            extra: Some(Default::default()),
                            ..Default::default()
                        },
                    ],
                }),
            };
            page.replace_page_data(page.page_task_token_for(Page::Rules), data, cx);
        });
    })
    .unwrap();
    fixture.settle_with_context(cx, &page, |page, cx| !page.rules.read(cx).is_projecting());
    let search_guard = cx.update(|cx| page.read(cx).rules.read(cx).hold_projection());
    let panel_top = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.find("rules-table-panel").bounds().top()
        })
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(
            (
                "input",
                page.read(cx).rules.read(cx).filter_input().entity_id(),
            ),
            cx,
        );
        window.input("beta", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(page.read(cx).rules.read(cx).is_projecting());
        assert_eq!(window.find("rules-table-panel").bounds().top(), panel_top);
        assert!(window.try_find("rule-inspector").is_none());
        assert!(window.try_find(("rule-row", 0usize)).is_none());
        assert!(window.try_find(("rule-row", 1usize)).is_none());
        window.press("secondary-a", cx);
        window.press("backspace", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(!page.read(cx).rules.read(cx).is_projecting());
        assert!(window.try_find(("rule-row", 0usize)).is_some());
        assert!(window.try_find(("rule-row", 1usize)).is_some());
        window.input("beta", cx);
    })
    .unwrap();
    drop(search_guard);
    fixture.settle_with_context(cx, &page, |page, cx| !page.rules.read(cx).is_projecting());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("rule-row", 1usize)).is_some());
        assert!(window.try_find(("rule-row", 0usize)).is_none());
        assert_eq!(
            window.find(("rule-enabled", 1usize)).label(),
            Some(
                zenclash_i18n::text_with(
                    "rules.row.enabled_named",
                    &[("index", "1".into()), ("payload", "beta.example".into())]
                )
                .as_str()
            )
        );
        window.press("secondary-a", cx);
        window.input("missing", cx);
    })
    .unwrap();
    fixture.settle_with_context(cx, &page, |page, cx| !page.rules.read(cx).is_projecting());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("rule-row", 1usize)).is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn blurring_preserves_loaded_rules_until_the_window_is_hidden(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let _runtime_context = fixture.enter_runtime();
    let (window, page) = open(cx, &fixture, Page::Rules);
    cx.update_window(window, |_, window, _| window.activate_window())
        .unwrap();
    fixture.settle_with_context(cx, &page, |page, _| {
        !page.persistent_loading && !page.loading && page.live_updates_enabled()
    });
    cx.update_window(window, |_, _, cx| {
        page.update(cx, |page, cx| {
            page.invalidate_page_load();
            page.replace_page_data(
                page.page_task_token_for(Page::Rules),
                PageSnapshotData::Rules {
                    proxies: None,
                    config: None,
                    catalog: Arc::new(RuleCatalog {
                        rules: vec![zenclash_core::Rule {
                            payload: "loaded.example".into(),
                            ..Default::default()
                        }],
                    }),
                },
                cx,
            );
        });
    })
    .unwrap();
    fixture.settle_with_context(cx, &page, |page, cx| !page.rules.read(cx).is_projecting());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(("rule-row", 0usize));
    })
    .unwrap();

    VisualTestContext::from_window(window, cx).deactivate_window();
    assert!(!cx.update(|cx| page.read(cx).live_updates_enabled()));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(("rule-row", 0usize));
        window.activate_window();
    })
    .unwrap();
    fixture.settle_with_context(cx, &page, |page, _| {
        page.live_updates_enabled() && !page.loading
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(
            (
                "input",
                page.read(cx).rules.read(cx).filter_input().entity_id(),
            ),
            cx,
        );
        window.input("loaded", cx);
    })
    .unwrap();
    assert!(cx.update(|cx| page.read(cx).rules.read(cx).is_projecting()));
    VisualTestContext::from_window(window, cx).deactivate_window();
    assert!(!cx.update(|cx| page.read(cx).rules.read(cx).is_projecting()));
    cx.update_window(window, |_, window, _| window.activate_window())
        .unwrap();
    fixture.settle_with_context(cx, &page, |page, cx| {
        page.live_updates_enabled() && !page.rules.read(cx).is_projecting()
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(("rule-row", 0usize));
    })
    .unwrap();

    VisualTestContext::from_window(window, cx).deactivate_window();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(("rule-row", 0usize));
        page.update(cx, |page, cx| page.set_window_visible(false, cx));
        window.render_frame(cx);
        assert!(window.try_find(("rule-row", 0usize)).is_none());
        window.remove_window();
    })
    .unwrap();
}

use super::super::test_support::controller_fixture::ControllerFixture;

fn indexed_catalog(payload: &str) -> RuleCatalog {
    RuleCatalog {
        rules: vec![zenclash_core::Rule {
            kind: "DOMAIN".into(),
            payload: payload.into(),
            proxy: "DIRECT".into(),
            index: Some(7),
            extra: Some(Default::default()),
            ..Default::default()
        }],
    }
}

fn open_observed_rules(
    cx: &mut TestAppContext,
    fixture: &Fixture,
) -> (gpui_kit::AnyWindowHandle, Entity<super::super::RuntimePage>) {
    let (window, page) = open(cx, fixture, Page::Rules);
    cx.update_window(window, |_, window, _| window.activate_window())
        .unwrap();
    fixture.settle_with_context(cx, &page, |page, cx| {
        !page.persistent_loading
            && !page.loading
            && page.page_observation.source().is_some()
            && !page.rules.read(cx).projecting
            && page.rules.read(cx).projection.is_some()
    });
    (window, page)
}

#[gpui_kit::test]
fn rule_toggle_click_uses_the_observed_controller_and_reconciles_its_receipt(
    cx: &mut TestAppContext,
) {
    let controller = ControllerFixture::new(false);
    controller.set_rule_catalog(indexed_catalog("observed.example"));
    let fixture = Fixture::with_controller(controller.endpoint_url().to_owned());
    let (window, page) = open_observed_rules(cx, &fixture);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(("rule-enabled", 7usize), cx);
    })
    .unwrap();
    fixture.settle_with_context(cx,&page,|page,cx| !page.rules.read(cx).has_pending_toggles()
        && !page.loading && controller.rule_patches().len()==1
        && matches!(page.page_observation.data(),PageSnapshotData::Rules {catalog,..} if catalog.rules[0].extra.as_ref().unwrap().disabled));
    cx.update(|cx| {
        assert_eq!(controller.rule_patches(), [serde_json::json!({"7":true})]);
        assert!(page.read(cx).notice.is_some());
        assert!(page.read(cx).rules.read(cx).pending_toggles.is_empty());
    });
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn a_late_rule_toggle_receipt_cannot_confirm_rows_after_navigation_round_trip(
    cx: &mut TestAppContext,
) {
    let controller = ControllerFixture::new(false);
    controller.set_rule_catalog(indexed_catalog("observed.example"));
    controller.set_rule_patches_blocked(true);
    let fixture = Fixture::with_controller(controller.endpoint_url().to_owned());
    let (window, page) = open_observed_rules(cx, &fixture);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(("rule-enabled", 7usize), cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |_| controller.rule_patches().len() == 1);
    cx.update(|cx| {
        page.update(cx, |page, cx| {
            assert!(page.rules.read(cx).pending_toggles.contains(&7));
            page.switch_to(Page::Settings, cx);
            page.switch_to(Page::Rules, cx);
        })
    });
    controller.set_rule_patches_blocked(false);
    fixture.settle_with_context(cx, &page, |page, cx| {
        !page.rules.read(cx).has_pending_toggles() && !page.loading
    });
    cx.update(|cx| {
        assert!(page.read(cx).rules.read(cx).confirmed_disabled.is_empty());
        assert!(
            page.read(cx).notice.is_none(),
            "old acknowledgement must not publish a reopened-row notice"
        );
        assert_eq!(controller.rule_patches(), [serde_json::json!({"7":true})]);
    });
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn a_rule_request_from_before_navigation_round_trip_never_reaches_the_controller(
    cx: &mut TestAppContext,
) {
    let controller = ControllerFixture::new(false);
    controller.set_rule_catalog(indexed_catalog("old.example"));
    let fixture = Fixture::with_controller(controller.endpoint_url().to_owned());
    let (window, page) = open_observed_rules(cx, &fixture);
    cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.rules
                .update(cx, |view, cx| view.request_rule_enabled(7, false, cx));
            page.switch_to(Page::Settings, cx);
            page.switch_to(Page::Rules, cx);
        })
    });
    fixture.settle_with_context(cx, &page, |page, cx| {
        !page.loading && !page.rules.read(cx).has_pending_toggles()
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(controller.rule_patches().is_empty());
        assert!(page.read(cx).rules.read(cx).confirmed_disabled.is_empty());
    });
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn a_rule_request_for_a_replaced_catalog_never_reaches_the_controller(cx: &mut TestAppContext) {
    let controller = ControllerFixture::new(false);
    controller.set_rule_catalog(indexed_catalog("old.example"));
    let fixture = Fixture::with_controller(controller.endpoint_url().to_owned());
    let (window, page) = open_observed_rules(cx, &fixture);
    cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.rules
                .update(cx, |view, cx| view.request_rule_enabled(7, false, cx));
            page.replace_page_data(
                page.page_task_token_for(Page::Rules),
                PageSnapshotData::Rules {
                    catalog: Arc::new(indexed_catalog("replacement.example")),
                    config: None,
                    proxies: None,
                },
                cx,
            );
        })
    });
    fixture.settle_with_context(cx, &page, |page, cx| {
        !page.loading && !page.rules.read(cx).has_pending_toggles()
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(controller.rule_patches().is_empty());
        assert!(page.read(cx).rules.read(cx).confirmed_disabled.is_empty());
    });
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn rule_pagination_and_disabled_filter_are_owned_by_the_retained_view(cx: &mut TestAppContext) {
    let controller = ControllerFixture::new(false);
    controller.set_rule_catalog(RuleCatalog {
        rules: (0..125)
            .map(|index| zenclash_core::Rule {
                kind: "DOMAIN".into(),
                payload: format!("entry-{index}"),
                index: Some(index),
                extra: Some(zenclash_core::RuleRuntimeStats {
                    disabled: index % 2 == 0,
                    ..Default::default()
                }),
                ..Default::default()
            })
            .collect(),
    });
    let fixture = Fixture::with_controller(controller.endpoint_url().to_owned());
    let (window, page) = open_observed_rules(cx, &fixture);
    cx.update(|cx| assert!(page.read(cx).error.is_none()));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let bounds = window.find(("rules-numbered-page", 2usize)).bounds();
        assert!(
            bounds.bottom() <= window.viewport_size().height,
            "pagination control is outside the viewport: bounds={bounds:?}, viewport={:?}",
            window.viewport_size()
        );
        window.click(("rules-numbered-page", 2usize), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        assert_eq!(page.read(cx).rules.read(cx).page_index, 2);
        window.render_frame(cx);
        window.find(("rule-row", 100usize));
        window.find(("rule-row", 124usize));
        assert!(window.try_find(("rule-row", 99usize)).is_none());
        window.click("rules-disabled-filter", cx);
    })
    .unwrap();
    fixture.settle_with_context(cx, &page, |page, cx| !page.rules.read(cx).projecting);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(("rule-row", 0usize));
        assert!(window.try_find(("rule-row", 1usize)).is_none());
        let input = page.read(cx).rules.read(cx).filter_input.entity_id();
        window.click(("input", input), cx);
        window.input("entry-124", cx);
    })
    .unwrap();
    fixture.settle_with_context(cx, &page, |page, cx| !page.rules.read(cx).projecting);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(("rule-row", 124usize));
        assert!(window.try_find(("rule-row", 0usize)).is_none());
        assert_eq!(page.read(cx).rules.read(cx).page_index, 0);
        assert!(
            page.read(cx)
                .rules
                .read(cx)
                .projection
                .as_ref()
                .unwrap()
                .filter
                .disabled_only
        );
        window.remove_window();
    })
    .unwrap();
}
