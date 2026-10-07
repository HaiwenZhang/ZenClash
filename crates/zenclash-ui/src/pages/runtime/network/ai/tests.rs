use super::*;
use crate::pages::runtime::ui_tests::{Fixture, open};

struct PreviewBackground;

struct NetworkPreview(Entity<RuntimePage>);

impl Render for NetworkPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .items_stretch()
            .bg(cx.theme().background)
            .child(crate::components::sidebar::Sidebar::new(Page::Network))
            .child(div().flex_1().min_w_0().h_full().child(self.0.clone()))
    }
}

impl Render for PreviewBackground {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .p_8()
            .bg(cx.theme().background)
            .child("ZenClash · 网络诊断")
    }
}
use gpui_kit::test::TestWindowExt;

#[gpui_kit::test]
fn ai_network_entry_opens_pending_dialog_and_escape_closes_it(cx: &mut gpui_kit::TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Network);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("ai-network-diagnostics", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        assert!(window.find("ai-network-run").visible());
        assert!(window.find("ai-network-copy").visible());
        window.press("escape", cx);
        assert!(!window.has_active_dialog(cx));
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn ai_network_close_aborts_owned_work_and_leaves_no_running_rows(
    cx: &mut gpui_kit::TestAppContext,
) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    cx.update(gpui_kit::init);
    let mut owner = None;
    let window = cx.open_window(gpui_kit::size(px(1200.), px(1000.)), |window, cx| {
        let view = cx.new(|_| {
            AiNetworkDialog::new(
                runtime.handle().clone(),
                Ok(NetworkProbeRoute::Direct),
                None,
            )
        });
        owner = Some(view);
        let background = cx.new(|_| PreviewBackground);
        gpui_kit::component::Root::new(background, window, cx)
    });
    let view = owner.unwrap();
    let task = runtime.spawn(std::future::pending::<()>());
    cx.update_window(window.into(), |_, window, cx| {
        view.update(cx, |view, _| {
            view.task.replace(&task);
            view.running = true;
            view.rows[0].1 = RowState::Running;
            view.started = Some(Instant::now());
        });
        open_dialog(view.clone(), window, cx);
        window.render_frame(cx);
        window.render_frame(cx);
        window.press("escape", cx);
        let state = view.read(cx);
        assert!(!state.running);
        assert!(state.stopped);
        assert_eq!(state.revision, 1);
        assert!(
            state
                .rows
                .iter()
                .all(|(_, state)| matches!(state, RowState::Pending))
        );
        window.remove_window();
    })
    .unwrap();
    assert!(runtime.block_on(task).unwrap_err().is_cancelled());
}

#[test]
#[ignore = "renders native Windows dialog screenshots without network requests"]
fn ai_network_native_preview() {
    use gpui_kit::component::{Root, ThemeMode};
    use gpui_kit::{Bounds, WindowBounds, WindowOptions, point, size};
    use std::{cell::RefCell, rc::Rc};
    let fixture = Fixture::new();
    let services = fixture.services();
    let runtime = services.runtime.clone();
    let _runtime_guard = runtime.enter();
    let handle = runtime.clone();
    let outcome = Rc::new(RefCell::new(None));
    let result = outcome.clone();
    let output = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/ai-network-preview");
    std::fs::create_dir_all(&output).unwrap();
    gpui_kit::application()
        .with_assets(crate::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
            zenclash_i18n::set_locale("zh-CN");
            let mut owner = None;
            let mut page_owner = None;
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(-10000.), px(-10000.)),
                            size(px(1280.), px(900.)),
                        ))),
                        show: true,
                        focus: false,
                        ..Default::default()
                    },
                    |window, cx| {
                        let view = cx.new(|_| {
                            AiNetworkDialog::new(
                                handle,
                                Ok(NetworkProbeRoute::MihomoHttp {
                                    host: "127.0.0.1".into(),
                                    port: 7890,
                                }),
                                Some(true),
                            )
                        });
                        owner = Some(view);
                        let page =
                            cx.new(|cx| RuntimePage::new(Page::Network, services, window, cx));
                        page_owner = Some(page.clone());
                        let background = cx.new(|_| NetworkPreview(page));
                        cx.new(|cx| Root::new(background, window, cx))
                    },
                )
                .unwrap();
            let owner = owner.unwrap();
            let page = page_owner.unwrap();
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let saved = cx.update(|cx| {
                    cx.update_window(window.into(), |_, window, cx| -> Result<(), String> {
                        for (mode, name) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")]
                        {
                            crate::design::apply_zen_theme(mode, Some(window), cx);
                            page.update(cx, |page, cx| {
                                page.page_read_task.cancel();
                                page.loading = false;
                                page.data = RuntimeData::Network {
                                    config: RuntimeConfig {
                                        mixed_port: 7890,
                                        ..Default::default()
                                    },
                                    system: SystemNetworkSnapshot::default(),
                                };
                                cx.notify();
                            });
                            window.render_frame(cx);
                            window.click("ai-network-diagnostics", cx);
                            window.render_frame(cx);
                            window.render_frame(cx);
                            if !window.find("ai-network-run").visible() {
                                return Err("run action is clipped".into());
                            }
                            if !window.find("ai-retest-exit_ip").visible()
                                || !window.find("ai-retest-claude").visible()
                            {
                                return Err("diagnostic rows are clipped".into());
                            }
                            window
                                .render_to_image()
                                .map_err(|e| e.to_string())?
                                .save(output.join(format!("{name}-pending.png")))
                                .map_err(|e| e.to_string())?;
                            window.press("escape", cx);
                            open_dialog(owner.clone(), window, cx);
                            owner.update(cx, |view, cx| {
                                for (_, state) in &mut view.rows {
                                    *state = RowState::Complete(CheckResult::unavailable());
                                }
                                cx.notify();
                            });
                            window.render_frame(cx);
                            window.render_frame(cx);
                            window
                                .render_to_image()
                                .map_err(|e| e.to_string())?
                                .save(output.join(format!("{name}-unavailable.png")))
                                .map_err(|e| e.to_string())?;
                            window.press("escape", cx);
                            owner.update(cx, |view, cx| {
                                for (_, state) in &mut view.rows {
                                    *state = RowState::Pending;
                                }
                                cx.notify();
                            });
                        }
                        window.remove_window();
                        Ok(())
                    })
                    .map_err(|error| error.to_string())?
                });
                *result.borrow_mut() = Some(saved);
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    outcome
        .borrow_mut()
        .take()
        .expect("native preview finished")
        .unwrap();
}

#[gpui_kit::test]
fn ai_network_narrow_dialog_keeps_actions_visible_and_copies_results(
    cx: &mut gpui_kit::TestAppContext,
) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
    });
    let mut owner = None;
    let window = cx.open_window(gpui_kit::size(px(720.), px(560.)), |window, cx| {
        let view = cx.new(|_| {
            AiNetworkDialog::new(
                runtime.handle().clone(),
                Ok(NetworkProbeRoute::Direct),
                None,
            )
        });
        owner = Some(view);
        let background = cx.new(|_| PreviewBackground);
        gpui_kit::component::Root::new(background, window, cx)
    });
    let owner = owner.unwrap();
    cx.update_window(window.into(), |_, window, cx| {
        owner.update(cx, |view, _| {
            for (_, row) in &mut view.rows {
                *row = RowState::Complete(CheckResult::unavailable());
            }
        });
        open_dialog(owner.clone(), window, cx);
        window.render_frame(cx);
        window.render_frame(cx);
        let button = window.find("ai-network-run");
        assert!(button.visible());
        assert!(button.bounds().bottom() <= window.viewport_size().height);
        assert!(button.bounds().right() <= window.viewport_size().width);
        window.click("ai-network-copy", cx);
        assert!(owner.read(cx).copied);
        assert_eq!(owner.read(cx).report().matches("null").count(), 7);
        window.press("escape", cx);
        window.remove_window();
    })
    .unwrap();
}
