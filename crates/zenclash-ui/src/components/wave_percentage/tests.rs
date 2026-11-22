use super::{normalize, water_column};

struct WaveHost;

impl gpui_kit::Render for WaveHost {
    fn render(
        &mut self,
        _: &mut gpui_kit::Window,
        _: &mut gpui_kit::Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        super::WavePercentage::new("quota", "Used quota").value(Some(34.2))
    }
}

#[gpui_kit::test]
fn reduced_motion_does_not_schedule_animation_frames(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext;
    cx.update(gpui_kit::init);
    cx.update(|cx| cx.set_reduce_motion(true));
    let window = cx.open_window(
        gpui_kit::size(gpui_kit::px(240.), gpui_kit::px(240.)),
        |window, cx| {
            let view = cx.new(|_| WaveHost);
            gpui_kit::component::Root::new(view, window, cx)
        },
    );
    cx.run_until_parked();
    assert_eq!(
        window
            .update(cx, |_, window, cx| window.simulate_next_frame(cx))
            .unwrap(),
        0
    );
}

#[gpui_kit::test]
fn inactive_window_does_not_schedule_animation_frames(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::{AppContext, VisualTestContext};
    cx.update(gpui_kit::init);
    cx.update(|cx| cx.set_reduce_motion(false));
    let window = cx.open_window(
        gpui_kit::size(gpui_kit::px(240.), gpui_kit::px(240.)),
        |window, cx| {
            let view = cx.new(|_| WaveHost);
            gpui_kit::component::Root::new(view, window, cx)
        },
    );
    VisualTestContext::from_window(window.into(), cx).deactivate_window();
    cx.run_until_parked();
    assert_eq!(
        window
            .update(cx, |_, window, cx| {
                assert!(!window.is_window_active());
                window.simulate_next_frame(cx)
            })
            .unwrap(),
        0
    );
}

#[test]
fn missing_and_non_finite_values_remain_unknown() {
    for value in [
        None,
        Some(f32::NAN),
        Some(f32::INFINITY),
        Some(f32::NEG_INFINITY),
    ] {
        assert_eq!(normalize(value), None);
    }
}

#[test]
fn out_of_range_values_are_clamped() {
    assert_eq!(
        [normalize(Some(-4.)), normalize(Some(134.2))],
        [Some(0.), Some(100.)]
    );
}

#[test]
fn waves_stay_inside_the_circle_at_every_phase() {
    for percent in [0., 0.001, 0.342, 0.999, 1.] {
        for phase in [0., 0.25, 0.5, 0.75, 1.] {
            for ix in 0..=128_u16 {
                let x = f32::from(ix) / 128.;
                let (top, bottom) = water_column(x, percent, phase);
                assert!(top <= bottom && (x - 0.5).powi(2) + (top - 0.5).powi(2) <= 0.250_001);
            }
        }
    }
}

#[test]
fn empty_and_full_have_no_residual_wave() {
    for phase in [0., 0.25, 0.75] {
        assert_eq!(water_column(0.5, 0., phase), (1., 1.));
        assert_eq!(water_column(0.5, 1., phase), (0., 1.));
    }
}
