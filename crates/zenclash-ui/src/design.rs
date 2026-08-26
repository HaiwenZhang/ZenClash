use gpui::{rgb, App, Hsla, Window};
use gpui_component::{Theme, ThemeMode};

/// Primary dark background used by the `ZenClash` visual system.
pub const DEEP_INK: u32 = 0x0007_1218;
/// Cyan accent used for downstream traffic and active controls.
pub const SIGNAL_CYAN: u32 = 0x0029_D3C2;
/// Amber accent used for upstream traffic and warnings.
pub const UPLINK_AMBER: u32 = 0x00F2_B84B;
/// Coral accent used for errors and destructive states.
pub const FAULT_CORAL: u32 = 0x00FF_6B64;

const DARK_PANEL: u32 = 0x000C_1C24;
const DARK_RAISED: u32 = 0x0010_2832;
const DARK_BORDER: u32 = 0x001B_3942;
const DARK_FOREGROUND: u32 = 0x00DC_E9E8;
const DARK_MUTED_FOREGROUND: u32 = 0x0082_9B9D;
const LIGHT_CANVAS: u32 = 0x00F4_F7FA;
const LIGHT_PANEL: u32 = 0x00FF_FFFF;
const LIGHT_RAISED: u32 = 0x00E9_EFF5;
const LIGHT_BORDER: u32 = 0x00D5_DEE7;
const LIGHT_INK: u32 = 0x0017_2635;
const LIGHT_MUTED_INK: u32 = 0x0060_7386;
const LIGHT_SIGNAL: u32 = 0x000B_7A71;
const LIGHT_UPLINK: u32 = 0x009B_650E;
const LIGHT_SUCCESS: u32 = 0x0016_7956;
const LIGHT_DANGER: u32 = 0x00BD_3F3F;
const LIGHT_SIDEBAR: u32 = 0x00ED_F2F7;

/// Converts a packed RGB value into GPUI's HSLA representation.
#[must_use]
pub fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// Maps combined throughput to the zero-to-100 activity scale used by
/// `gpui-component` progress indicators.
#[must_use]
pub fn throughput_activity_percent(bytes_per_second: u64) -> f32 {
    const FULL_ACTIVITY_BYTES: u128 = 4 * 1024 * 1024;

    let percent = (u128::from(bytes_per_second) * 100 / FULL_ACTIVITY_BYTES).min(100);
    f32::from(u8::try_from(percent).unwrap_or(100))
}

/// Applies the `ZenClash` "network oscilloscope" palette to gpui-component so
/// every stock component participates in the same visual system.
pub fn apply_zen_theme(mode: ThemeMode, window: Option<&mut Window>, cx: &mut App) {
    Theme::change(mode, None, cx);
    let dark = mode.is_dark();
    let theme = Theme::global_mut(cx);

    let background = color(if dark { DEEP_INK } else { LIGHT_CANVAS });
    let panel = color(if dark { DARK_PANEL } else { LIGHT_PANEL });
    let raised = color(if dark { DARK_RAISED } else { LIGHT_RAISED });
    let border = color(if dark { DARK_BORDER } else { LIGHT_BORDER });
    let foreground = color(if dark { DARK_FOREGROUND } else { LIGHT_INK });
    let muted_foreground = color(if dark {
        DARK_MUTED_FOREGROUND
    } else {
        LIGHT_MUTED_INK
    });
    let signal = color(if dark { SIGNAL_CYAN } else { LIGHT_SIGNAL });
    let signal_foreground = color(if dark { 0x0003_110F } else { LIGHT_PANEL });
    let amber = color(if dark { UPLINK_AMBER } else { LIGHT_UPLINK });
    let coral = color(if dark { FAULT_CORAL } else { LIGHT_DANGER });
    let success = color(if dark { 0x0043_C98B } else { LIGHT_SUCCESS });
    let sidebar = color(if dark { 0x0009_171E } else { LIGHT_SIDEBAR });
    let sidebar_accent = if dark { raised } else { panel };

    configure_theme_metrics(theme);

    let colors = &mut theme.colors;
    colors.background = background;
    colors.foreground = foreground;
    colors.secondary = panel;
    colors.secondary_foreground = foreground;
    colors.secondary_hover = raised;
    colors.secondary_active = raised;
    colors.muted = raised;
    colors.muted_foreground = muted_foreground;
    colors.border = border;
    colors.input = border;
    colors.ring = signal;
    colors.caret = signal;
    colors.primary = signal;
    colors.primary_foreground = signal_foreground;
    colors.primary_hover = signal.opacity(0.84);
    colors.primary_active = signal.opacity(0.72);
    colors.accent = raised;
    colors.accent_foreground = foreground;
    colors.success = success;
    colors.success_foreground = color(0x0004_140D);
    colors.success_hover = success.opacity(0.84);
    colors.success_active = success.opacity(0.72);
    colors.warning = amber;
    colors.warning_foreground = color(0x0021_1600);
    colors.warning_hover = amber.opacity(0.84);
    colors.warning_active = amber.opacity(0.72);
    colors.danger = coral;
    colors.danger_foreground = color(0x0026_0504);
    colors.danger_hover = coral.opacity(0.84);
    colors.danger_active = coral.opacity(0.72);
    colors.info = signal;
    colors.info_foreground = signal_foreground;
    colors.info_hover = signal.opacity(0.84);
    colors.info_active = signal.opacity(0.72);
    colors.progress_bar = signal;
    colors.sidebar = sidebar;
    colors.sidebar_foreground = foreground;
    colors.sidebar_border = border;
    colors.sidebar_accent = sidebar_accent;
    colors.sidebar_accent_foreground = foreground;
    colors.sidebar_primary = signal;
    colors.sidebar_primary_foreground = signal_foreground;
    colors.title_bar = if dark { background } else { panel };
    colors.title_bar_border = border;
    colors.list = panel;
    colors.list_head = raised;
    colors.list_even = background.opacity(0.36);
    colors.list_hover = raised;
    colors.list_active = signal.opacity(0.14);
    colors.list_active_border = signal;
    colors.table = panel;
    colors.table_head = raised;
    colors.table_head_foreground = muted_foreground;
    colors.table_even = background.opacity(0.36);
    colors.table_hover = raised;
    colors.table_active = signal.opacity(0.14);
    colors.table_active_border = signal;
    colors.table_row_border = border;
    colors.switch = border;
    colors.switch_thumb = foreground;
    colors.tab = panel;
    colors.tab_active = signal;
    colors.tab_active_foreground = signal_foreground;
    colors.tab_bar = panel;
    colors.tab_bar_segmented = raised;
    colors.tab_foreground = muted_foreground;
    colors.popover = panel;
    colors.popover_foreground = foreground;
    colors.overlay = color(0x0002_090C).opacity(0.72);
    colors.scrollbar = background;
    colors.scrollbar_thumb = border;
    colors.scrollbar_thumb_hover = muted_foreground;
    colors.selection = signal.opacity(0.28);
    colors.chart_1 = signal;
    colors.chart_2 = amber;
    colors.chart_3 = success;
    colors.chart_4 = coral;
    colors.chart_5 = color(0x007B_A7FF);

    if let Some(window) = window {
        window.refresh();
    }
}

fn configure_theme_metrics(theme: &mut Theme) {
    theme.font_family = ".SystemUIFont".into();
    theme.mono_font_family = "SF Mono".into();
    theme.font_size = gpui::px(15.);
    theme.mono_font_size = gpui::px(12.);
    theme.radius = gpui::px(8.);
    theme.radius_lg = gpui::px(12.);
    theme.shadow = !theme.mode.is_dark();
    theme.tile_shadow = false;
}

#[cfg(test)]
fn contrast_ratio(left: u32, right: u32) -> f64 {
    let left = relative_luminance(left);
    let right = relative_luminance(right);
    (left.max(right) + 0.05) / (left.min(right) + 0.05)
}

#[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::{
        contrast_ratio, throughput_activity_percent, DARK_FOREGROUND, DARK_MUTED_FOREGROUND,
        DEEP_INK, LIGHT_CANVAS, LIGHT_INK, LIGHT_MUTED_INK, LIGHT_SIGNAL, SIGNAL_CYAN,
    };

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
}
