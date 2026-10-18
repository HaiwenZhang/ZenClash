//! Accepted runtime configuration, cache finalization and guarded recovery protocols.

use std::sync::{Arc, atomic::AtomicBool};

use super::{ControlledConfigError, ControlledConfigResult, storage::RuntimeCacheTransaction};
use crate::{ManagedCoreProcess, MihomoError, data_coordinator::DataWriteLease};

/// The existing mutation and filesystem authority, transferred together into completion.
pub(super) struct RuntimeMutationGuards {
    _mutation: tokio::sync::OwnedMutexGuard<()>,
    write_lease: DataWriteLease,
}

impl RuntimeMutationGuards {
    pub(super) fn new(
        mutation: tokio::sync::OwnedMutexGuard<()>,
        write_lease: DataWriteLease,
    ) -> Self {
        Self {
            _mutation: mutation,
            write_lease,
        }
    }

    pub(super) fn write_lease(&self) -> &DataWriteLease {
        &self.write_lease
    }
}

#[must_use]
pub(crate) struct RuntimeCandidateValidation {
    _guards: RuntimeMutationGuards,
}

impl RuntimeCandidateValidation {
    pub(super) fn new(guards: RuntimeMutationGuards) -> Self {
        Self { _guards: guards }
    }
}

/// A controller-accepted payload paired with its staged cache and previous bytes.
pub(super) struct AcceptedRuntime {
    cache: RuntimeCacheTransaction,
    runtime: crate::client::AppliedConfig,
    previous_payload: Option<String>,
}

impl AcceptedRuntime {
    pub(super) fn new(
        cache: RuntimeCacheTransaction,
        runtime: crate::client::AppliedConfig,
        previous_payload: Option<String>,
    ) -> Self {
        Self {
            cache,
            runtime,
            previous_payload,
        }
    }

    pub(super) async fn commit(self) -> ControlledConfigResult<()> {
        // Persistence is durable: finalization cannot let cache Drop undo it.
        self.cache.finalize(self.runtime.commit()).await
    }

    pub(super) async fn rollback(self) -> ControlledConfigResult<()> {
        let cache = rollback_runtime_cache(self.cache).await;
        let runtime = self
            .runtime
            .rollback(self.previous_payload)
            .await
            .map_err(ControlledConfigError::Profile);
        match (cache, runtime) {
            (Ok(()), Ok(())) => Ok(()),
            (cache, runtime) => Err(ControlledConfigError::Transaction(format!(
                "缓存恢复：{}；运行内核恢复：{}",
                result_label(cache),
                result_label(runtime),
            ))),
        }
    }
}

#[must_use]
pub(crate) struct RuntimeApplicationTransaction {
    recovery: RuntimeApplicationRecovery,
    guards: RuntimeMutationGuards,
}

enum RuntimeApplicationRecovery {
    HotReload(Box<AcceptedRuntime>),
    Restart {
        cache: RuntimeCacheTransaction,
        process: Arc<ManagedCoreProcess>,
        cancelled: Option<Arc<AtomicBool>>,
    },
}

impl RuntimeApplicationTransaction {
    pub(super) fn hot_reload(runtime: AcceptedRuntime, guards: RuntimeMutationGuards) -> Self {
        Self {
            recovery: RuntimeApplicationRecovery::HotReload(Box::new(runtime)),
            guards,
        }
    }

    pub(super) fn restart(
        cache: RuntimeCacheTransaction,
        process: Arc<ManagedCoreProcess>,
        cancelled: Option<Arc<AtomicBool>>,
        guards: RuntimeMutationGuards,
    ) -> Self {
        Self {
            recovery: RuntimeApplicationRecovery::Restart {
                cache,
                process,
                cancelled,
            },
            guards,
        }
    }

    pub(crate) async fn commit(self) -> ControlledConfigResult<()> {
        match self.recovery {
            RuntimeApplicationRecovery::HotReload(runtime) => runtime.commit().await,
            RuntimeApplicationRecovery::Restart { cache, .. } => {
                cache.commit();
                Ok(())
            }
        }
    }

    pub(crate) async fn rollback(self) -> ControlledConfigResult<()> {
        match self.recovery {
            RuntimeApplicationRecovery::HotReload(runtime) => runtime.rollback().await,
            RuntimeApplicationRecovery::Restart {
                cache,
                process,
                cancelled,
            } => {
                let cache = rollback_runtime_cache(cache).await;
                let runtime = if cache.is_ok() {
                    process
                        .restart_and_wait_until_with_lease(
                            std::time::Duration::from_secs(20),
                            cancelled,
                            &self.guards.write_lease,
                        )
                        .await
                        .map_err(ControlledConfigError::Profile)
                } else {
                    Err(ControlledConfigError::Transaction(
                        "启动缓存未恢复，拒绝使用候选缓存再次启动内核".into(),
                    ))
                };
                match (cache, runtime) {
                    (Ok(()), Ok(())) => Ok(()),
                    (cache, runtime) => Err(ControlledConfigError::Transaction(format!(
                        "缓存恢复：{}；运行内核恢复：{}",
                        result_label(cache),
                        result_label(runtime),
                    ))),
                }
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct RuntimeMutationError {
    pub(crate) cause: ControlledConfigError,
    pub(crate) attempted: bool,
}

impl RuntimeMutationError {
    pub(super) fn attempted(cause: ControlledConfigError) -> Self {
        Self {
            cause,
            attempted: true,
        }
    }
}

impl From<ControlledConfigError> for RuntimeMutationError {
    fn from(cause: ControlledConfigError) -> Self {
        Self {
            cause,
            attempted: false,
        }
    }
}

impl From<MihomoError> for RuntimeMutationError {
    fn from(cause: MihomoError) -> Self {
        ControlledConfigError::Profile(cause).into()
    }
}

impl From<std::io::Error> for RuntimeMutationError {
    fn from(cause: std::io::Error) -> Self {
        ControlledConfigError::Io(cause).into()
    }
}
pub(super) async fn rollback_runtime_cache(
    cache: RuntimeCacheTransaction,
) -> ControlledConfigResult<()> {
    tokio::task::spawn_blocking(move || cache.rollback())
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
}

pub(super) async fn rollback_cache_and_restart(
    cache: RuntimeCacheTransaction,
    process: Arc<ManagedCoreProcess>,
    write_lease: &DataWriteLease,
    cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
) -> String {
    let cache_rollback = tokio::task::spawn_blocking(move || cache.rollback())
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))
        .and_then(|result| result);
    if let Err(error) = cache_rollback {
        return format!("失败（缓存恢复失败：{error}）");
    }
    result_label(
        process
            .restart_and_wait_until_with_lease(
                std::time::Duration::from_secs(20),
                cancelled,
                write_lease,
            )
            .await,
    )
}

pub(super) fn result_label<T, E: std::fmt::Display>(result: Result<T, E>) -> String {
    result.map_or_else(|error| format!("失败（{error}）"), |_| "成功".into())
}
