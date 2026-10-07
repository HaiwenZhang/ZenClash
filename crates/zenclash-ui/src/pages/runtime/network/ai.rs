//! Dialog-owned diagnostics; closing the overlay cancels its task and drops its results.
use super::super::*;
use super::model;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::WindowExt;
use std::time::Instant;
use zenclash_core::{
    NetworkProbeRoute,
    ai_network::{Check, CheckResult, Diagnostics, Status},
};

#[cfg(test)]
mod tests;

fn text(key: &str) -> String {
    zenclash_i18n::text(&format!("ai_network.{key}"))
}

impl RuntimePage {
    pub(in crate::pages::runtime) fn open_ai_network(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let route = match &self.data {
            RuntimeData::Network { config, .. } => model::network_probe_route(
                config,
                self.preferences.network_probe_route == NetworkProbeRoutePreference::Mihomo,
            ),
            _ => Err(text("config_unavailable")),
        };
        let tun = match &self.data {
            RuntimeData::Network { config, .. } => Some(config.tun.enable),
            _ => None,
        };
        let runtime = self.runtime.clone();
        let view = cx.new(|_| AiNetworkDialog::new(runtime, route, tun));
        open_dialog(view, window, cx);
    }
}

fn open_dialog(view: Entity<AiNetworkDialog>, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, window, cx| {
        let close = view.downgrade();
        let confirm = view.downgrade();
        dialog
            .bg(cx.theme().group_box)
            .title(
                h_flex()
                    .gap_3()
                    .child(Icon::new(IconName::Globe).size_6())
                    .child(text("title")),
            )
            .width(
                (window.rem_size() * 64.)
                    .min(window.viewport_size().width - window.rem_size() * 2.),
            )
            .margin_top(window.rem_size())
            .overlay_closable(false)
            .on_ok(move |_, _, cx| {
                let _ = confirm.update(cx, |view, cx| view.start(None, cx));
                false
            })
            .on_close(move |_, _, cx| {
                let _ = close.update(cx, |view, cx| view.stop(cx));
            })
            .child(view.clone())
    });
}

enum RowState {
    Pending,
    Running,
    Complete(CheckResult),
}

struct AiNetworkDialog {
    runtime: tokio::runtime::Handle,
    route: Result<NetworkProbeRoute, String>,
    tun: Option<bool>,
    rows: Vec<(Check, RowState)>,
    task: super::super::loader::PageReadTask,
    revision: u64,
    running: bool,
    stopped: bool,
    elapsed: std::time::Duration,
    started: Option<Instant>,
    copied: bool,
}

impl AiNetworkDialog {
    fn new(
        runtime: tokio::runtime::Handle,
        route: Result<NetworkProbeRoute, String>,
        tun: Option<bool>,
    ) -> Self {
        Self {
            runtime,
            route,
            tun,
            rows: Check::ALL
                .into_iter()
                .map(|kind| (kind, RowState::Pending))
                .collect(),
            task: Default::default(),
            revision: 0,
            running: false,
            stopped: false,
            elapsed: Default::default(),
            started: None,
            copied: false,
        }
    }

    fn start(&mut self, only: Option<Check>, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let Ok(route) = self.route.clone() else {
            return;
        };
        let checks = only.map_or_else(|| Check::ALL.to_vec(), |kind| vec![kind]);
        for (kind, state) in &mut self.rows {
            if checks.contains(kind)
                || (only == Some(Check::ExitIp) && matches!(kind, Check::Risk | Check::Timezone))
            {
                *state = RowState::Pending;
            }
        }
        self.running = true;
        self.stopped = false;
        self.copied = false;
        self.started = Some(Instant::now());
        self.elapsed = std::time::Duration::ZERO;
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        let tun = self.tun;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let task = self.runtime.spawn(async move {
            let mut diagnostics = Diagnostics::new(&route, tun);
            for kind in checks {
                if tx.send((kind, None)).is_err() {
                    break;
                }
                let result = diagnostics.check(kind).await;
                if tx.send((kind, Some(result))).is_err() {
                    break;
                }
            }
        });
        self.task.replace(&task);
        cx.spawn(async move |this, cx| {
            while let Some((kind, result)) = rx.recv().await {
                if this
                    .update(cx, |view, cx| {
                        if !view.running || view.revision != revision {
                            return;
                        }
                        if let Some((_, row)) = view.rows.iter_mut().find(|(key, _)| *key == kind) {
                            *row = result.map_or(RowState::Running, RowState::Complete);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
            let failed = task.await.is_err();
            let _ = this.update(cx, |view, cx| {
                if view.revision != revision {
                    return;
                }
                if failed {
                    for (_, row) in &mut view.rows {
                        if matches!(row, RowState::Running) {
                            *row = RowState::Complete(CheckResult::unavailable());
                        }
                    }
                }
                view.finish();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn finish(&mut self) {
        self.running = false;
        if let Some(started) = self.started.take() {
            self.elapsed = started.elapsed();
        }
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if self.running {
            self.task.cancel();
            self.revision = self.revision.wrapping_add(1);
            self.stopped = true;
            self.finish();
            for (_, state) in &mut self.rows {
                if matches!(state, RowState::Running) {
                    *state = RowState::Pending;
                }
            }
            cx.notify();
        }
    }

    fn report(&self) -> String {
        let mut lines = vec![text("title"), self.route_label(), text("disclaimer")];
        for (kind, state) in &self.rows {
            let label = text(kind.key());
            if let RowState::Complete(result) = state {
                lines.push(format!(
                    "{label}: [{}] {}\n{}\n{}",
                    text(status_key(result.status())),
                    result.summary(),
                    result.detail(),
                    result.evidence()
                ));
            } else {
                lines.push(format!("{label}: {}", text("pending")));
            }
        }
        lines.join("\n\n")
    }

    fn route_label(&self) -> String {
        match &self.route {
            Ok(NetworkProbeRoute::Direct) => text("route_host"),
            Ok(NetworkProbeRoute::MihomoHttp { .. }) => text("route_mihomo"),
            Err(error) => error.clone(),
        }
    }

    fn render_row(&self, kind: Check, state: &RowState, cx: &mut Context<Self>) -> gpui_kit::Div {
        let theme = cx.theme();
        let (summary, detail, status) = match state {
            RowState::Pending => (text("pending"), String::new(), "pending"),
            RowState::Running => (text("testing"), String::new(), "testing"),
            RowState::Complete(result) => (
                result.summary().to_owned(),
                result.detail().to_owned(),
                status_key(result.status()),
            ),
        };
        let color = match state {
            RowState::Complete(result) => match result.status() {
                Status::Passed => theme.success,
                Status::Attention => theme.warning,
                Status::Failed => theme.danger,
                _ => theme.muted_foreground,
            },
            _ => theme.muted_foreground,
        };
        let icon = match state {
            RowState::Complete(result)
                if matches!(result.status(), Status::Attention | Status::Failed) =>
            {
                IconName::TriangleAlert
            }
            RowState::Complete(result) if result.status() == Status::Passed => {
                IconName::CircleCheck
            }
            _ => IconName::Info,
        };
        h_flex()
            .px_3()
            .py_3()
            .gap_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .w_24()
                    .flex_shrink_0()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(text(kind.key())),
            )
            .child(v_flex().flex_1().min_w_0().gap_1().child(summary).when(
                !detail.is_empty(),
                |view| {
                    view.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(detail),
                    )
                },
            ))
            .child(
                h_flex()
                    .w_24()
                    .flex_shrink_0()
                    .gap_2()
                    .text_color(color)
                    .child(Icon::new(icon).size_4())
                    .child(text(status)),
            )
            .child(
                Button::new(format!("ai-retest-{}", kind.key()))
                    .w_16()
                    .small()
                    .outline()
                    .label(text("retest"))
                    .disabled(self.running || self.route.is_err())
                    .on_click(cx.listener(move |view, _, _, cx| view.start(Some(kind), cx))),
            )
    }
}

fn status_key(status: Status) -> &'static str {
    match status {
        Status::Info => "observed",
        Status::Passed => "passed",
        Status::Attention => "attention",
        Status::Failed => "high_risk",
        Status::Unknown => "unknown",
    }
}

impl Render for AiNetworkDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let completed = self
            .rows
            .iter()
            .filter(|(_, state)| matches!(state, RowState::Complete(_)))
            .count();
        let attention = self.rows.iter().filter(|(_, state)| matches!(state, RowState::Complete(result) if matches!(result.status(), Status::Attention | Status::Failed))).count();
        let unknown = self.rows.iter().filter(|(_, state)| matches!(state, RowState::Complete(result) if result.status() == Status::Unknown)).count();
        let summary = if self.running {
            "testing"
        } else if self.stopped {
            "stopped"
        } else if completed == Check::ALL.len() {
            "completed"
        } else if completed > 0 {
            "partial"
        } else {
            "ready"
        };
        let counts = zenclash_i18n::text_with(
            "ai_network.counts",
            &[
                ("done", completed.to_string()),
                ("attention", attention.to_string()),
                ("unknown", unknown.to_string()),
                (
                    "seconds",
                    format!(
                        "{:.1}",
                        self.started
                            .map_or(self.elapsed, |start| start.elapsed())
                            .as_secs_f64()
                    ),
                ),
            ],
        );
        v_flex()
            .id("ai-network-dialog")
            .test_support()
            .gap_4()
            .text_sm()
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(text("subtitle")),
            )
            .child(
                v_flex()
                    .p_3()
                    .gap_1()
                    .border_1()
                    .border_color(theme.border)
                    .rounded(theme.radius)
                    .bg(theme.secondary)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Icon::new(if completed == Check::ALL.len() {
                                    IconName::CircleCheck
                                } else {
                                    IconName::Info
                                })
                                .size_5(),
                            )
                            .child(
                                div()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(text(summary)),
                            ),
                    )
                    .child(div().text_color(theme.muted_foreground).child(
                        if completed == 0 && !self.running {
                            text("ready_hint")
                        } else {
                            counts
                        },
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(self.route_label()),
                    ),
            )
            .when(attention > 0, |view| {
                view.child(
                    v_flex()
                        .gap_1()
                        .child(div().text_color(theme.warning).child(text("findings")))
                        .child(
                            div()
                                .text_color(theme.muted_foreground)
                                .child(text("findings_hint")),
                        ),
                )
            })
            .child(
                v_flex()
                    .id("ai-network-results")
                    .min_h_0()
                    .h((window.viewport_size().height
                        - window.rem_size() * if attention > 0 { 29. } else { 24. })
                    .max(window.rem_size() * 6.)
                    .min(window.rem_size() * if completed == 0 { 24. } else { 31. }))
                    .flex_shrink_0()
                    .overflow_y_scrollbar()
                    .child(
                        v_flex()
                            .flex_shrink_0()
                            .border_1()
                            .border_color(theme.border)
                            .rounded(theme.radius)
                            .child(
                                h_flex()
                                    .px_3()
                                    .py_2()
                                    .gap_3()
                                    .bg(theme.secondary)
                                    .rounded_t(theme.radius)
                                    .text_color(theme.muted_foreground)
                                    .child(div().w_24().flex_shrink_0().child(text("column_check")))
                                    .child(div().flex_1().min_w_0().child(text("column_result")))
                                    .child(
                                        div().w_24().flex_shrink_0().child(text("column_status")),
                                    )
                                    .child(div().w_16().flex_shrink_0()),
                            )
                            .children(
                                self.rows
                                    .iter()
                                    .map(|(kind, state)| self.render_row(*kind, state, cx)),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .justify_between()
                    .pt_3()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(text("disclaimer")),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("ai-network-copy")
                                    .outline()
                                    .icon(IconName::Copy)
                                    .label(text(if self.copied { "copied" } else { "copy" }))
                                    .disabled(completed == 0 || self.running)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            view.report(),
                                        ));
                                        view.copied = true;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("ai-network-run")
                                    .primary()
                                    .icon(crate::assets::AppIcon::RefreshCw)
                                    .label(text(if self.running {
                                        "stop"
                                    } else if completed > 0 {
                                        "run_again"
                                    } else {
                                        "run"
                                    }))
                                    .disabled(self.route.is_err())
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        if view.running {
                                            view.stop(cx);
                                        } else {
                                            view.start(None, cx);
                                        }
                                    })),
                            ),
                    ),
            )
    }
}
