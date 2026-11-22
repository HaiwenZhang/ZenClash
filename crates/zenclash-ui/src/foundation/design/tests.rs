use super::{
    DARK_DANGER_FOREGROUND, DARK_FOREGROUND, DARK_INFO, DARK_MUTED_FOREGROUND, DARK_PANEL,
    DARK_RAISED, DARK_SIDEBAR, DARK_SIDEBAR_ACCENT, DARK_SIGNAL_FOREGROUND, DARK_SUCCESS,
    DARK_SUCCESS_FOREGROUND, DARK_WARNING_FOREGROUND, DEEP_INK, FAULT_CORAL, LIGHT_CANVAS,
    LIGHT_DANGER, LIGHT_INFO, LIGHT_INK, LIGHT_MUTED_INK, LIGHT_PANEL, LIGHT_RAISED, LIGHT_SIDEBAR,
    LIGHT_SIDEBAR_ACCENT, LIGHT_SIGNAL, LIGHT_SUCCESS, LIGHT_UPLINK, SIGNAL_CYAN, UPLINK_AMBER,
    color, throughput_activity_percent,
};

fn contrast_ratio(left: u32, right: u32) -> f64 {
    let left = relative_luminance(left);
    let right = relative_luminance(right);
    (left.max(right) + 0.05) / (left.min(right) + 0.05)
}

fn relative_luminance(rgb: u32) -> f64 {
    let channel = |shift| {
        let value = f64::from(u8::try_from((rgb >> shift) & 0xff_u32).unwrap_or_default()) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

#[gpui_kit::test]
fn desktop_surfaces_follow_the_product_palette_in_both_modes(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::{ActiveTheme, ThemeMode};
    cx.update(gpui_kit::init);
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        cx.update(|cx| {
            super::apply_zen_theme(mode, None, cx);
            let theme = cx.theme();
            let dark = mode.is_dark();
            assert_eq!(theme.group_box, theme.secondary);
            assert_eq!(theme.group_box_foreground, theme.foreground);
            assert_ne!(theme.info, theme.primary);
            assert_eq!(theme.tab_active, theme.sidebar_accent);
            assert_eq!(theme.tab_active_foreground, theme.primary);
            assert_eq!(theme.tokens.button_primary.color, theme.primary);
            assert_eq!(theme.button_primary_foreground, theme.primary_foreground);
            assert_eq!(
                theme.success_foreground,
                color(if dark {
                    DARK_SUCCESS_FOREGROUND
                } else {
                    LIGHT_PANEL
                })
            );
            assert_eq!(
                theme.warning_foreground,
                color(if dark {
                    DARK_WARNING_FOREGROUND
                } else {
                    LIGHT_PANEL
                })
            );
            assert_eq!(
                theme.danger_foreground,
                color(if dark {
                    DARK_DANGER_FOREGROUND
                } else {
                    LIGHT_PANEL
                })
            );
            assert_eq!(
                theme.info_foreground,
                color(if dark { DEEP_INK } else { LIGHT_PANEL })
            );
            assert!(theme.radius < theme.radius_lg);
            assert!(theme.shadow);
            assert_eq!(super::online_indicator(theme), theme.success);
            assert_eq!(
                super::brand_accent(theme),
                color(if dark { SIGNAL_CYAN } else { super::LIGHT_MINT })
            );
        });
    }
}

#[test]
fn all_semantic_foreground_background_pairs_meet_wcag_aa_in_both_themes() {
    let pairs = [
        ("light.foreground/background", LIGHT_INK, LIGHT_CANVAS),
        ("light.foreground/panel", LIGHT_INK, LIGHT_PANEL),
        ("light.foreground/raised", LIGHT_INK, LIGHT_RAISED),
        ("light.foreground/sidebar", LIGHT_INK, LIGHT_SIDEBAR),
        ("light.muted/background", LIGHT_MUTED_INK, LIGHT_CANVAS),
        ("light.muted/panel", LIGHT_MUTED_INK, LIGHT_PANEL),
        ("light.muted/raised", LIGHT_MUTED_INK, LIGHT_RAISED),
        ("light.muted/sidebar", LIGHT_MUTED_INK, LIGHT_SIDEBAR),
        ("light.primary", LIGHT_PANEL, LIGHT_SIGNAL),
        ("light.success", LIGHT_PANEL, LIGHT_SUCCESS),
        ("light.warning", LIGHT_PANEL, LIGHT_UPLINK),
        ("light.danger", LIGHT_PANEL, LIGHT_DANGER),
        ("light.info", LIGHT_PANEL, LIGHT_INFO),
        ("light.sidebar_accent", LIGHT_SIGNAL, LIGHT_SIDEBAR_ACCENT),
        ("dark.foreground/background", DARK_FOREGROUND, DEEP_INK),
        ("dark.foreground/panel", DARK_FOREGROUND, DARK_PANEL),
        ("dark.foreground/raised", DARK_FOREGROUND, DARK_RAISED),
        ("dark.foreground/sidebar", DARK_FOREGROUND, DARK_SIDEBAR),
        ("dark.muted/background", DARK_MUTED_FOREGROUND, DEEP_INK),
        ("dark.muted/panel", DARK_MUTED_FOREGROUND, DARK_PANEL),
        ("dark.muted/raised", DARK_MUTED_FOREGROUND, DARK_RAISED),
        ("dark.muted/sidebar", DARK_MUTED_FOREGROUND, DARK_SIDEBAR),
        ("dark.primary", DARK_SIGNAL_FOREGROUND, SIGNAL_CYAN),
        ("dark.success", DARK_SUCCESS_FOREGROUND, DARK_SUCCESS),
        ("dark.warning", DARK_WARNING_FOREGROUND, UPLINK_AMBER),
        ("dark.danger", DARK_DANGER_FOREGROUND, FAULT_CORAL),
        ("dark.info", DEEP_INK, DARK_INFO),
        ("dark.sidebar_accent", SIGNAL_CYAN, DARK_SIDEBAR_ACCENT),
    ];

    for (name, fg, bg) in pairs {
        let ratio = contrast_ratio(fg, bg);
        assert!(
            ratio >= 4.5,
            "semantic color pair {name} failed WCAG AA contrast: {ratio:.2}:1 (< 4.5:1)"
        );
    }
}

#[test]
fn activity_percent_is_bounded_without_float_casts() {
    assert!((throughput_activity_percent(0) - 0.0).abs() < f32::EPSILON);
    assert!((throughput_activity_percent(2 * 1024 * 1024) - 50.0).abs() < f32::EPSILON);
    assert!((throughput_activity_percent(u64::MAX) - 100.0).abs() < f32::EPSILON);
}

#[test]
fn light_body_text_exceeds_wcag_aaa_contrast() {
    assert!(contrast_ratio(LIGHT_INK, LIGHT_CANVAS) >= 7.0);
}

#[test]
fn light_muted_text_meets_wcag_aa_contrast() {
    assert!(contrast_ratio(LIGHT_MUTED_INK, LIGHT_CANVAS) >= 4.5);
}

#[test]
fn light_signal_text_meets_wcag_aa_contrast() {
    assert!(contrast_ratio(LIGHT_SIGNAL, LIGHT_CANVAS) >= 4.5);
}

#[test]
fn dark_body_text_exceeds_wcag_aaa_contrast() {
    assert!(contrast_ratio(DARK_FOREGROUND, DEEP_INK) >= 7.0);
}

#[test]
fn dark_muted_text_meets_wcag_aa_contrast() {
    assert!(contrast_ratio(DARK_MUTED_FOREGROUND, DEEP_INK) >= 4.5);
}

#[test]
fn dark_signal_text_meets_wcag_aa_contrast() {
    assert!(contrast_ratio(SIGNAL_CYAN, DEEP_INK) >= 4.5);
}
