//! Recovery-capture hook contract consumed by [`crate::CoreSession`] without
//! depending on the concrete traffic-capture session module.

use std::{future::Future, pin::Pin};

use crate::session_errors::TrafficCaptureError;

pub(crate) type CoreRecoveryHookFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), TrafficCaptureError>> + Send + 'a>>;

pub(crate) trait CoreRecoveryCapture: Send + Sync {
    fn release_owned(&self) -> CoreRecoveryHookFuture<'_>;
    fn reconcile(&self) -> CoreRecoveryHookFuture<'_>;
}
