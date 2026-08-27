use gpui::{AppContext, Context, Entity, Window};
use gpui_component::input::InputState;
use zenclash_core::{ProfileApplication, ProfileApplyOutcome, ProfileChange};

use super::super::{
    Button, ButtonVariants, Disableable, IconName, Input, ParentElement, RuntimePage, Styled,
    h_flex, px, setting_card, v_flex,
};

pub(crate) struct ProfileEditorState {
    pub(super) input: Entity<InputState>,
    pub(super) original: Option<String>,
    pub(super) profile_id: Option<String>,
}

enum ProfileEditorSaveOutcome {
    Applied(std::path::PathBuf),
    Stored,
}

impl ProfileEditorState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<'_, RuntimePage>) -> Self {
        Self {
            input: cx.new(|cx| {
                InputState::new(window, cx)
                    .code_editor("yaml")
                    .rows(24)
                    .placeholder(zenclash_i18n::text("overrides.editor.placeholder"))
            }),
            original: None,
            profile_id: None,
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
    pub(super) fn open_profile_yaml_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preview) = &self.config_preview else {
            self.error = Some(zenclash_i18n::text("overrides.errors.preview_required"));
            cx.notify();
            return;
        };
        let Some(profile_id) = self.profile_catalog.active.clone() else {
            self.error = Some(zenclash_i18n::text("overrides.errors.unmanaged_profile"));
            cx.notify();
            return;
        };
        let original = preview.source.clone();
        self.profile_editor.input.update(cx, |input, cx| {
            input.set_value(original.clone(), window, cx);
        });
        self.profile_editor.original = Some(original);
        self.profile_editor.profile_id = Some(profile_id);
        self.error = None;
        cx.notify();
    }

    pub(super) fn cancel_profile_yaml_editor(&mut self, cx: &mut Context<Self>) {
        self.profile_editor.original = None;
        self.profile_editor.profile_id = None;
        cx.notify();
    }

    pub(super) fn save_profile_yaml_editor(&mut self, cx: &mut Context<Self>) {
        let (Some(store), Some(id), Some(original)) = (
            self.profile_store.clone(),
            self.profile_editor.profile_id.clone(),
            self.profile_editor.original.clone(),
        ) else {
            self.error = Some(zenclash_i18n::text("overrides.errors.editor_expired"));
            cx.notify();
            return;
        };
        let candidate = self.profile_editor.input.read(cx).value().to_string();
        let Some(token) = self.begin_mutation(super::super::Page::Override) else {
            return;
        };
        let controlled = self.controlled_config_store.clone();
        let core_session = self.core_session.clone();
        let core_name = self.core_kind.display_name();
        let task = self.runtime.spawn(async move {
            let overrides = super::super::profiles::workflow::load_enabled_overrides().await?;
            let application = ProfileApplication::new(store, controlled, core_session);
            match application
                .apply(ProfileChange::EditYaml {
                    id,
                    expected_payload: original,
                    new_payload: candidate,
                    overrides,
                })
                .await
            {
                ProfileApplyOutcome::Applied { path, .. } => {
                    Ok(ProfileEditorSaveOutcome::Applied(path))
                }
                ProfileApplyOutcome::Stored { .. } => Ok(ProfileEditorSaveOutcome::Stored),
                ProfileApplyOutcome::Rejected { cause, .. } => Err(cause.to_string()),
                ProfileApplyOutcome::RolledBack { cause, .. } => Err(zenclash_i18n::text_with(
                    "overrides.errors.editor_rejected_rolled_back",
                    &[("core", core_name.to_owned()), ("error", cause.to_string())],
                )),
                ProfileApplyOutcome::RuntimeUnknown { cause, .. } => Err(zenclash_i18n::text_with(
                    "overrides.errors.editor_runtime_unknown",
                    &[("core", core_name.to_owned()), ("error", cause.to_string())],
                )),
                ProfileApplyOutcome::PersistedButRuntimeUnknown {
                    cause, rollback, ..
                } => Err(zenclash_i18n::text_with(
                    "overrides.errors.editor_rejected_rollback_failed",
                    &[
                        ("core", core_name.to_owned()),
                        ("error", cause.to_string()),
                        ("rollback", rollback.to_string()),
                    ],
                )),
            }
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
                this.mutating = false;
                match result {
                    Ok(ProfileEditorSaveOutcome::Applied(path))
                        if this.is_page_task_current(token) =>
                    {
                        this.profile_path = Some(path.clone());
                        this.profile_editor.original = None;
                        this.profile_editor.profile_id = None;
                        this.config_preview = None;
                        this.invalidate_config_inputs();
                        if let Err(error) = this.reload_profile_catalog() {
                            this.set_page_error(token, error);
                        } else {
                            this.notice = Some(zenclash_i18n::text_with(
                                "overrides.notices.editor_saved",
                                &[("core", this.core_kind.display_name().to_owned())],
                            ));
                            cx.emit(super::super::ProfileActivated { path });
                        }
                    }
                    Ok(ProfileEditorSaveOutcome::Stored) if this.is_page_task_current(token) => {
                        this.profile_editor.original = None;
                        this.profile_editor.profile_id = None;
                        this.config_preview = None;
                        this.invalidate_config_inputs();
                        if let Err(error) = this.reload_profile_catalog() {
                            this.set_page_error(token, error);
                        } else {
                            this.notice = Some(zenclash_i18n::text(
                                "overrides.notices.editor_saved_inactive",
                            ));
                        }
                    }
                    Ok(_) => {}
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_profile_yaml_editor(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        setting_card(zenclash_i18n::text("overrides.editor.title"), theme)
            .child(
                v_flex()
                    .h(px(520.))
                    .p_3()
                    .child(Input::new(&self.profile_editor.input).h_full()),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .p_3()
                    .child(
                        Button::new("cancel-profile-yaml-edit")
                            .label(zenclash_i18n::text("overrides.editor.cancel"))
                            .ghost()
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cancel_profile_yaml_editor(cx);
                            })),
                    )
                    .child(
                        Button::new("save-profile-yaml-edit")
                            .icon(IconName::Check)
                            .label(zenclash_i18n::text("overrides.editor.save"))
                            .primary()
                            .loading(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.save_profile_yaml_editor(cx);
                            })),
                    ),
            )
    }
}
