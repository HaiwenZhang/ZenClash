//! Automatic runtime monitoring primitives: edge-triggered observation and
//! injected-store configuration source scanning.

mod observation;
mod source_scanner;

pub use observation::{EdgeStableObservation, StableObservation};
pub use source_scanner::{PendingSourceRevision, SourceRevisionKey, SourceScanner};
