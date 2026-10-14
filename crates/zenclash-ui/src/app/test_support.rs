//! Shared isolated application services for window and status-panel interaction tests.

pub(super) fn app_test_services(
    fixture: &crate::pages::runtime::test_support::RuntimePageFixture,
) -> crate::app::AppServices {
    let services = fixture.services();
    crate::app::AppServices {
        initializing: true,
        await_service_handoff: false,
        profile_store: services.profile_store,
        override_store: services.override_store,
        preferences_store: None,
        preferences: services.preferences.clone(),
        core_kind: services.core_kind,
        core_session: services.core_session,
        client: services.client,
        traffic_monitor: services.traffic_monitor,
        log_monitor: services.log_monitor,
        traffic_history_store: services.traffic_history_store,
        traffic_history_session: None,
        profile_path: services.profile_path,
        controlled_config_store: services.controlled_config_store,
        runtime: services.runtime,
        startup_notice: None,
        startup_error: None,
        restart_after_exit: Default::default(),
    }
}
