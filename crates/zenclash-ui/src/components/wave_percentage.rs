use std::{f32::consts::TAU, time::Duration};

use gpui_kit::base::Progress;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    Animation, AnimationExt, App, ElementId, FontWeight, IntoElement, ParentElement, PathBuilder,
    RenderOnce, SharedString, Styled, Window, canvas, div, point, px, rems,
};

/// Controlled circular percentage indicator with two clipped water waves.
///
/// The caller owns the value and localized label. Unknown or non-finite values
/// display a dash, not zero. Animation respects GPUI's reduced-motion preference
/// and is only mounted for a known value strictly between zero and 100 percent.
#[derive(IntoElement)]
pub struct WavePercentage {
    id: ElementId,
    value: Option<f32>,
    label: SharedString,
    diameter: f32,
    animated: bool,
}

impl WavePercentage {
    /// Creates an indicator with a stable ID and an accessible, localized label.
    /// The default diameter is eight rems; the default value is unknown.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            value: None,
            label: label.into(),
            diameter: 8.,
            animated: true,
        }
    }

    /// Sets the percentage, clamping finite values to `0..=100`.
    #[must_use]
    pub fn value(mut self, value: Option<f32>) -> Self {
        self.value = normalize(value);
        self
    }

    /// Sets the diameter in rems. Invalid sizes keep the default geometry.
    #[must_use]
    pub fn diameter(mut self, rems: f32) -> Self {
        if rems.is_finite() && rems > 0. {
            self.diameter = rems;
        }
        self
    }

    /// Enables water movement. Disable for paused, hidden, or stale content.
    #[must_use]
    pub fn animated(mut self, animated: bool) -> Self {
        self.animated = animated;
        self
    }
}

impl RenderOnce for WavePercentage {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let value = self.value;
        let waves = WaveSurface { value, phase: 0. };
        let waves = if self.animated
            && window.is_window_active()
            && value.is_some_and(|value| value > 0. && value < 100.)
        {
            waves
                .with_animation(
                    "water-phase",
                    Animation::new(Duration::from_secs(4))
                        .repeat()
                        .with_max_fps(30.),
                    |mut surface, phase| {
                        surface.phase = phase;
                        surface
                    },
                )
                .into_any_element()
        } else {
            waves.into_any_element()
        };
        Progress::new(self.id)
            .accessibility_label(self.label.clone())
            .value(value.unwrap_or(0.))
            .indeterminate(value.is_none())
            .relative()
            .size(rems(self.diameter))
            .flex_shrink_0()
            .rounded_full()
            .border_1()
            .border_color(crate::foundation::design::brand_accent(cx.theme()).opacity(0.65))
            .bg(cx.theme().group_box)
            .child(waves)
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .text_color(cx.theme().foreground)
                    .child(
                        div().text_2xl().font_weight(FontWeight::SEMIBOLD).child(
                            value.map_or_else(|| "—".into(), |value| format!("{value:.1}%")),
                        ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.label),
                    ),
            )
    }
}

#[derive(IntoElement)]
struct WaveSurface {
    value: Option<f32>,
    phase: f32,
}

impl RenderOnce for WaveSurface {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = crate::foundation::design::brand_accent(cx.theme());
        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                let Some(value) = self.value.filter(|value| *value > 0.) else {
                    return;
                };
                // Pixels here are resolved canvas geometry, not layout constants.
                let diameter = f32::from(bounds.size.width.min(bounds.size.height));
                let origin = bounds.center() - point(px(diameter / 2.), px(diameter / 2.));
                for (offset, opacity) in [(0., 0.3), (0.4, 0.55)] {
                    let mut path = PathBuilder::fill();
                    for ix in 0..=128_u16 {
                        let x = f32::from(ix) / 128.;
                        let (top, _) = water_column(x, value / 100., self.phase + offset);
                        let p = origin + point(px(x * diameter), px(top * diameter));
                        if ix == 0 {
                            path.move_to(p);
                        } else {
                            path.line_to(p);
                        }
                    }
                    for ix in (0..=128_u16).rev() {
                        let x = f32::from(ix) / 128.;
                        let (_, bottom) = water_column(x, value / 100., self.phase + offset);
                        path.line_to(origin + point(px(x * diameter), px(bottom * diameter)));
                    }
                    path.close();
                    if let Ok(path) = path.build() {
                        window.paint_path(path, color.opacity(opacity));
                    }
                }
            },
        )
        .absolute()
        .inset_0()
        .size_full()
    }
}

fn normalize(value: Option<f32>) -> Option<f32> {
    value
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0., 100.))
}

// Intersect each vertical water column with the circle analytically. Rounded
// parent clipping does not clip arbitrary canvas paths in GPUI.
fn water_column(x: f32, fraction: f32, phase: f32) -> (f32, f32) {
    let half_chord = (0.25 - (x - 0.5).powi(2)).max(0.).sqrt();
    let top = 0.5 - half_chord;
    let bottom = 0.5 + half_chord;
    let amplitude = 0.035_f32.min(fraction * 0.2).min((1. - fraction) * 0.2);
    let surface = 1. - fraction + amplitude * ((x + phase) * TAU).sin();
    (surface.clamp(top, bottom), bottom)
}

#[cfg(test)]
mod tests;
