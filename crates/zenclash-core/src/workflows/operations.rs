//! Proxy-selection and provider-update operational workflows.

pub(crate) mod provider;
pub(crate) mod proxy;

pub use provider::{
    CoreResourceAction, ProviderActionReceipt, ProviderActionStatus, ProviderKind,
    ProviderOperationError, ProviderOperationFailure, ProviderOperationResult,
    ProviderOperationalStatus, ProviderOperations, ProviderUpdateBatchReceipt,
    ProviderUpdateTarget, ResourceActionReceipt,
};
pub use proxy::{
    AcceptedProxySelection, ConnectionPolicy, ObservedProxySelection, ProxyDelayTarget,
    ProxyGroupActionReceipt, ProxyGroupMeasurementOutcome, ProxyOperations, ProxySelectionOutcome,
    ProxySelectionReadback, ProxySelectionReceipt, ProxyVisibility, ProxyWorkspaceObservation,
};
