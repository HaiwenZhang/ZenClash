use crate::core::structure::ServiceLifecycleState;
use std::sync::atomic::{AtomicU8, Ordering};

static SERVICE_STATE: AtomicU8 = AtomicU8::new(ServiceLifecycleState::Starting as u8);
static CORE_STATE: AtomicU8 = AtomicU8::new(ServiceLifecycleState::Running as u8);

pub fn set_service_lifecycle_state(state: ServiceLifecycleState) {
    SERVICE_STATE.store(state as u8, Ordering::Relaxed);
}

pub fn service_lifecycle_state() -> ServiceLifecycleState {
    ServiceLifecycleState::from_u8(SERVICE_STATE.load(Ordering::Relaxed))
}

pub(super) fn set_core_lifecycle_state(state: ServiceLifecycleState) {
    CORE_STATE.store(state as u8, Ordering::Relaxed);
}

pub(super) fn core_lifecycle_state() -> ServiceLifecycleState {
    ServiceLifecycleState::from_u8(CORE_STATE.load(Ordering::Relaxed))
}
