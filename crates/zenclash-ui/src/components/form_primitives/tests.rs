use super::{settings_input_row, settings_switch_disabled, visible_description};

#[test]
fn visible_description_filters_blank_text_and_preserves_meaningful_copy() {
    assert_eq!(visible_description("".into()), None);
    assert_eq!(visible_description("   \t\n ".into()), None);
    assert_eq!(
        visible_description("Autostart points to an older executable".into()),
        Some("Autostart points to an older executable".into())
    );
}

#[gpui_kit::test]
fn settings_rows_render_non_empty_descriptions_beneath_labels(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::{ActiveTheme, Root, v_flex};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AppContext, Context, InteractiveElement, IntoElement, ParentElement, Render, Styled,
        TestSupportExt, Window, div, px, size,
    };

    struct RowsView;
    impl Render for RowsView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = cx.theme().clone();
            let long_desc = "Autostart currently points to an older executable path and should be re-enabled to launch this build.";
            v_flex()
                .w(px(280.))
                .child(
                    div()
                        .id("switch-no-desc")
                        .test_support()
                        .child(settings_switch_disabled(
                            "Autostart",
                            "",
                            false,
                            "test-switch-empty",
                            &theme,
                            true,
                            |_, _, _| {},
                        )),
                )
                .child(
                    div()
                        .id("switch-with-desc")
                        .test_support()
                        .child(settings_switch_disabled(
                            "Autostart",
                            long_desc,
                            false,
                            "test-switch-desc",
                            &theme,
                            true,
                            |_, _, _| {},
                        )),
                )
                .child(
                    div()
                        .id("input-no-desc")
                        .test_support()
                        .child(settings_input_row("Mixed port", "", div(), &theme)),
                )
                .child(
                    div()
                        .id("input-with-desc")
                        .test_support()
                        .child(settings_input_row("Mixed port", long_desc, div(), &theme)),
                )
        }
    }

    cx.update(gpui_kit::init);
    let handle = cx.open_window(size(px(320.), px(600.)), |window, cx| {
        let view = cx.new(|_| RowsView);
        Root::new(view, window, cx)
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.find("switch-with-desc").bounds().size.height
                > window.find("switch-no-desc").bounds().size.height,
            "disabled switch row must render non-empty description beneath label"
        );
        assert!(
            window.find("input-with-desc").bounds().size.height
                > window.find("input-no-desc").bounds().size.height,
            "settings input row must render non-empty description beneath label"
        );
        window.remove_window();
    })
    .unwrap();
}
