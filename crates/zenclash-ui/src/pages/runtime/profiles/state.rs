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
            subscription_name: cx
                .new(|cx| InputState::new(window, cx).placeholder("例如：机场主订阅")),
            subscription_url: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("https://example.com/api/v1/client/subscribe…")
            }),
            subscription_user_agent: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value("clash.meta")
                    .placeholder("clash.meta")
            }),
            subscription_authorization: cx
                .new(|cx| InputState::new(window, cx).placeholder("Bearer … 或 Basic …（可留空）")),
            subscription_route: RemoteProfileRoute::DirectWithMihomoFallback,
            request_name: cx.new(|cx| InputState::new(window, cx).placeholder("在线订阅名称")),
            request_url: cx.new(|cx| {
                InputState::new(window, cx).placeholder("https://example.com/profile.yaml")
            }),
            request_user_agent: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value("clash.meta")
                    .placeholder("clash.meta")
            }),
            request_authorization: cx.new(|cx| {
                InputState::new(window, cx).placeholder("Bearer … 或 Basic …（留空即删除）")
            }),
            request_timeout_seconds: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value("30")
                    .placeholder("30")
            }),
            update_cron: cx.new(|cx| {
                InputState::new(window, cx).placeholder("例如：0 */6 * * *（分 时 日 月 周）")
            }),
            editing_profile_id: None,
            editing_route: RemoteProfileRoute::DirectWithMihomoFallback,
            editing_fixed_update_interval: false,
        }
    }
}
