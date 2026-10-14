use crate::pages::runtime::test_support::{
    RuntimePageFixture as Fixture, open_runtime_page_window as open, owned_ui_children,
};
use crate::pages::runtime::*;
use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt;
use std::time::Duration;

#[gpui_kit::test]
fn stopped_owner_completion_cannot_authorize_old_config_for_a_new_owner(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let [previous, replacement] = owned_ui_children(&fixture);
    let runtime = fixture.runtime.as_ref().unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(previous.clone()))
        .unwrap();
    // This tall headless viewport exercises the production action, not native layout acceptance.
    let (window, page) = open(cx, &fixture, Page::Settings);
    // Keep geometry fixed while asserting the stop command and publication race.
    cx.update(|cx| cx.set_reduce_motion(true));
    fixture.settle(cx, &page, |page| {
        !page.persistent_loading && !page.config_inputs.is_loading()
    });
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.switch_to(Page::Mihomo, cx);
            page.invalidate_page_load();
            page.page_observation
                .replace_fixture_data(PageSnapshotData::Core {
                    version: VersionInfo::default(),
                    config: RuntimeConfig {
                        mixed_port: 12345,
                        ..Default::default()
                    },
                });
            page.page_observation
                .set_fixture_generation(fixture.core.generation());
        });
        window.render_frame(cx);
        window.click("stop-mihomo-core", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("ok").visible());
        window.click("ok", cx);
        assert!(
            page.read(cx).core_busy(),
            "confirmation must submit the stop intent"
        );
    })
    .unwrap();
    // Do not pump the GUI while the real owned child is stopped and another owner is published.
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while previous.snapshot().pid.is_some() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        {
            fixture
                .core
                .switch_to_process(replacement.clone())
                .await
                .unwrap();
        }
    });
    fixture.settle(cx, &page, |page| !page.core_busy());
    cx.update_window(window, |_, window, cx| {
        assert!(
            page.read(cx).config().is_none(),
            "stopped controller data was republished as current"
        );
        {
            assert_ne!(
                page.read(cx).notice.as_deref(),
                Some(zenclash_i18n::text("automatic.user_stopped").as_str())
            );
            assert_eq!(
                page.read(cx).mihomo_binary().as_deref(),
                Some(replacement.launch_config().binary.as_path())
            );
        }
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn stopping_an_owner_discards_its_previous_controller_config(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let [previous, _replacement] = owned_ui_children(&fixture);
    let runtime = fixture.runtime.as_ref().unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(previous.clone()))
        .unwrap();
    // This tall headless viewport exercises the production action, not native layout acceptance.
    let (window, page) = open(cx, &fixture, Page::Settings);
    // Keep geometry fixed while asserting the stop command and publication race.
    cx.update(|cx| cx.set_reduce_motion(true));
    fixture.settle(cx, &page, |page| {
        !page.persistent_loading && !page.config_inputs.is_loading()
    });
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.switch_to(Page::Mihomo, cx);
            page.invalidate_page_load();
            page.page_observation
                .replace_fixture_data(PageSnapshotData::Core {
                    version: VersionInfo::default(),
                    config: RuntimeConfig {
                        mixed_port: 12345,
                        ..Default::default()
                    },
                });
            page.page_observation
                .set_fixture_generation(fixture.core.generation());
        });
        window.render_frame(cx);
        window.click("stop-mihomo-core", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("ok").visible());
        window.click("ok", cx);
        assert!(
            page.read(cx).core_busy(),
            "confirmation must submit the stop intent"
        );
    })
    .unwrap();
    // Do not pump the GUI while the real owned child is stopped and another owner is published.
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while previous.snapshot().pid.is_some() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        {}
    });
    fixture.settle(cx, &page, |page| !page.core_busy());
    cx.update_window(window, |_, window, cx| {
        assert!(
            page.read(cx).config().is_none(),
            "stopped controller data was republished as current"
        );
        {}
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn actual_owner_switch_updates_the_ui_binary_source_and_detach_clears_it(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let [previous, replacement] = owned_ui_children(&fixture);
    let runtime = fixture.runtime.as_ref().unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(previous.clone()))
        .unwrap();
    let (window, page) = open(cx, &fixture, Page::Mihomo);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, _, cx| {
        page.update(cx, |page, _| {
            page.page_observation
                .replace_fixture_data(PageSnapshotData::Core {
                    version: VersionInfo::default(),
                    config: RuntimeConfig::default(),
                });
            page.page_observation
                .set_fixture_generation(fixture.core.generation());
        });
    })
    .unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(replacement.clone()))
        .unwrap();
    assert!(previous.snapshot().pid.is_none());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            page.read(cx).mihomo_binary().as_deref(),
            Some(replacement.launch_config().binary.as_path())
        );
        assert!(
            page.read(cx).config().is_none(),
            "old owner runtime config must not remain current"
        );
    })
    .unwrap();
    runtime
        .block_on(
            fixture
                .core
                .switch_to_direct(zenclash_core::MihomoEndpoint::default()),
        )
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(page.read(cx).mihomo_binary().is_none());
        window.remove_window();
    })
    .unwrap();
    assert!(replacement.snapshot().pid.is_none());
}
