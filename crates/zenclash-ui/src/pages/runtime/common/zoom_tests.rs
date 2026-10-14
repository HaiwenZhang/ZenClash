use super::*;
use crate::{components::workspace, pages::Page};
use gpui_kit::component::{ActiveTheme, Root, Theme, ThemeMode};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, Context, Render, TestAppContext, TestSupportExt, Window, px, size};

struct WorkspaceInformation;

impl Render for WorkspaceInformation {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .child(workspace::title(Page::Network, cx))
            .child(
                div()
                    .id("zoom-information-row")
                    .test_support()
                    .child(info_row(
                        "数据文件目录",
                        "资料很长的已解析目录 / a long current data directory",
                        cx.theme(),
                    )),
            )
    }
}

#[gpui_kit::test]
fn workspace_heading_and_information_row_scale_together_when_rem_changes(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let handle = cx.open_window(size(px(900.), px(500.)), |window, cx| {
        let view = cx.new(|_| WorkspaceInformation);
        Root::new(view, window, cx)
    });
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        cx.update(|cx| crate::design::apply_zen_theme(mode, None, cx));
        for font in [12., 16., 20.] {
            cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(font)));
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(window.rem_size(), px(font));
                let heading = window.find("workspace-page-title").bounds();
                let row = window.find("zoom-information-row").bounds();
                assert!((f32::from(heading.size.height) - font * 1.75 * 1.25).abs() <= 1.);
                assert!((f32::from(row.size.height) - font * 3.125).abs() <= 1.);
                assert!(heading.bottom() <= row.top());
                assert!(row.right() <= window.viewport_size().width);
            })
            .unwrap();
        }
    }
    cx.update_window(handle.into(), |_, window, _| window.remove_window())
        .unwrap();
}
