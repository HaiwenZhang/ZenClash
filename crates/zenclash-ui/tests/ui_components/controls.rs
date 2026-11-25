use gpui_kit::component::{Root, ThemeMode, h_flex, v_flex};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AppContext, Context, IntoElement, ParentElement, Render, Styled, TestAppContext, Window, div,
    px, size,
};
use zenclash_ui::components::feedback::{Feedback, FeedbackKind, FeedbackNotifications};
use zenclash_ui::components::mint_switch::MintSwitch;
use zenclash_ui::components::wave_percentage::WavePercentage;
use zenclash_ui::components::workspace;
use zenclash_ui::foundation::actions::Page;
use zenclash_ui::foundation::design::apply_zen_theme;

struct SwitchOwner {
    checked: bool,
    disabled: bool,
    changes: usize,
}

impl Render for SwitchOwner {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().p_4().child(
            MintSwitch::new("controls-mint-switch")
                .accessibility_label("Enable TUN")
                .checked(self.checked)
                .disabled(self.disabled)
                .on_click(cx.listener(|this, next, _, cx| {
                    this.checked = *next;
                    this.changes += 1;
                    cx.notify();
                })),
        )
    }
}

#[gpui_kit::test]
fn mint_switch_handles_pointer_and_keyboard_and_rejects_disabled_activation(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_kit::init);
    let mut owner = None;
    let handle = cx.open_window(size(px(360.), px(140.)), |window, cx| {
        let view = cx.new(|_| SwitchOwner {
            checked: false,
            disabled: false,
            changes: 0,
        });
        owner = Some(view.clone());
        Root::new(view, window, cx)
    });
    let owner = owner.unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        apply_zen_theme(ThemeMode::Light, Some(window), cx);
        window.render_frame(cx);

        let initial = window.find("controls-mint-switch");
        assert_eq!(initial.label(), Some("Enable TUN"));
        assert_eq!(initial.checked(), Some(false));

        window.click("controls-mint-switch", cx);
        assert!(owner.read(cx).checked);
        assert_eq!(window.find("controls-mint-switch").checked(), Some(true));

        window.press("space", cx);
        assert!(!owner.read(cx).checked);
        assert_eq!(window.find("controls-mint-switch").checked(), Some(false));

        window.press("enter", cx);
        assert!(owner.read(cx).checked);
        assert_eq!(owner.read(cx).changes, 3);

        owner.update(cx, |state, cx| {
            state.disabled = true;
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(window.find("controls-mint-switch").disabled(), Some(true));

        window.click("controls-mint-switch", cx);
        window.press("space", cx);
        window.press("enter", cx);
        assert!(owner.read(cx).checked);
        assert_eq!(owner.read(cx).changes, 3);

        window.remove_window();
    })
    .unwrap();
}

struct WaveGallery;

impl Render for WaveGallery {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .p_4()
            .gap_3()
            .child(
                WavePercentage::new("wave-zero", "Zero")
                    .value(Some(0.))
                    .diameter(6.),
            )
            .child(
                WavePercentage::new("wave-mid", "Half")
                    .value(Some(50.))
                    .diameter(6.),
            )
            .child(
                WavePercentage::new("wave-full", "Full")
                    .value(Some(100.))
                    .diameter(6.),
            )
            .child(
                WavePercentage::new("wave-clamped-low", "Clamped Low")
                    .value(Some(-25.))
                    .diameter(6.),
            )
            .child(
                WavePercentage::new("wave-clamped-high", "Clamped High")
                    .value(Some(180.))
                    .diameter(6.),
            )
            .child(
                WavePercentage::new("wave-unknown", "Unknown")
                    .value(None)
                    .diameter(6.),
            )
            .child(
                WavePercentage::new("wave-nan", "NaN")
                    .value(Some(f32::NAN))
                    .diameter(6.),
            )
    }
}

#[gpui_kit::test]
fn wave_percentage_renders_boundary_clamped_and_unknown_states(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
    });
    let handle = cx.open_window(size(px(960.), px(220.)), |window, cx| {
        let view = cx.new(|_| WaveGallery);
        Root::new(view, window, cx)
    });

    cx.update_window(handle.into(), |_, window, cx| {
        apply_zen_theme(ThemeMode::Dark, Some(window), cx);
        window.render_frame(cx);

        for (id, expected_label) in [
            ("wave-zero", "Zero"),
            ("wave-mid", "Half"),
            ("wave-full", "Full"),
            ("wave-clamped-low", "Clamped Low"),
            ("wave-clamped-high", "Clamped High"),
            ("wave-unknown", "Unknown"),
            ("wave-nan", "NaN"),
        ] {
            let gauge = window.find(id);
            assert!(gauge.visible(), "expected {id} to be visible");
            assert_eq!(gauge.label(), Some(expected_label));
            assert!(gauge.bounds().size.width > px(0.));
            assert!(gauge.bounds().size.height > px(0.));
        }

        // Reduced motion must prevent animation frame scheduling even for mid-range values.
        assert_eq!(window.simulate_next_frame(cx), 0);
        window.remove_window();
    })
    .unwrap();
}

struct WorkspaceHost {
    page: Page,
    notifications: FeedbackNotifications,
}

impl Render for WorkspaceHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .p_4()
            .gap_3()
            .child(workspace::title(self.page, cx))
    }
}

#[gpui_kit::test]
fn feedback_notifications_deduplicate_and_recover_across_publish_cycles(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let mut host = None;
    let handle = cx.open_window(size(px(640.), px(320.)), |window, cx| {
        let view = cx.new(|_| WorkspaceHost {
            page: Page::Home,
            notifications: FeedbackNotifications::default(),
        });
        host = Some(view.clone());
        Root::new(view, window, cx)
    });
    let host = host.unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);

        host.update(cx, |state, cx| {
            state.notifications.publish(
                vec![
                    Feedback::new("request", FeedbackKind::Error, "Core offline".into()),
                    Feedback::new("startup", FeedbackKind::Error, "Core offline".into()),
                    Feedback::new("notice", FeedbackKind::Info, "Reloaded".into()),
                ],
                window,
                cx,
            );
            assert_eq!(
                state.notifications.current_message("request").as_deref(),
                Some("Core offline")
            );
            assert_eq!(
                state.notifications.current_message("notice").as_deref(),
                Some("Reloaded")
            );

            // Clearing active episodes represents recovery.
            state.notifications.publish(Vec::new(), window, cx);
            assert_eq!(state.notifications.current_message("request"), None);

            // After recovery, a fresh warning/success episode is accepted cleanly.
            state.notifications.publish(
                vec![
                    Feedback::new("request", FeedbackKind::Warning, "Degraded".into()),
                    Feedback::new("apply", FeedbackKind::Success, "Applied".into()),
                ],
                window,
                cx,
            );
            assert_eq!(
                state.notifications.current_message("request").as_deref(),
                Some("Degraded")
            );
            assert_eq!(
                state.notifications.current_message("apply").as_deref(),
                Some("Applied")
            );
        });

        window.render_frame(cx);
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn workspace_title_renders_for_primary_subtitle_and_settings_pages(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let mut host = None;
    let handle = cx.open_window(size(px(640.), px(240.)), |window, cx| {
        let view = cx.new(|_| WorkspaceHost {
            page: Page::Home,
            notifications: FeedbackNotifications::default(),
        });
        host = Some(view.clone());
        Root::new(view, window, cx)
    });
    let host = host.unwrap();

    for page in [Page::Home, Page::Logs, Page::Rules, Page::Settings] {
        cx.update_window(handle.into(), |_, window, cx| {
            host.update(cx, |state, cx| {
                state.page = page;
                cx.notify();
            });
            window.render_frame(cx);
            let title = window.find("workspace-page-title");
            assert!(
                title.visible(),
                "workspace title should render for {page:?}"
            );
            assert!(title.bounds().size.height > px(0.));
        })
        .unwrap();
    }

    cx.update_window(handle.into(), |_, window, _| window.remove_window())
        .unwrap();
}
