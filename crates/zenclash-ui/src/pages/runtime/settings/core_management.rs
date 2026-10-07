use std::path::{Path, PathBuf};

use super::super::{
    Button, ButtonVariants, Context, CoreBinaryInfo, CoreKind, Disableable, FluentBuilder,
    IconName, IntoElement, MihomoLaunchConfig, ParentElement, PathPromptOptions, RuntimeData,
    RuntimePage, Selectable, Sizable, Styled, div, h_flex, px, setting_card, v_flex,
};

#[derive(Default)]
pub(in crate::pages::runtime) struct CoreManagementUiState {
    mihomo: CoreProbeState,
    meow: CoreProbeState,
}

#[derive(Default)]
struct CoreProbeState {
    checking: bool,
    source: String,
    info: Option<CoreBinaryInfo>,
    error: Option<String>,
}

impl CoreManagementUiState {
    fn get(&self, kind: CoreKind) -> &CoreProbeState {
        match kind {
            CoreKind::Mihomo => &self.mihomo,
            CoreKind::Meow => &self.meow,
        }
    }

    fn get_mut(&mut self, kind: CoreKind) -> &mut CoreProbeState {
        match kind {
            CoreKind::Mihomo => &mut self.mihomo,
            CoreKind::Meow => &mut self.meow,
        }
    }
}

struct CoreProbeResult {
    kind: CoreKind,
    source: String,
    result: Result<CoreBinaryInfo, String>,
}

impl RuntimePage {
    pub(in crate::pages::runtime) fn render_core_management(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let kind = self.preferences.core_kind;
        let custom = self.preferences.core_binaries.path(kind).is_some();
        let state = self.core_management.get(kind);
        let locked = binary_environment_override(kind).is_some();
        let path = self
            .preferences
            .core_binaries
            .path(kind)
            .map(|p| p.display().to_string())
            .or_else(|| {
                state
                    .info
                    .as_ref()
                    .map(|info| info.path.display().to_string())
            })
            .unwrap_or_else(|| zenclash_i18n::text("core_management.status.not_checked"));
        setting_card(zenclash_i18n::text("settings_redesign.core_source"), theme)
            .child(
                h_flex()
                    .gap_3()
                    .p_4()
                    .child(
                        Button::new(("auto-core-binary", core_index(kind)))
                            .flex_1()
                            .h_20()
                            .outline()
                            .selected(!custom)
                            .label(zenclash_i18n::text("core_management.actions.automatic"))
                            .when(!custom, |button| {
                                button.custom(
                                    gpui_kit::component::button::ButtonCustomVariant::new(cx)
                                        .color(theme.sidebar_accent)
                                        .foreground(theme.primary)
                                        .active(theme.sidebar_accent),
                                )
                            })
                            .disabled(self.core_busy() || state.checking || locked)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if custom {
                                    this.use_automatic_core_binary(kind, cx);
                                }
                            })),
                    )
                    .child(
                        Button::new(("select-core-binary", core_index(kind)))
                            .flex_1()
                            .h_20()
                            .outline()
                            .selected(custom)
                            .icon(IconName::FolderOpen)
                            .label(zenclash_i18n::text("settings_redesign.custom_core"))
                            .disabled(self.core_busy() || state.checking || locked)
                            .on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.choose_core_binary(kind, cx)
                                }),
                            ),
                    ),
            )
            .child(super::super::info_row(
                zenclash_i18n::text("core_page.process.binary"),
                path,
                theme,
            ))
            .child(super::super::info_row(
                zenclash_i18n::text("core_management.status.running"),
                self.core_kind.display_name(),
                theme,
            ))
            .child(super::super::info_row(
                zenclash_i18n::text("core_management.status.next"),
                kind.display_name(),
                theme,
            ))
            .when(locked, |view| {
                view.child(super::super::context_note(
                    zenclash_i18n::text("core_management.summary.environment_override"),
                    theme,
                ))
            })
            .child(
                h_flex()
                    .p_4()
                    .gap_2()
                    .child(
                        Button::new("refresh-core-management")
                            .outline()
                            .small()
                            .label(zenclash_i18n::text("core_management.summary.refresh"))
                            .disabled(self.core_busy() || state.checking)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.refresh_core_management(cx)),
                            ),
                    )
                    .child(
                        Button::new("manage-core-kinds")
                            .outline()
                            .small()
                            .label(zenclash_i18n::text("settings_redesign.manage_cores"))
                            .on_click(cx.listener(|_, _, window, cx| {
                                use gpui_kit::component::{ActiveTheme, WindowExt};
                                if window.has_active_dialog(cx) {
                                    return;
                                }
                                let owner = cx.entity().downgrade();
                                window.open_dialog(cx, move |dialog, window, cx| {
                                    let content = owner
                                        .update(cx, |page, cx| {
                                            let theme = cx.theme().clone();
                                            v_flex()
                                                .child(page.render_core_binary_row(
                                                    CoreKind::Mihomo,
                                                    &theme,
                                                    cx,
                                                ))
                                                .child(page.render_core_binary_row(
                                                    CoreKind::Meow,
                                                    &theme,
                                                    cx,
                                                ))
                                                .child(page.render_core_extra_network(cx))
                                        })
                                        .ok();
                                    dialog
                                        .title(zenclash_i18n::text("core_management.title"))
                                        .bg(cx.theme().group_box)
                                        .width(window.rem_size() * 46.)
                                        .margin_top(
                                            ((window.viewport_size().height
                                                - window.rem_size() * 26.)
                                                / 2.)
                                                .max(window.rem_size()),
                                        )
                                        .when_some(content, |dialog, content| dialog.child(content))
                                });
                            })),
                    ),
            )
    }

    fn render_core_binary_row(
        &self,
        kind: CoreKind,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let state = self.core_management.get(kind);
        let requested = self.preferences.core_kind == kind;
        let running = self.core_kind == kind && self.runtime_core_available();
        let index = core_index(kind);
        let (status, status_color, path, version) = if state.checking {
            (
                zenclash_i18n::text("core_management.status.checking"),
                theme.primary,
                zenclash_i18n::text("core_management.status.checking_version"),
                String::new(),
            )
        } else if let Some(info) = state.info.as_ref() {
            (
                zenclash_i18n::text("core_management.status.available"),
                theme.success,
                info.path.display().to_string(),
                format!("{} · {}", info.version, info.architecture),
            )
        } else {
            (
                zenclash_i18n::text("core_management.status.unavailable"),
                theme.danger,
                state
                    .error
                    .clone()
                    .unwrap_or_else(|| zenclash_i18n::text("core_management.status.not_checked")),
                String::new(),
            )
        };
        let environment_locked = binary_environment_override(kind).is_some();

        h_flex()
            .min_h(px(104.))
            .px_4()
            .py_3()
            .gap_4()
            .items_start()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .min_w(px(0.))
                    .flex_1()
                    .gap_3()
                    .items_start()
                    .child(
                        div()
                            .mt_1()
                            .size(px(10.))
                            .rounded_full()
                            .bg(status_color)
                            .border_1()
                            .border_color(status_color.opacity(0.45)),
                    )
                    .child(
                        v_flex()
                            .min_w(px(0.))
                            .flex_1()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                            .child(kind.display_name()),
                                    )
                                    .child(status_badge(status, status_color))
                                    .when(running, |row| {
                                        row.child(status_badge(
                                            zenclash_i18n::text("core_management.status.running"),
                                            theme.primary,
                                        ))
                                    })
                                    .when(requested, |row| {
                                        row.child(status_badge(
                                            zenclash_i18n::text("core_management.status.next"),
                                            theme.warning,
                                        ))
                                    })
                                    .when(kind.is_experimental(), |row| {
                                        row.child(status_badge(
                                            zenclash_i18n::text(
                                                "core_management.status.experimental",
                                            ),
                                            theme.warning,
                                        ))
                                    }),
                            )
                            .child(div().text_xs().text_color(theme.muted_foreground).child(
                                zenclash_i18n::text_with(
                                    "core_management.status.source",
                                    &[("source", empty_source(&state.source))],
                                ),
                            ))
                            .child(
                                div()
                                    .max_w(px(610.))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_xs()
                                    .text_color(if state.info.is_some() {
                                        theme.foreground
                                    } else {
                                        theme.danger
                                    })
                                    .child(path),
                            )
                            .when(!version.is_empty(), |column| {
                                column.child(
                                    div()
                                        .font_family(theme.mono_font_family.clone())
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(version),
                                )
                            }),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .items_end()
                    .child(
                        Button::new(("select-core-binary", index))
                            .label(zenclash_i18n::text("core_management.actions.select_file"))
                            .icon(IconName::FolderOpen)
                            .small()
                            .outline()
                            .disabled(self.core_busy() || state.checking || environment_locked)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.choose_core_binary(kind, cx);
                            })),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new(("auto-core-binary", index))
                                    .label(zenclash_i18n::text("core_management.actions.automatic"))
                                    .small()
                                    .ghost()
                                    .disabled(
                                        self.core_busy()
                                            || state.checking
                                            || environment_locked
                                            || self.preferences.core_binaries.path(kind).is_none(),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.use_automatic_core_binary(kind, cx);
                                    })),
                            )
                            .child(
                                Button::new(("activate-core", index))
                                    .label(if requested {
                                        zenclash_i18n::text("core_management.actions.selected")
                                    } else {
                                        zenclash_i18n::text("core_management.actions.use_next")
                                    })
                                    .small()
                                    .outline()
                                    .selected(requested)
                                    .disabled(
                                        self.core_busy()
                                            || state.checking
                                            || state.info.is_none()
                                            || requested,
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_preferred_core(kind, cx);
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn runtime_core_available(&self) -> bool {
        let operational = self.operational_status.snapshot();
        if operational.process.is_fresh()
            && let Some(process) = operational.process.value()
            && process.generation == self.core_session.generation()
        {
            return process.running;
        }
        self.core_session.runtime_descriptor().backend()
            == zenclash_core::CoreRuntimeBackend::Direct
            && self.data_runtime_version == self.core_session.generation()
            && matches!(
                self.data,
                RuntimeData::Core { .. }
                    | RuntimeData::Settings {
                        config: Some(_),
                        ..
                    }
            )
    }

    pub(in crate::pages::runtime) fn refresh_core_management(&mut self, cx: &mut Context<Self>) {
        for kind in [CoreKind::Mihomo, CoreKind::Meow] {
            let state = self.core_management.get_mut(kind);
            state.checking = true;
            state.error = None;
        }
        let binaries = self.preferences.core_binaries.clone();
        let task = self.runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                [CoreKind::Mihomo, CoreKind::Meow]
                    .map(|kind| probe_core_binary(kind, binaries.path(kind)))
            })
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "core_management.errors.detection_task",
                    &[("error", error.to_string())],
                )
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.map_err(|error| {
                zenclash_i18n::text_with(
                    "core_management.errors.detection_task",
                    &[("error", error.to_string())],
                )
            });
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(results)) => {
                        for result in results {
                            let state = this.core_management.get_mut(result.kind);
                            state.checking = false;
                            state.source = result.source;
                            match result.result {
                                Ok(info) => {
                                    state.info = Some(info);
                                    state.error = None;
                                }
                                Err(error) => {
                                    state.info = None;
                                    state.error = Some(error);
                                }
                            }
                        }
                    }
                    Ok(Err(error)) | Err(error) => {
                        for kind in [CoreKind::Mihomo, CoreKind::Meow] {
                            let state = this.core_management.get_mut(kind);
                            state.checking = false;
                            state.info = None;
                            state.error = Some(error.clone());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn choose_core_binary(&mut self, kind: CoreKind, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(
                zenclash_i18n::text_with(
                    "core_management.dialog.choose",
                    &[("core", kind.display_name().to_owned())],
                )
                .into(),
            ),
        });
        cx.spawn(async move |this, cx| {
            let selection = receiver.await;
            let _ = this.update(cx, |this, cx| match selection {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.validate_and_store_core_binary(kind, path, cx);
                    }
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    this.error = Some(zenclash_i18n::text_with(
                        "core_management.errors.chooser",
                        &[("error", error.to_string())],
                    ));
                    cx.notify();
                }
                Err(error) => {
                    this.error = Some(zenclash_i18n::text_with(
                        "core_management.errors.chooser_task",
                        &[("error", error.to_string())],
                    ));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn validate_and_store_core_binary(
        &mut self,
        kind: CoreKind,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.preferences_store.clone() else {
            self.error = Some(zenclash_i18n::text(
                "core_management.errors.preferences_unavailable",
            ));
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(self.page) else {
            return;
        };
        self.core_management.get_mut(kind).checking = true;
        let task = self.runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                let info = zenclash_core::validate_core_binary(kind, path)
                    .map_err(|error| error.to_string())?;
                let canonical = info.path.clone();
                let preferences = store
                    .update(move |preferences| {
                        preferences.core_binaries.set(kind, Some(canonical));
                    })
                    .map_err(|error| error.to_string())?;
                Ok::<_, String>((info, preferences))
            })
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "core_management.errors.validation_task",
                    &[("error", error.to_string())],
                )
            })?
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "core_management.errors.validation_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok((info, preferences)) => {
                        let state = this.core_management.get_mut(kind);
                        state.checking = false;
                        state.source = zenclash_i18n::text("core_management.source.custom");
                        state.info = Some(info);
                        state.error = None;
                        if this.is_page_task_current(token) {
                            this.notice = Some(zenclash_i18n::text_with(
                                "core_management.notices.validated",
                                &[("core", kind.display_name().to_owned())],
                            ));
                        }
                        this.accept_preferences(
                            preferences,
                            crate::pages::runtime::PreferenceScope::CoreBinary(kind),
                            cx,
                        );
                    }
                    Err(error) => {
                        let state = this.core_management.get_mut(kind);
                        state.checking = false;
                        state.error = Some(error.clone());
                        this.set_page_error(token, error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn use_automatic_core_binary(&mut self, kind: CoreKind, cx: &mut Context<Self>) {
        let Some(store) = self.preferences_store.clone() else {
            self.error = Some(zenclash_i18n::text(
                "core_management.errors.preferences_unavailable",
            ));
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(self.page) else {
            return;
        };
        let task = self.runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                store
                    .update(|preferences| preferences.core_binaries.set(kind, None))
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "core_management.errors.discovery_task",
                    &[("error", error.to_string())],
                )
            })?
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "core_management.errors.discovery_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(preferences) => {
                        if this.is_page_task_current(token) {
                            this.notice = Some(zenclash_i18n::text_with(
                                "core_management.notices.automatic",
                                &[("core", kind.display_name().to_owned())],
                            ));
                        }
                        this.accept_preferences(
                            preferences,
                            crate::pages::runtime::PreferenceScope::CoreBinary(kind),
                            cx,
                        );
                        this.refresh_core_management(cx);
                    }
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn set_preferred_core(&mut self, kind: CoreKind, cx: &mut Context<Self>) {
        let Some(info) = self.core_management.get(kind).info.clone() else {
            self.error = Some(zenclash_i18n::text_with(
                "core_management.errors.not_validated",
                &[("core", kind.display_name().to_owned())],
            ));
            cx.notify();
            return;
        };
        let Some(store) = self.preferences_store.clone() else {
            self.error = Some(zenclash_i18n::text(
                "core_management.errors.preferences_unavailable",
            ));
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(self.page) else {
            return;
        };
        let task = self.runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                zenclash_core::validate_core_binary(kind, &info.path)
                    .map_err(|error| error.to_string())?;
                store
                    .update(|preferences| preferences.core_kind = kind)
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "core_management.errors.switch_task",
                    &[("error", error.to_string())],
                )
            })?
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "core_management.errors.switch_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(preferences) => {
                        if this.is_page_task_current(token) {
                            this.notice = Some(if kind == this.core_kind {
                                zenclash_i18n::text_with(
                                    "core_management.notices.already_current",
                                    &[("core", kind.display_name().to_owned())],
                                )
                            } else {
                                zenclash_i18n::text_with(
                                    "core_management.notices.preferred",
                                    &[("core", kind.display_name().to_owned())],
                                )
                            });
                        }
                        this.accept_preferences(
                            preferences,
                            crate::pages::runtime::PreferenceScope::CoreKind,
                            cx,
                        );
                    }
                    Err(error) => {
                        let state = this.core_management.get_mut(kind);
                        state.info = None;
                        state.error = Some(error.clone());
                        this.set_page_error(token, error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

fn probe_core_binary(kind: CoreKind, preferred: Option<&Path>) -> CoreProbeResult {
    let source = binary_source(kind, preferred);
    let result = project_root()
        .and_then(|root| {
            MihomoLaunchConfig::discover_for_kind_with_binary(root, kind, preferred)
                .map_err(|error| error.to_string())
        })
        .and_then(|launch| {
            zenclash_core::validate_core_binary(kind, launch.binary)
                .map_err(|error| error.to_string())
        });
    CoreProbeResult {
        kind,
        source,
        result,
    }
}

fn project_root() -> Result<PathBuf, String> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| zenclash_i18n::text("core_management.errors.workspace"))
}

fn binary_environment_override(kind: CoreKind) -> Option<&'static str> {
    kind.binary_environment_variable()
}

fn binary_source(kind: CoreKind, preferred: Option<&Path>) -> String {
    binary_environment_override(kind).map_or_else(
        || {
            if preferred.is_some() {
                zenclash_i18n::text("core_management.source.custom")
            } else {
                zenclash_i18n::text("core_management.source.automatic")
            }
        },
        |variable| {
            zenclash_i18n::text_with(
                "core_management.source.environment",
                &[("variable", variable.to_owned())],
            )
        },
    )
}

fn core_index(kind: CoreKind) -> usize {
    match kind {
        CoreKind::Mihomo => 0,
        CoreKind::Meow => 1,
    }
}

fn empty_source(source: &str) -> String {
    if source.is_empty() {
        zenclash_i18n::text("core_management.status.waiting")
    } else {
        source.to_owned()
    }
}

fn status_badge(label: String, color: gpui_kit::Hsla) -> gpui_kit::AnyElement {
    div()
        .px_2()
        .py(px(2.))
        .rounded_full()
        .border_1()
        .border_color(color.opacity(0.4))
        .bg(color.opacity(0.09))
        .text_size(px(10.))
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(color)
        .child(label)
        .into_any_element()
}
