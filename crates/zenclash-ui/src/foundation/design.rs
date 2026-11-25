use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{App, Hsla, Window, rgb};

/// Primary dark background used by the `ZenClash` visual system.
pub const DEEP_INK: u32 = 0x0010_161D;
/// Mint accent used for branding and active controls.
pub const SIGNAL_CYAN: u32 = 0x0058_D2BF;
/// Amber accent used for warnings and attention states.
pub const UPLINK_AMBER: u32 = 0x00F2_B84B;
/// Coral accent used for errors and destructive states.
pub const FAULT_CORAL: u32 = 0x00FF_6B64;

const DARK_PANEL: u32 = 0x0019_2732;
const DARK_RAISED: u32 = 0x0021_3342;
const DARK_BORDER: u32 = 0x002E_475A;
const DARK_FOREGROUND: u32 = 0x00EC_F2F8;
const DARK_MUTED_FOREGROUND: u32 = 0x00A8_B7C6;
const DARK_SIGNAL_FOREGROUND: u32 = 0x0003_110F;
const DARK_SUCCESS: u32 = 0x0043_C98B;
const DARK_SUCCESS_FOREGROUND: u32 = 0x0004_140D;
const DARK_WARNING_FOREGROUND: u32 = 0x0021_1600;
const DARK_DANGER_FOREGROUND: u32 = 0x0026_0504;
const DARK_INFO: u32 = 0x0062_A8FF;
const DARK_SIDEBAR: u32 = 0x0013_1C23;
const DARK_SIDEBAR_ACCENT: u32 = 0x0016_3B3B;
const LIGHT_CANVAS: u32 = 0x00F0_FCF7;
pub(crate) const LIGHT_PANEL: u32 = 0x00FF_FFFF;
const LIGHT_RAISED: u32 = 0x00F1_F7F9;
const LIGHT_BORDER: u32 = 0x00DF_E9F1;
const LIGHT_INK: u32 = 0x000D_1B43;
const LIGHT_MUTED_INK: u32 = 0x0057_6B93;
const LIGHT_SIGNAL: u32 = 0x000B_7A71;
const LIGHT_UPLINK: u32 = 0x009B_650E;
const LIGHT_SUCCESS: u32 = 0x0016_7956;
const LIGHT_DANGER: u32 = 0x00BD_3F3F;
const LIGHT_INFO: u32 = 0x0024_6FDD;
const LIGHT_SIDEBAR: u32 = 0x00F7_FDFB;
const LIGHT_SIDEBAR_ACCENT: u32 = 0x00DA_F9EC;
const LIGHT_MINT: u32 = 0x0000_CFA6;

/// Converts a packed RGB value into GPUI's HSLA representation.
#[must_use]
pub fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// Product mint brand accent for active switches, progress rings, and highlights.
///
/// Use this token instead of chart palette colors (`theme.chart_3`) for UI controls.
#[must_use]
pub fn brand_accent(theme: &Theme) -> Hsla {
    if theme.mode.is_dark() {
        color(SIGNAL_CYAN)
    } else {
        color(LIGHT_MINT)
    }
}

/// Semantic live/connected status indicator color aligned across all pages and chrome.
#[must_use]
pub fn online_indicator(theme: &Theme) -> Hsla {
    theme.success
}

/// Subtle light canvas transition used by the approved desktop drafts.
pub(crate) fn workspace_background(theme: &Theme) -> gpui_kit::Background {
    if theme.mode.is_dark() {
        theme.background.into()
    } else {
        gpui_kit::linear_gradient(
            135.,
            gpui_kit::linear_color_stop(theme.sidebar, 0.),
            gpui_kit::linear_color_stop(theme.background, 1.),
        )
    }
}

/// Maps combined throughput to the zero-to-100 activity scale used by
/// `GPUI Kit` progress indicators.
#[must_use]
pub fn throughput_activity_percent(bytes_per_second: u64) -> f32 {
    const FULL_ACTIVITY_BYTES: u128 = 4 * 1024 * 1024;

    let percent = (u128::from(bytes_per_second) * 100 / FULL_ACTIVITY_BYTES).min(100);
    f32::from(u8::try_from(percent).unwrap_or(100))
}

/// Applies the approved `ZenClash` desktop palette to GPUI Kit so
/// every stock component participates in the same visual system.
pub fn apply_zen_theme(mode: ThemeMode, window: Option<&mut Window>, cx: &mut App) {
    Theme::change(mode, None, cx);
    let dark = mode.is_dark();
    Theme::update(cx, |theme| {
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
        let signal_foreground = color(if dark {
            DARK_SIGNAL_FOREGROUND
        } else {
            LIGHT_PANEL
        });
        let amber = color(if dark { UPLINK_AMBER } else { LIGHT_UPLINK });
        let coral = color(if dark { FAULT_CORAL } else { LIGHT_DANGER });
        let success = color(if dark { DARK_SUCCESS } else { LIGHT_SUCCESS });
        let sidebar = color(if dark { DARK_SIDEBAR } else { LIGHT_SIDEBAR });
        let sidebar_accent = color(if dark {
            DARK_SIDEBAR_ACCENT
        } else {
            LIGHT_SIDEBAR_ACCENT
        });

        configure_theme_metrics(theme);

        let colors = &mut theme.colors;
        colors.background = background;
        colors.foreground = foreground;
        colors.secondary = panel;
        colors.secondary_foreground = foreground;
        colors.secondary_hover = raised;
        colors.secondary_active = raised;
        colors.group_box = panel;
        colors.group_box_foreground = foreground;
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
        colors.button_primary = signal;
        colors.button_primary_foreground = signal_foreground;
        colors.button_primary_hover = colors.primary_hover;
        colors.button_primary_active = colors.primary_active;
        colors.accent = raised;
        colors.accent_foreground = foreground;
        colors.success = success;
        colors.success_foreground = color(if dark {
            DARK_SUCCESS_FOREGROUND
        } else {
            LIGHT_PANEL
        });
        colors.success_hover = success.opacity(0.84);
        colors.success_active = success.opacity(0.72);
        colors.warning = amber;
        colors.warning_foreground = color(if dark {
            DARK_WARNING_FOREGROUND
        } else {
            LIGHT_PANEL
        });
        colors.warning_hover = amber.opacity(0.84);
        colors.warning_active = amber.opacity(0.72);
        colors.danger = coral;
        colors.danger_foreground = color(if dark {
            DARK_DANGER_FOREGROUND
        } else {
            LIGHT_PANEL
        });
        colors.danger_hover = coral.opacity(0.84);
        colors.danger_active = coral.opacity(0.72);
        let info = color(if dark { DARK_INFO } else { LIGHT_INFO });
        colors.info = info;
        colors.info_foreground = color(if dark { DEEP_INK } else { LIGHT_PANEL });
        colors.info_hover = info.opacity(0.84);
        colors.info_active = info.opacity(0.72);
        colors.progress_bar = signal;
        colors.sidebar = sidebar;
        colors.sidebar_foreground = foreground;
        colors.sidebar_border = border;
        colors.sidebar_accent = sidebar_accent;
        colors.sidebar_accent_foreground = signal;
        colors.sidebar_primary = signal;
        colors.sidebar_primary_foreground = signal_foreground;
        colors.title_bar = if dark { sidebar } else { panel };
        colors.title_bar_border = border;
        colors.list = panel;
        colors.list_head = raised;
        colors.list_even = background.opacity(0.36);
        colors.list_hover = raised;
        colors.list_active = if dark {
            signal.opacity(0.14)
        } else {
            sidebar_accent
        };
        colors.list_active_border = signal;
        colors.table = panel;
        colors.table_head = raised;
        colors.table_head_foreground = muted_foreground;
        colors.table_even = background.opacity(0.36);
        colors.table_hover = raised;
        colors.table_active = if dark {
            signal.opacity(0.14)
        } else {
            sidebar_accent
        };
        colors.table_active_border = signal;
        colors.table_row_border = border;
        colors.switch = border;
        colors.switch_thumb = foreground;
        colors.tab = panel;
        colors.tab_active = sidebar_accent;
        colors.tab_active_foreground = signal;
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
        colors.chart_1 = color(if dark { 0x0062_A8FF } else { 0x003B_82F6 });
        colors.chart_2 = color(if dark { 0x0084_A0FF } else { 0x0095_45FF });
        colors.chart_3 = if dark { signal } else { color(LIGHT_MINT) };
        colors.chart_4 = coral;
        colors.chart_5 = color(if dark { 0x00A7_8BFA } else { 0x008B_5CF6 });
    });

    if let Some(window) = window {
        window.refresh();
    }
}

fn configure_theme_metrics(theme: &mut Theme) {
    #[cfg(target_os = "macos")]
    {
        theme.font_family = ".SystemUIFont".into();
        theme.mono_font_family = "SF Mono".into();
    }
    #[cfg(target_os = "windows")]
    {
        theme.font_family = "Segoe UI".into();
        theme.mono_font_family = "Cascadia Mono".into();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        theme.font_family = "sans-serif".into();
        theme.mono_font_family = "monospace".into();
    }
    theme.font_size = gpui_kit::px(16.);
    theme.mono_font_size = gpui_kit::px(12.);
    theme.radius = gpui_kit::px(6.);
    theme.radius_lg = gpui_kit::px(10.);
    theme.shadow = true;
}

#[cfg(test)]
mod tests;
