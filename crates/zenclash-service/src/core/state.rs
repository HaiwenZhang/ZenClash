use crate::core::structure::ServiceLifecycleState;
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicU8, Ordering};

pub fn set_service_lifecycle_state(state: ServiceLifecycleState) {
    service_lifecycle_state_cell().store(state as u8, Ordering::Relaxed);
}

pub fn service_lifecycle_state() -> ServiceLifecycleState {
    ServiceLifecycleState::from_u8(service_lifecycle_state_cell().load(Ordering::Relaxed))
}

pub(super) fn set_core_lifecycle_state(state: ServiceLifecycleState) {
    core_lifecycle_state_cell().store(state as u8, Ordering::Relaxed);
}

pub(super) fn core_lifecycle_state() -> ServiceLifecycleState {
    ServiceLifecycleState::from_u8(core_lifecycle_state_cell().load(Ordering::Relaxed))
}

fn service_lifecycle_state_cell() -> &'static AtomicU8 {
    static SERVICE_STATE: Lazy<AtomicU8> = Lazy::new(|| AtomicU8::new(ServiceLifecycleState::Starting as u8));
    &SERVICE_STATE
}

fn core_lifecycle_state_cell() -> &'static AtomicU8 {
    static CORE_STATE: Lazy<AtomicU8> = Lazy::new(|| AtomicU8::new(ServiceLifecycleState::Running as u8));
    &CORE_STATE
}
