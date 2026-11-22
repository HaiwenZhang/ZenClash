use gpui_kit::component::{ActiveTheme, Root, ThemeMode, h_flex, v_flex};
use gpui_kit::test::{TestSupportExt, TestWindowExt};
use gpui_kit::{
    AppContext, Context, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, TestAppContext, Window, div, px, rems, size,
};
use zenclash_ui::components::surface::{
    StatusTone, copy_button, detail_row, status_pill, surface_card, table_fixed_header_cell,
};
use zenclash_ui::foundation::design::{
    DEEP_INK, FAULT_CORAL, SIGNAL_CYAN, UPLINK_AMBER, apply_zen_theme, brand_accent, color,
    online_indicator, throughput_activity_percent,
};

struct SurfaceShowcase {
    clipboard_payload: String,
}

impl Render for SurfaceShowcase {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tones = [
            ("pill-neutral", "Neutral", StatusTone::Neutral),
            ("pill-accent", "Accent", StatusTone::Accent),
            ("pill-success", "Online", StatusTone::Success),
            ("pill-warning", "Degraded", StatusTone::Warning),
            ("pill-danger", "Failed", StatusTone::Danger),
            ("pill-info", "Info", StatusTone::Info),
        ];

        div().size_full().p_4().child(
            surface_card(&theme)
                .id("showcase-card")
                .test_support()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            table_fixed_header_cell(rems(8.), "Protocol")
                                .id("table-col-protocol")
                                .test_support(),
                        )
                        .child(
                            table_fixed_header_cell(rems(10.), "Target")
                                .id("table-col-target")
                                .test_support(),
                        ),
                )
                .child(detail_row(
                    "Status",
                    h_flex()
                        .gap_2()
                        .children(tones.into_iter().map(|(id, label, tone)| {
                            status_pill(label, tone, &theme)
                                .id(id)
                                .test_support()
                                .aria_label(label)
                        })),
                    &theme,
                ))
                .child(v_flex().gap_2().child(copy_button(
                    "showcase-copy-btn",
                    "Copy Endpoint",
                    self.clipboard_payload.clone(),
                    "Copied endpoint",
                ))),
        )
    }
}

fn relative_luminance(rgb: u32) -> f64 {
    let channel = |shift: u32| {
        let value = f64::from(u8::try_from((rgb >> shift) & 0xff).unwrap_or_default()) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

fn contrast_ratio(left: u32, right: u32) -> f64 {
    let left = relative_luminance(left);
    let right = relative_luminance(right);
    (left.max(right) + 0.05) / (left.min(right) + 0.05)
}

#[gpui_kit::test]
fn apply_zen_theme_switches_light_and_dark_surfaces_in_headless_window(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let handle = cx.open_window(size(px(720.), px(360.)), |window, cx| {
        let view = cx.new(|_| SurfaceShowcase {
            clipboard_payload: "http://127.0.0.1:9090".into(),
        });
        Root::new(view, window, cx)
    });

    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        cx.update_window(handle.into(), |_, window, cx| {
            apply_zen_theme(mode, Some(window), cx);
            window.render_frame(cx);

            let theme = cx.theme();
            assert_eq!(theme.mode, mode);
            assert_eq!(theme.group_box, theme.secondary);
            assert_eq!(online_indicator(theme), theme.success);
            assert!(theme.radius < theme.radius_lg);
            assert!(theme.shadow);
            if mode.is_dark() {
                assert_eq!(brand_accent(theme), color(SIGNAL_CYAN));
                assert_eq!(theme.background, color(DEEP_INK));
            } else {
                assert_ne!(brand_accent(theme), color(SIGNAL_CYAN));
                assert_ne!(theme.background, color(DEEP_INK));
            }

            let card = window.find("showcase-card");
            assert!(card.visible());
            assert!(card.bounds().size.width > px(0.));

            for pill_id in [
                "pill-neutral",
                "pill-accent",
                "pill-success",
                "pill-warning",
                "pill-danger",
                "pill-info",
            ] {
                let pill = window.find(pill_id);
                assert!(pill.visible(), "expected {pill_id} to be visible");
                assert!(pill.bounds().size.width > px(0.));
            }

            let col_left = window.find("table-col-protocol").bounds();
            let col_right = window.find("table-col-target").bounds();
            assert!(col_right.left() >= col_left.right());
        })
        .unwrap();
    }

    cx.update_window(handle.into(), |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn copy_button_writes_payload_to_clipboard(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let handle = cx.open_window(size(px(640.), px(320.)), |window, cx| {
        let view = cx.new(|_| SurfaceShowcase {
            clipboard_payload: "export http_proxy=http://127.0.0.1:7890".into(),
        });
        Root::new(view, window, cx)
    });

    cx.update_window(handle.into(), |_, window, cx| {
        apply_zen_theme(ThemeMode::Light, Some(window), cx);
        window.render_frame(cx);
        window.click("showcase-copy-btn", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("export http_proxy=http://127.0.0.1:7890".to_owned())
        );
        window.remove_window();
    })
    .unwrap();
}

#[test]
fn design_accents_meet_wcag_aa_on_deep_ink_and_throughput_activity_is_clamped() {
    for (name, accent) in [
        ("SIGNAL_CYAN", SIGNAL_CYAN),
        ("UPLINK_AMBER", UPLINK_AMBER),
        ("FAULT_CORAL", FAULT_CORAL),
    ] {
        let ratio = contrast_ratio(accent, DEEP_INK);
        assert!(
            ratio >= 4.5,
            "{name} on DEEP_INK must meet WCAG AA (>= 4.5:1), got {ratio:.2}:1"
        );
    }

    assert!((throughput_activity_percent(0) - 0.0).abs() < f32::EPSILON);
    assert!((throughput_activity_percent(2 * 1024 * 1024) - 50.0).abs() < f32::EPSILON);
    assert!((throughput_activity_percent(4 * 1024 * 1024) - 100.0).abs() < f32::EPSILON);
    assert!((throughput_activity_percent(u64::MAX) - 100.0).abs() < f32::EPSILON);
}
