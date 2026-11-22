use super::*;

#[gpui_kit::test]
fn successful_bootstrap_preserves_an_earlier_page_read_error(cx: &mut gpui_kit::TestAppContext) {
    use crate::test_support::{RuntimePageFixture, open_runtime_page_window};

    let fixture = RuntimePageFixture::new();
    let (window, page) = open_runtime_page_window(cx, &fixture, Page::Override);
    fixture.wait_for_initial_reads(cx, &page);
    let state = load_initial_persistent_state(
        &fixture.controlled,
        Some(fixture.profiles.clone()),
        Some(fixture.overrides.clone()),
    );
    assert!(state.error.is_none());
    let task = fixture
        .runtime
        .as_ref()
        .unwrap()
        .spawn(async move { state });
    cx.update(|cx| {
        page.update(cx, |page, cx| {
            // Force the failing page read to publish before successful bootstrap adoption.
            page.error = Some("resources query failed".into());
            page.persistent_loading = true;
            page.config_inputs.begin_bootstrap_read();
            page.finish_initial_persistent_state(task, fixture.core.generation(), cx);
        });
    });
    fixture.wait_for_initial_reads(cx, &page);
    cx.update_window(window, |_, window, cx| {
        assert_eq!(
            page.read(cx).error.as_deref(),
            Some("resources query failed")
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn report_automatic_runtime_preserves_error_when_notice_triggers_refresh(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::test_support::{RuntimePageFixture, open_runtime_page_window};

    let fixture = RuntimePageFixture::new();
    let (window, page) = open_runtime_page_window(cx, &fixture, Page::Home);
    fixture.wait_for_initial_reads(cx, &page);
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.report_automatic_runtime(
                Some("automatic.network_stopped"),
                Some("network probe failed".into()),
                cx,
            );
        });
        assert_eq!(
            page.read(cx).error.as_deref(),
            Some(
                zenclash_i18n::text_with(
                    "automatic.failed",
                    &[("error", "network probe failed".to_owned())],
                )
                .as_str()
            )
        );
        assert_eq!(
            page.read(cx).notice.as_deref(),
            Some(zenclash_i18n::text("automatic.network_stopped").as_str())
        );
        window.remove_window();
    })
    .unwrap();
}
