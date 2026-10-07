use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::{ActiveTheme, WindowExt};
use gpui_kit::{AppContext, Context, Entity, Focusable, Window, prelude::FluentBuilder};

use super::super::{
    Button, ButtonVariants, Disableable, IconName, ParentElement, RuntimePage, Styled, h_flex,
    v_flex,
};

pub(crate) struct ProfileEditorState {
    pub(in crate::pages::runtime) input: Entity<EditorState>,
    pub(in crate::pages::runtime) original: Option<String>,
    pub(in crate::pages::runtime) profile_id: Option<String>,
    dialog_open: bool,
}

impl ProfileEditorState {
    pub(crate) fn new(window: &mut Window, cx: &mut gpui_kit::App) -> Self {
        Self {
            input: cx.new(|cx| {
                EditorState::new(window, cx)
                    .language("yaml")
                    .placeholder(zenclash_i18n::text("overrides.editor.placeholder"))
            }),
            original: None,
            profile_id: None,
            dialog_open: false,
        }
    }

    pub(in crate::pages::runtime) fn refresh_localized_placeholder(
        &self,
        window: &mut Window,
        cx: &mut Context<'_, RuntimePage>,
    ) {
        self.input.update(cx, |input, cx| {
            input.set_placeholder(
                zenclash_i18n::text("overrides.editor.placeholder"),
                window,
                cx,
            );
        });
    }
}

impl RuntimePage {
    fn release_profile_yaml_editor_text(&self, saved: String, cx: &mut Context<Self>) {
        let input = self.overrides.editor.input.clone();
        let page = cx.entity().downgrade();
        let window_handle = self.window_handle;
        cx.defer(move |cx| {
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = page.update(cx, |page, cx| {
                    if page.overrides.editor.original.is_none() {
                        if page.overrides.editor.dialog_open {
                            page.overrides.editor.dialog_open = false;
                            window.close_dialog(cx);
                        }
                        input.update(cx, |input, cx| {
                            if input.value() == saved {
                                input.set_value(String::new(), window, cx);
                            }
                        });
                    }
                });
            });
        });
    }

    pub(super) fn open_profile_yaml_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.core_busy() || window.has_active_dialog(cx) {
            return;
        }
        let Some(preview) = &self.overrides.preview else {
            self.error = Some(zenclash_i18n::text("overrides.errors.preview_required"));
            cx.notify();
            return;
        };
        let Some(profile_id) = self.profiles.catalog.active.clone() else {
            self.error = Some(zenclash_i18n::text("overrides.errors.unmanaged_profile"));
            cx.notify();
            return;
        };
        let original = preview.source.clone();
        self.overrides.editor.input.update(cx, |input, cx| {
            input.set_value(original.clone(), window, cx);
        });
        self.overrides.editor.original = Some(original);
        self.overrides.editor.profile_id = Some(profile_id);
        self.error = None;
        self.overrides.editor.dialog_open = true;
        let owner = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let content = owner
                .update(cx, |page, cx| {
                    let theme = cx.theme().clone();
                    page.render_profile_yaml_editor(&theme, window, cx)
                })
                .ok();
            let busy = owner
                .update(cx, |page, _| page.core_busy())
                .unwrap_or(false);
            let cancel_owner = owner.clone();
            dialog
                .title(zenclash_i18n::text("overrides.editor.title"))
                .width(
                    (window.rem_size() * 64.)
                        .min(window.viewport_size().width - window.rem_size() * 2.),
                )
                .margin_top(window.rem_size())
                .close_button(!busy)
                .overlay_closable(false)
                .when_some(content, |dialog, content| dialog.child(content))
                .on_cancel(move |_, window, cx| {
                    cancel_owner
                        .update(cx, |page, cx| {
                            if page.core_busy() {
                                return false;
                            }
                            page.cancel_profile_yaml_editor(window, cx);
                            true
                        })
                        .unwrap_or(true)
                })
        });
        self.overrides
            .editor
            .input
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    pub(super) fn cancel_profile_yaml_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.overrides.editor.input.update(cx, |input, cx| {
            input.set_value(String::new(), window, cx);
        });
        self.overrides.editor.original = None;
        self.overrides.editor.profile_id = None;
        self.overrides.editor.dialog_open = false;
        self.error = None;
        cx.notify();
    }

    pub(super) fn save_profile_yaml_editor(&mut self, cx: &mut Context<Self>) {
        let (Some(store), Some(id), Some(original)) = (
            self.profiles.store.clone(),
            self.overrides.editor.profile_id.clone(),
            self.overrides.editor.original.clone(),
        ) else {
            self.error = Some(zenclash_i18n::text("overrides.errors.editor_expired"));
            cx.notify();
            return;
        };
        let candidate = self.overrides.editor.input.read(cx).value().to_string();
        let Some(token) = self.begin_mutation(super::super::Page::Override) else {
            return;
        };
        let controlled = self.controlled_config_store.clone();
        let service = self.profile_service.clone();
        let submitted_id = id.clone();
        let submitted_original = original.clone();
        let submitted_payload = candidate.clone();
        let task = self.runtime.spawn(async move {
            service
                .edit_yaml(store, controlled, id, original, candidate)
                .await
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "overrides.errors.editor_workflow",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(outcome) => {
                        this.synchronize_profile_recovery();
                        let warning = outcome.warning();
                        let applied_path = outcome
                            .runtime_version()
                            .map(|version| (outcome.path().to_path_buf(), version));
                        this.complete_profile_yaml_save(
                            token,
                            submitted_id,
                            submitted_original,
                            submitted_payload,
                            applied_path,
                            cx,
                        );
                        if this.is_page_task_current(token)
                            && outcome
                                .runtime_version()
                                .is_some_and(|version| this.profile_service.is_current(version))
                            && warning.is_some()
                        {
                            this.notice = warning;
                        }
                    }
                    Err(error) => {
                        this.synchronize_profile_recovery();
                        this.finish_mutation(token);
                        this.set_page_error(token, error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(in crate::pages::runtime) fn complete_profile_yaml_save(
        &mut self,
        token: super::super::PageTaskToken,
        id: String,
        original: String,
        saved: String,
        applied_path: Option<(std::path::PathBuf, u64)>,
        cx: &mut Context<Self>,
    ) {
        self.finish_mutation(token);
        if self.overrides.editor.profile_id.as_deref() == Some(id.as_str())
            && self.overrides.editor.original.as_deref() == Some(original.as_str())
        {
            if self.overrides.editor.input.read(cx).value() == saved {
                self.overrides.editor.original = None;
                self.overrides.editor.profile_id = None;
                self.release_profile_yaml_editor_text(saved, cx);
            } else {
                // This acknowledgement becomes the baseline for the newer draft.
                self.overrides.editor.original = Some(saved);
            }
        }
        self.overrides.invalidate_preview();
        let was_applied = applied_path.is_some();
        let is_current = applied_path
            .as_ref()
            .is_some_and(|(_, version)| self.profile_service.is_current(*version));
        if was_applied {
            self.synchronize_committed_profile(cx);
        }
        self.invalidate_config_inputs(cx);
        self.reload_profile_catalog(cx);
        if self.is_page_task_current(token) {
            self.notice = Some(if is_current {
                zenclash_i18n::text_with(
                    "overrides.notices.editor_saved",
                    &[("core", self.core_kind.display_name().to_owned())],
                )
            } else {
                zenclash_i18n::text("overrides.notices.editor_saved_inactive")
            });
        }
    }

    pub(super) fn render_profile_yaml_editor(
        &self,
        theme: &gpui_kit::component::Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        v_flex()
            .gap_3()
            .child(
                v_flex()
                    .h((window.viewport_size().height - window.rem_size() * 14.)
                        .max(window.rem_size() * 8.)
                        .min(window.rem_size() * 36.))
                    .child(Editor::new(&self.overrides.editor.input).h_full()),
            )
            .when_some(self.error.as_ref(), |view, error| {
                view.child(
                    super::super::div()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(error.clone()),
                )
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .p_3()
                    .child(
                        Button::new("cancel-profile-yaml-edit")
                            .label(zenclash_i18n::text("overrides.editor.cancel"))
                            .ghost()
                            .disabled(self.core_busy())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.cancel_profile_yaml_editor(window, cx);
                                window.close_dialog(cx);
                            })),
                    )
                    .child(
                        Button::new("save-profile-yaml-edit")
                            .icon(IconName::Check)
                            .label(zenclash_i18n::text("overrides.editor.save"))
                            .primary()
                            .loading(self.core_busy())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.save_profile_yaml_editor(cx);
                            })),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{IntoElement, Render, Styled, TestAppContext, div, px, size};

    use super::*;

    struct Document {
        editor: ProfileEditorState,
    }

    impl Render for Document {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(Editor::new(&self.editor.input).h_full())
        }
    }

    #[gpui_kit::test]
    fn yaml_editor_keeps_multiline_keyboard_editing_and_undo(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut editor = None;
        let handle = cx.open_window(size(px(800.), px(600.)), |window, cx| {
            let view = cx.new(|cx| {
                let state = ProfileEditorState::new(window, cx);
                editor = Some(state.input.clone());
                Document { editor: state }
            });
            Root::new(view, window, cx)
        });
        let editor = editor.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let id = ("input", editor.entity_id());
            window.click(id, cx);
            window.input("mixed-port: 7890", cx);
            window.press("enter", cx);
            window.input("mode: rule", cx);
            assert_eq!(editor.read(cx).language_name(), "yaml");
            assert!(editor.read(cx).is_code_editor());
            assert_eq!(editor.read(cx).value(), "mixed-port: 7890\nmode: rule");
            window.press("secondary-a", cx);
            window.press("backspace", cx);
            assert_eq!(editor.read(cx).value(), "");
            window.press("secondary-z", cx);
            assert_eq!(editor.read(cx).value(), "mixed-port: 7890\nmode: rule");
        })
        .unwrap();
    }
}
