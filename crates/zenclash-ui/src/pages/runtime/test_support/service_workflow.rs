//! Service-capable page setup; scenarios retain their own actions and assertions.
use super::RuntimePageFixture as Fixture;
use crate::pages::runtime::*;
use gpui_kit::component::Root;
use gpui_kit::{TestAppContext, size};
pub(in crate::pages::runtime) fn open_service_tun(
    cx: &mut TestAppContext,
    fixture: &Fixture,
) -> (AnyWindowHandle, Entity<RuntimePage>) {
    open_service_page(cx, fixture, Page::Tun)
}

pub(in crate::pages::runtime) fn open_service_page(
    cx: &mut TestAppContext,
    fixture: &Fixture,
    initial: Page,
) -> (AnyWindowHandle, Entity<RuntimePage>) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_kit::init(cx);
        // Match Kit's dialog fixtures: pointer clicks target a resting surface.
        cx.set_reduce_motion(true);
    });
    let mut page = None;
    let height = if initial == Page::Home { 2600. } else { 1000. };
    let handle = cx.open_window(size(px(1200.), px(height)), |window, cx| {
        let mut services = fixture.services();
        let manager = zenclash_core::ServiceManager::new(
            fixture.core.clone(),
            services.traffic_capture.clone(),
        );
        services.profile_service = services.profile_service.with_service_manager(manager);
        let view = cx.new(|cx| RuntimePage::new(initial, services, window, cx));
        page = Some(view.clone());
        Root::new(view, window, cx)
    });
    (handle.into(), page.unwrap())
}
