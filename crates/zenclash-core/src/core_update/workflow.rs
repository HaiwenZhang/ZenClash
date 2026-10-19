use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use super::{
    CoreUpdateError, CoreUpdateResult, PreparedCoreUpdate, service::MihomoReleaseService,
    transaction::CoreUpdateTransaction,
};
use crate::{ManagedCoreProcess, MihomoClient, VersionInfo, data_coordinator::DataWriteLease};

pub(crate) struct InstalledCore {
    pub(crate) version: VersionInfo,
    pub(crate) cleanup_error: Option<String>,
}

impl MihomoReleaseService {
    // Only CoreSession may enter this activation window, while holding its transition gate.
    pub(crate) async fn install_prepared(
        &self,
        prepared: PreparedCoreUpdate,
        process: Arc<ManagedCoreProcess>,
        client: MihomoClient,
        cancelled: Arc<AtomicBool>,
        lease: &DataWriteLease,
    ) -> CoreUpdateResult<InstalledCore> {
        let expected_tag = prepared.tag().to_owned();
        let candidate = prepared.candidate_path()?.to_path_buf();
        // Resolve the startup payload here: a profile may have changed during download.
        let validator = process
            .config_validator()
            .with_binary(candidate)
            .with_write_lease(lease);
        let precheck_process = process.clone();
        let precheck = tokio::task::spawn_blocking(move || {
            let payload: Arc<str> = precheck_process
                .read_launch_payload()
                .map_err(|error| CoreUpdateError::Runtime(error.to_string()))?
                .into();
            validator.validate_payload(&payload).map_err(|error| {
                CoreUpdateError::Runtime(zenclash_i18n::text_with(
                    "core_update.details.candidate_rejected_config",
                    &[("error", error.to_string())],
                ))
            })?;
            Ok::<_, CoreUpdateError>(payload)
        })
        .await
        .map_err(|error| {
            CoreUpdateError::Runtime(zenclash_i18n::text_with(
                "core_update.details.precheck_task",
                &[("error", error.to_string())],
            ))
        })?;
        if cancelled.load(Ordering::Acquire) {
            tokio::task::spawn_blocking(move || drop(prepared))
                .await
                .map_err(|error| CoreUpdateError::Runtime(error.to_string()))?;
            return Err(CoreUpdateError::Cancelled);
        }
        let payload = match precheck {
            Ok(payload) => payload,
            Err(error) => {
                tokio::task::spawn_blocking(move || drop(prepared))
                    .await
                    .map_err(|error| CoreUpdateError::Runtime(error.to_string()))?;
                return Err(error);
            }
        };
        let restore_running = process.is_running();
        stop_process(process.clone()).await?;
        if cancelled.load(Ordering::Acquire) {
            tokio::task::spawn_blocking(move || drop(prepared))
                .await
                .map_err(|error| CoreUpdateError::Runtime(error.to_string()))?;
            return Err(CoreUpdateError::Cancelled);
        }
        let transaction = match tokio::task::spawn_blocking(move || prepared.activate()).await {
            Ok(Ok(transaction)) => transaction,
            Ok(Err(error)) => {
                return Err(restart_after_activation_failure(
                    process,
                    error.to_string(),
                    &cancelled,
                    lease,
                    restore_running,
                    payload,
                )
                .await);
            }
            Err(error) => {
                return Err(restart_after_activation_failure(
                    process,
                    zenclash_i18n::text_with(
                        "core_update.details.activation_task",
                        &[("error", error.to_string())],
                    ),
                    &cancelled,
                    lease,
                    restore_running,
                    payload,
                )
                .await);
            }
        };
        let verification = async {
            restart_process(process.clone(), &cancelled, lease, payload.clone()).await?;
            let reported = tokio::select! {
                biased;
                () = wait_for_cancellation(Some(cancelled.clone())) => return Err(CoreUpdateError::Cancelled),
                result = client.version() => result.map_err(|error| CoreUpdateError::Runtime(error.to_string()))?,
            };
            if !reported.meta || !versions_match(&reported.version, &expected_tag) {
                return Err(CoreUpdateError::Runtime(zenclash_i18n::text_with(
                    "core_update.details.runtime_version",
                    &[
                        ("actual", format!("{:?}", reported.version)),
                        ("expected", expected_tag.clone()),
                    ],
                )));
            }
            if cancelled.load(Ordering::Acquire) {
                return Err(CoreUpdateError::Cancelled);
            }
            Ok(reported)
        }.await;
        let reported = match verification {
            Ok(reported) => reported,
            Err(error) => {
                return Err(rollback_rejected_core(
                    process,
                    transaction,
                    error,
                    &cancelled,
                    lease,
                    restore_running,
                    payload,
                )
                .await);
            }
        };
        let (transaction, acceptance) = tokio::task::spawn_blocking(move || {
            let mut transaction = transaction;
            let acceptance = transaction.record_commit();
            (transaction, acceptance)
        })
        .await
        .map_err(|error| CoreUpdateError::Runtime(error.to_string()))?;
        if let Err(error) = acceptance {
            return Err(rollback_rejected_core(
                process,
                transaction,
                error,
                &cancelled,
                lease,
                restore_running,
                payload,
            )
            .await);
        }
        // Acceptance is durable before backup cleanup can fail.
        let cleanup_error = tokio::task::spawn_blocking(move || transaction.commit())
            .await
            .map_err(|error| {
                CoreUpdateError::Runtime(zenclash_i18n::text_with(
                    "core_update.details.commit_task",
                    &[("error", error.to_string())],
                ))
            })?
            .err()
            .map(|error| error.to_string());
        Ok(InstalledCore {
            version: reported,
            cleanup_error,
        })
    }
}

pub(crate) fn is_cancelled(cancelled: &Option<Arc<AtomicBool>>) -> bool {
    cancelled
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::Acquire))
}

pub(crate) async fn wait_for_cancellation(cancelled: Option<Arc<AtomicBool>>) {
    let Some(cancelled) = cancelled else {
        std::future::pending::<()>().await;
        return;
    };
    while !cancelled.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn stop_process(process: Arc<ManagedCoreProcess>) -> CoreUpdateResult<()> {
    process
        .stop_async()
        .await
        .map_err(|error| CoreUpdateError::Runtime(error.to_string()))
}

async fn restart_process(
    process: Arc<ManagedCoreProcess>,
    cancelled: &Arc<AtomicBool>,
    lease: &DataWriteLease,
    payload: Arc<str>,
) -> CoreUpdateResult<()> {
    process
        .restart_and_wait_with_payload(
            Duration::from_secs(20),
            Some(cancelled.clone()),
            lease,
            payload,
        )
        .await
        .map_err(|error| {
            if cancelled.load(Ordering::Acquire) {
                CoreUpdateError::Cancelled
            } else {
                CoreUpdateError::Runtime(error.to_string())
            }
        })
}

async fn restart_after_activation_failure(
    process: Arc<ManagedCoreProcess>,
    activation_error: String,
    cancelled: &Arc<AtomicBool>,
    lease: &DataWriteLease,
    restore_running: bool,
    payload: Arc<str>,
) -> CoreUpdateError {
    if cancelled.load(Ordering::Acquire) {
        return CoreUpdateError::Cancelled;
    }
    if !restore_running {
        return CoreUpdateError::Runtime(zenclash_i18n::text_with(
            "core_page.errors.release_previous_stopped",
            &[("error", activation_error)],
        ));
    }
    match restart_process(process, cancelled, lease, payload).await {
        Ok(()) => CoreUpdateError::Runtime(zenclash_i18n::text_with(
            "core_update.details.previous_restarted",
            &[("error", activation_error)],
        )),
        Err(restart) => CoreUpdateError::Runtime(zenclash_i18n::text_with(
            "core_update.details.previous_restart_failed",
            &[
                ("error", activation_error),
                ("restart", restart.to_string()),
            ],
        )),
    }
}

async fn rollback_rejected_core(
    process: Arc<ManagedCoreProcess>,
    transaction: CoreUpdateTransaction,
    rejection: CoreUpdateError,
    cancelled: &Arc<AtomicBool>,
    lease: &DataWriteLease,
    restore_running: bool,
    payload: Arc<str>,
) -> CoreUpdateError {
    if let Err(error) = stop_process(process.clone()).await {
        let backup = transaction
            .preserve_for_manual_recovery()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|error| format!("recovery record could not be preserved: {error}"));
        return CoreUpdateError::Runtime(zenclash_i18n::text_with(
            "core_update.details.candidate_stop_failed",
            &[
                ("error", rejection.to_string()),
                ("stop", error.to_string()),
                ("backup", backup),
            ],
        ));
    }
    let rollback = tokio::task::spawn_blocking(move || transaction.rollback_with_status()).await;
    let (result, restored) = match rollback {
        Ok(result) => result,
        Err(error) => {
            return CoreUpdateError::Runtime(zenclash_i18n::text_with(
                "core_update.details.rollback_task",
                &[
                    ("error", rejection.to_string()),
                    ("rollback", error.to_string()),
                ],
            ));
        }
    };
    if !restored {
        return CoreUpdateError::Runtime(zenclash_i18n::text_with(
            "core_update.details.restore_failed",
            &[
                ("error", rejection.to_string()),
                ("rollback", result.unwrap_err().to_string()),
            ],
        ));
    }
    if cancelled.load(Ordering::Acquire) {
        return match result {
            Ok(()) => CoreUpdateError::Cancelled,
            Err(error) => CoreUpdateError::Runtime(zenclash_i18n::text_with(
                "core_update.details.restored_cleanup_failed",
                &[
                    ("error", rejection.to_string()),
                    ("cleanup", error.to_string()),
                ],
            )),
        };
    }
    let cleanup = result.err();
    if !restore_running {
        return CoreUpdateError::Runtime(with_cleanup_failure(
            zenclash_i18n::text_with(
                "core_page.errors.release_previous_restored_stopped",
                &[("error", rejection.to_string())],
            ),
            cleanup,
        ));
    }
    let message = match restart_process(process, cancelled, lease, payload).await {
        Ok(()) => zenclash_i18n::text_with(
            "core_update.details.restored_restarted",
            &[("error", rejection.to_string())],
        ),
        Err(error) => zenclash_i18n::text_with(
            "core_update.details.restored_restart_failed",
            &[
                ("error", rejection.to_string()),
                ("restart", error.to_string()),
            ],
        ),
    };
    CoreUpdateError::Runtime(with_cleanup_failure(message, cleanup))
}

fn with_cleanup_failure(message: String, cleanup: Option<CoreUpdateError>) -> String {
    match cleanup {
        Some(cleanup) => zenclash_i18n::text_with(
            "core_update.details.cleanup_failed",
            &[("error", message), ("cleanup", cleanup.to_string())],
        ),
        None => message,
    }
}

pub(super) fn versions_match(left: &str, right: &str) -> bool {
    left.trim().trim_start_matches('v') == right.trim().trim_start_matches('v')
}
