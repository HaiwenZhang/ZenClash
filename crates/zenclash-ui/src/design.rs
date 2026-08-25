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

    let background = color(if dark { DEEP_INK } else { 0x00F1_F7F6 });
    let panel = color(if dark { 0x000C_1C24 } else { 0x00FF_FFFF });
    let raised = color(if dark { 0x0010_2832 } else { 0x00E4_EFED });
    let border = color(if dark { 0x001B_3942 } else { 0x00C7_DAD7 });
    let foreground = color(if dark { 0x00DC_E9E8 } else { 0x0013_2B30 });
    let muted_foreground = color(if dark { 0x0082_9B9D } else { 0x0062_7C7E });
    let signal = color(SIGNAL_CYAN);
    let signal_foreground = color(if dark { 0x0003_110F } else { 0x0006_201D });
    let amber = color(UPLINK_AMBER);
    let coral = color(FAULT_CORAL);
    let success = color(if dark { 0x0043_C98B } else { 0x0016_8A5B });

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
    colors.sidebar = color(if dark { 0x0009_171E } else { 0x00EA_F3F1 });
    colors.sidebar_foreground = foreground;
    colors.sidebar_border = border;
    colors.sidebar_accent = raised;
    colors.sidebar_accent_foreground = foreground;
    colors.sidebar_primary = signal;
    colors.sidebar_primary_foreground = signal_foreground;
    colors.title_bar = background;
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
    theme.shadow = false;
    theme.tile_shadow = false;
}

#[cfg(test)]
mod tests {
    use super::throughput_activity_percent;

    #[test]
    fn activity_percent_is_bounded_without_float_casts() {
        assert!((throughput_activity_percent(0) - 0.0).abs() < f32::EPSILON);
        assert!((throughput_activity_percent(2 * 1024 * 1024) - 50.0).abs() < f32::EPSILON);
        assert!((throughput_activity_percent(u64::MAX) - 100.0).abs() < f32::EPSILON);
    }
}
