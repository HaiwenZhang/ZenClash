use gpui::{AppContext, Context, Entity, Window};
use gpui_component::input::InputState;
use zenclash_core::RemoteProfileRoute;

/// Input and editor state owned by the profiles page.
pub(crate) struct ProfileFormState {
    pub(super) adding_subscription: bool,
    pub(super) subscription_name: Entity<InputState>,
    pub(super) subscription_url: Entity<InputState>,
    pub(super) subscription_user_agent: Entity<InputState>,
    pub(super) subscription_authorization: Entity<InputState>,
    pub(super) subscription_route: RemoteProfileRoute,
    pub(super) request_name: Entity<InputState>,
    pub(super) request_url: Entity<InputState>,
    pub(super) request_user_agent: Entity<InputState>,
    pub(super) request_authorization: Entity<InputState>,
    pub(super) request_timeout_seconds: Entity<InputState>,
    pub(super) update_cron: Entity<InputState>,
    pub(super) editing_profile_id: Option<String>,
    pub(super) editing_route: RemoteProfileRoute,
    pub(super) editing_fixed_update_interval: bool,
}

impl ProfileFormState {
    pub(crate) fn new(
        window: &mut Window,
        cx: &mut Context<'_, super::super::RuntimePage>,
    ) -> Self {
        Self {
            adding_subscription: false,
            subscription_name: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(zenclash_i18n::text("profiles.form.placeholder_name"))
            }),
            subscription_url: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("https://example.com/api/v1/client/subscribe…")
            }),
            subscription_user_agent: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value("clash.meta")
                    .placeholder("clash.meta")
            }),
            subscription_authorization: cx.new(|cx| {
                InputState::new(window, cx).placeholder(zenclash_i18n::text(
                    "profiles.form.placeholder_authorization",
                ))
            }),
            subscription_route: RemoteProfileRoute::DirectWithMihomoFallback,
            request_name: cx.new(|cx| {
                InputState::new(window, cx).placeholder(zenclash_i18n::text(
                    "profiles.form.placeholder_request_name",
                ))
            }),
            request_url: cx.new(|cx| {
                InputState::new(window, cx).placeholder("https://example.com/profile.yaml")
            }),
            request_user_agent: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value("clash.meta")
                    .placeholder("clash.meta")
            }),
            request_authorization: cx.new(|cx| {
                InputState::new(window, cx).placeholder(zenclash_i18n::text(
                    "profiles.form.placeholder_request_authorization",
                ))
            }),
            request_timeout_seconds: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value("30")
                    .placeholder("30")
            }),
            update_cron: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(zenclash_i18n::text("profiles.form.placeholder_cron"))
            }),
            editing_profile_id: None,
            editing_route: RemoteProfileRoute::DirectWithMihomoFallback,
            editing_fixed_update_interval: false,
        }
    }

    pub(in crate::pages::runtime) fn refresh_localized_placeholders(
        &self,
        window: &mut Window,
        cx: &mut Context<'_, super::super::RuntimePage>,
    ) {
        for (input, key) in [
            (&self.subscription_name, "profiles.form.placeholder_name"),
            (
                &self.subscription_authorization,
                "profiles.form.placeholder_authorization",
            ),
            (&self.request_name, "profiles.form.placeholder_request_name"),
            (
                &self.request_authorization,
                "profiles.form.placeholder_request_authorization",
            ),
            (&self.update_cron, "profiles.form.placeholder_cron"),
        ] {
            input.update(cx, |input, cx| {
                input.set_placeholder(zenclash_i18n::text(key), window, cx);
            });
        }
    }
}
