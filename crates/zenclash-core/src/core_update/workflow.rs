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
use crate::{
    CoreConfigValidator, MihomoClient, MihomoProcess, VersionInfo, data_coordinator::DataWriteLease,
};

pub(crate) struct InstalledCore {
    pub(crate) version: VersionInfo,
    pub(crate) cleanup_error: Option<String>,
}

impl MihomoReleaseService {
    // Only CoreSession may enter this activation window, while holding its transition gate.
    pub(crate) async fn install_prepared(
        &self,
        prepared: PreparedCoreUpdate,
        process: Arc<MihomoProcess>,
        client: MihomoClient,
        cancelled: Arc<AtomicBool>,
        lease: &DataWriteLease,
    ) -> CoreUpdateResult<InstalledCore> {
        let expected_tag = prepared.tag().to_owned();
        let candidate = prepared.candidate_path()?.to_path_buf();
        // Resolve the startup payload here: a profile may have changed during download.
        let config = process.launch_config();
        let validator =
            CoreConfigValidator::new(process.kind(), candidate, config.home_dir.clone())
                .with_write_lease(lease);
        let config_file = config.config_file.clone();
        let precheck = tokio::task::spawn_blocking(move || validator.validate_file(config_file))
            .await
            .map_err(|error| {
                CoreUpdateError::Runtime(format!("候选内核配置预检任务异常结束：{error}"))
            })?
            .map_err(|error| {
                CoreUpdateError::Runtime(format!("候选内核拒绝当前运行配置：{error}"))
            });
        if cancelled.load(Ordering::Acquire) {
            tokio::task::spawn_blocking(move || drop(prepared))
                .await
                .map_err(|error| CoreUpdateError::Runtime(error.to_string()))?;
            return Err(CoreUpdateError::Cancelled);
        }
        if let Err(error) = precheck {
            tokio::task::spawn_blocking(move || drop(prepared))
                .await
                .map_err(|error| CoreUpdateError::Runtime(error.to_string()))?;
            return Err(error);
        }
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
                )
                .await);
            }
            Err(error) => {
                return Err(restart_after_activation_failure(
                    process,
                    format!("启用候选内核任务异常结束：{error}"),
                    &cancelled,
                    lease,
                    restore_running,
                )
                .await);
            }
        };
        let verification = async {
            restart_process(process.clone(), &cancelled, lease).await?;
            let reported = tokio::select! {
                biased;
                () = wait_for_cancellation(Some(cancelled.clone())) => return Err(CoreUpdateError::Cancelled),
                result = client.version() => result.map_err(|error| CoreUpdateError::Runtime(error.to_string()))?,
            };
            if !reported.meta || !versions_match(&reported.version, &expected_tag) {
                return Err(CoreUpdateError::Runtime(format!(
                    "新内核 /version 返回 {:?}，期望 {}", reported.version, expected_tag
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
                )
                .await);
            }
        };
        // commit accepts the new core before attempting backup cleanup. Keep that distinction.
        let cleanup_error = tokio::task::spawn_blocking(move || transaction.commit())
            .await
            .map_err(|error| {
                CoreUpdateError::Runtime(format!("提交内核更新任务异常结束：{error}"))
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

async fn stop_process(process: Arc<MihomoProcess>) -> CoreUpdateResult<()> {
    process
        .stop_async()
        .await
        .map_err(|error| CoreUpdateError::Runtime(error.to_string()))
}

async fn restart_process(
    process: Arc<MihomoProcess>,
    cancelled: &Arc<AtomicBool>,
    lease: &DataWriteLease,
) -> CoreUpdateResult<()> {
    process
        .restart_and_wait_until_with_lease(Duration::from_secs(20), Some(cancelled.clone()), lease)
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
    process: Arc<MihomoProcess>,
    activation_error: String,
    cancelled: &Arc<AtomicBool>,
    lease: &DataWriteLease,
    restore_running: bool,
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
    match restart_process(process, cancelled, lease).await {
        Ok(()) => CoreUpdateError::Runtime(format!("{activation_error}；旧内核已重新启动")),
        Err(restart) => {
            CoreUpdateError::Runtime(format!("{activation_error}；旧内核重新启动失败：{restart}"))
        }
    }
}

async fn rollback_rejected_core(
    process: Arc<MihomoProcess>,
    transaction: CoreUpdateTransaction,
    rejection: CoreUpdateError,
    cancelled: &Arc<AtomicBool>,
    lease: &DataWriteLease,
    restore_running: bool,
) -> CoreUpdateError {
    if let Err(error) = stop_process(process.clone()).await {
        let backup = transaction.preserve_for_manual_recovery();
        return CoreUpdateError::Runtime(format!(
            "{rejection}；停止候选内核失败，未强制替换运行中的文件：{error}；旧内核备份保留在 {}",
            backup.display()
        ));
    }
    let rollback = tokio::task::spawn_blocking(move || transaction.rollback_with_status()).await;
    let (result, restored) = match rollback {
        Ok(result) => result,
        Err(error) => {
            return CoreUpdateError::Runtime(format!("{rejection}；回滚任务异常结束：{error}"));
        }
    };
    if !restored {
        return CoreUpdateError::Runtime(format!(
            "{rejection}；回滚旧内核失败：{}",
            result.unwrap_err()
        ));
    }
    if cancelled.load(Ordering::Acquire) {
        return match result {
            Ok(()) => CoreUpdateError::Cancelled,
            Err(error) => {
                CoreUpdateError::Runtime(format!("{rejection}；旧内核已恢复，但清理失败：{error}"))
            }
        };
    }
    let cleanup = result
        .err()
        .map_or(String::new(), |error| format!("；清理失败：{error}"));
    if !restore_running {
        return CoreUpdateError::Runtime(zenclash_i18n::text_with(
            "core_page.errors.release_previous_restored_stopped",
            &[("error", rejection.to_string()), ("cleanup", cleanup)],
        ));
    }
    match restart_process(process, cancelled, lease).await {
        Ok(()) => CoreUpdateError::Runtime(format!("{rejection}；已自动恢复并启动旧内核{cleanup}")),
        Err(error) => CoreUpdateError::Runtime(format!(
            "{rejection}；旧内核已恢复但重新启动失败：{error}{cleanup}"
        )),
    }
}

pub(super) fn versions_match(left: &str, right: &str) -> bool {
    left.trim().trim_start_matches('v') == right.trim().trim_start_matches('v')
}
