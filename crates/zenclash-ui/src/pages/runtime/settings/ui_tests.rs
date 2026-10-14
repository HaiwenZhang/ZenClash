use crate::pages::runtime::test_support::{
    RuntimePageFixture as Fixture, open_runtime_page_window as open,
};
use crate::pages::runtime::*;
use gpui_kit::component::WindowExt;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{TestAppContext, size};

#[gpui_kit::test]
fn offline_settings_keep_local_controls_and_accessible_names(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Settings);
    fixture.settle(cx, &page, |page| !page.persistent_loading && !page.loading);
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.invalidate_page_load();
            page.page_observation
                .replace_fixture_data(PageSnapshotData::Settings {
                    config: None,
                    autostart: Err("autostart unavailable".into()),
                });
            let store =
                zenclash_core::AppPreferencesStore::new(fixture.root.join("preferences.json"));
            page.preferences = store
                .update(|preferences| preferences.traffic_history_enabled = false)
                .unwrap();
            page.preferences_store = Some(store);
            cx.notify();
        });
        window.render_frame(cx);
        window.find("settings-language");
        window.find("theme-dark");
        assert!(window.try_find("settings-ipv6").is_none());
        window.click("settings-autostart", cx);
        assert!(!page.read(cx).core_busy());
        assert!(matches!(
            page.read(cx).page_observation.data(),
            PageSnapshotData::Settings { autostart: Err(error), .. } if error == "autostart unavailable"
        ));
        let focus = page.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        for _ in 0..40 {
            window.press("tab", cx);
            assert_ne!(window.find("settings-autostart").focused(), Some(true));
        }
    })
    .unwrap();
    // Default 1280 px app window minus the 224 px application sidebar.
    cx.simulate_window_resize(window, size(px(1056.), px(820.)));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        for id in ["theme-light", "theme-dark", "theme-system"] {
            let bounds = window.find(id).bounds();
            assert!(bounds.size.width > px(0.));
            assert!(
                bounds.origin.x >= px(0.) && bounds.right() <= px(1056.),
                "{id} exceeds the workspace width"
            );
        }
        for (card, ids) in [
            (
                "settings-appearance-card",
                ["settings-language", "settings-language"],
            ),
            (
                "settings-appearance-card",
                ["settings-tray-visible", "settings-tray-visible"],
            ),
        ] {
            let card_bounds = window.find(card).bounds();
            for id in ids {
                let bounds = window.find(id).bounds();
                assert!(
                    bounds.origin.x >= card_bounds.origin.x
                        && bounds.right() <= card_bounds.right(),
                    "{id} is clipped by its settings card"
                );
            }
        }
        assert!(window.try_find("settings-traffic-history").is_some());
        assert!(window.simulate_next_frame(cx) > 0);
        window.render_frame(cx);
        assert_eq!(page.read(cx).body_scroll.offset().y, px(0.));
        assert!(window.find("settings-traffic-history").visible());
        window.click("settings-traffic-history", cx);
        window.render_frame(cx);
        assert_eq!(page.read(cx).body_scroll.offset().y, px(0.));
    })
    .unwrap();
    fixture.settle(cx, &page, |page| page.preferences.traffic_history_enabled);
    assert!(
        zenclash_core::AppPreferencesStore::new(fixture.root.join("preferences.json"))
            .load()
            .unwrap()
            .traffic_history_enabled
    );
    let navigation_target = page.downgrade();
    cx.update(|cx| {
        // The fixture hosts RuntimePage alone; install its application's navigation receiver.
        cx.on_action(move |_: &crate::app::NavigateDns, cx| {
            navigation_target
                .update(cx, |page, cx| page.switch_to(Page::Dns, cx))
                .unwrap();
        });
    });
    cx.update_window(window, |_, window, cx| {
        window.click("settings-tab-dns", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| page.read(cx).page), Page::Dns);
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn license_and_fork_notices_are_readable_with_an_offline_core(cx: &mut TestAppContext) {
    use gpui_kit::component::WindowExt as _;
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Settings);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.persistent_loading && !page.loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-license-notices", cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        assert!(window.find("license-notices-scroll").visible());
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
        assert!(!page.read(cx).core_busy());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn settings_list_modal_stages_acceptance_and_cancels_without_saving(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Dns);
    fixture.settle(cx, &page, |page| {
        !page.persistent_loading && !page.config_inputs.is_loading()
    });
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.page_observation
                .replace_fixture_data(PageSnapshotData::Config(RuntimeConfig::default()));
            page.page_observation
                .set_fixture_generation(page.core_session.generation());
            page.error = None;
            cx.notify();
        });
        window.render_frame(cx);
        window.click("settings-edit-settings_redesign.dns_nameserver", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        window.input("https://example.com/dns-query", cx);
        assert_eq!(
            page.read(cx).config_inputs.dns.nameserver.read(cx).value(),
            ""
        );
        window.press("escape", cx);
        window.render_frame(cx);
        assert_eq!(
            page.read(cx).config_inputs.dns.nameserver.read(cx).value(),
            ""
        );
        window.click("settings-edit-settings_redesign.dns_nameserver", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.input("https://example.net/dns-query", cx);
        window.click("ok", cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
        assert_eq!(
            page.read(cx).config_inputs.dns.nameserver.read(cx).value(),
            "https://example.net/dns-query"
        );
        assert!(
            page.read(cx)
                .controlled_config
                .pointer("/dns/nameserver")
                .is_none()
        );
        page.update(cx, |page, cx| {
            page.config_inputs
                .core
                .mixed_port
                .update(cx, |input, cx| input.set_value("7899", window, cx));
        });
        window.click("settings-cancel-draft", cx);
        assert_eq!(
            page.read(cx).config_inputs.dns.nameserver.read(cx).value(),
            ""
        );
        assert_eq!(
            page.read(cx).config_inputs.core.mixed_port.read(cx).value(),
            "7899"
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn settings_enum_popover_accepts_keyboard_selection_without_saving(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Dns);
    fixture.settle(cx, &page, |page| {
        !page.persistent_loading && !page.config_inputs.is_loading()
    });
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.page_observation
                .replace_fixture_data(PageSnapshotData::Config(RuntimeConfig::default()));
            page.page_observation
                .set_fixture_generation(page.core_session.generation());
            page.config_inputs
                .dns
                .enhanced_mode
                .update(cx, |input, cx| input.set_value("fake-ip", window, cx));
            cx.notify();
        });
        window.render_frame(cx);
        window.click("dns-mode", cx);
        window.render_frame(cx);
        assert!(
            window.find("normal").visible(),
            "all enum choices must be visible"
        );
        window.press("down", cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(
            page.read(cx)
                .config_inputs
                .dns
                .enhanced_mode
                .read(cx)
                .value(),
            "redir-host"
        );
        assert!(
            page.read(cx)
                .controlled_config
                .pointer("/dns/enhanced-mode")
                .is_none()
        );
        window.click("dns-mode", cx);
        window.render_frame(cx);
        window.press("down", cx);
        window.press("escape", cx);
        assert_eq!(
            page.read(cx)
                .config_inputs
                .dns
                .enhanced_mode
                .read(cx)
                .value(),
            "redir-host"
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn app_update_check_opens_placeholder_dialog(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Settings);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("check-app-update", cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        window.click("app-update-close", cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
        window.remove_window();
    })
    .unwrap();
}
