use std::{sync::Arc, time::Duration};

use super::{
    service::MihomoReleaseService, transaction::CoreUpdateTransaction, CoreUpdateError,
    CoreUpdateResult, MihomoRelease,
};
use crate::{MihomoClient, MihomoProcess, VersionInfo};

impl MihomoReleaseService {
    /// Installs one release into a managed Mihomo process and commits only
    /// after the restarted controller reports the requested version.
    ///
    /// Downloading, hashing, extraction, and the candidate `-v` check happen
    /// while the current process remains online. The short activation window
    /// stops the process, atomically swaps the executable, and starts it again.
    /// A failed start or version mismatch restores and restarts the old core.
    ///
    /// # Errors
    ///
    /// Returns an error for release/integrity failures, process stop/start
    /// failures, a controller version mismatch, or failed rollback.
    pub async fn install_managed(
        &self,
        release: &MihomoRelease,
        process: Arc<MihomoProcess>,
        client: MihomoClient,
    ) -> CoreUpdateResult<VersionInfo> {
        let target = process.snapshot().binary;
        let prepared = self.prepare(release, target).await?;
        stop_process(process.clone()).await?;
        let transaction = match tokio::task::spawn_blocking(move || prepared.activate()).await {
            Ok(Ok(transaction)) => transaction,
            Ok(Err(error)) => {
                return Err(restart_after_activation_failure(process, error.to_string()).await)
            }
            Err(error) => {
                return Err(restart_after_activation_failure(
                    process,
                    format!("启用候选内核任务异常结束：{error}"),
                )
                .await)
            }
        };
        let verification = async {
            restart_process(process.clone()).await?;
            let reported = client
                .version()
                .await
                .map_err(|error| CoreUpdateError::Runtime(error.to_string()))?;
            if !reported.meta || !versions_match(&reported.version, &release.tag) {
                return Err(CoreUpdateError::Runtime(format!(
                    "新内核 /version 返回 {:?}，期望 {}",
                    reported.version, release.tag
                )));
            }
            Ok(reported)
        }
        .await;
        let reported = match verification {
            Ok(reported) => reported,
            Err(error) => {
                return Err(rollback_rejected_core(process, transaction, error.to_string()).await)
            }
        };
        tokio::task::spawn_blocking(move || transaction.commit())
            .await
            .map_err(|error| {
                CoreUpdateError::Runtime(format!("提交内核更新任务异常结束：{error}"))
            })??;
        Ok(reported)
    }
}

async fn stop_process(process: Arc<MihomoProcess>) -> CoreUpdateResult<()> {
    tokio::task::spawn_blocking(move || process.stop())
        .await
        .map_err(|error| CoreUpdateError::Runtime(format!("停止内核任务异常结束：{error}")))?
        .map_err(|error| CoreUpdateError::Runtime(error.to_string()))
}

async fn restart_process(process: Arc<MihomoProcess>) -> CoreUpdateResult<()> {
    let restarting = process.clone();
    tokio::task::spawn_blocking(move || restarting.restart())
        .await
        .map_err(|error| CoreUpdateError::Runtime(format!("启动内核任务异常结束：{error}")))?
        .map_err(|error| CoreUpdateError::Runtime(error.to_string()))?;
    process
        .wait_until_ready(Duration::from_secs(20))
        .await
        .map_err(|error| CoreUpdateError::Runtime(error.to_string()))
}

async fn restart_after_activation_failure(
    process: Arc<MihomoProcess>,
    activation_error: String,
) -> CoreUpdateError {
    match restart_process(process).await {
        Ok(()) => CoreUpdateError::Runtime(format!("{activation_error}；旧内核已重新启动")),
        Err(restart) => {
            CoreUpdateError::Runtime(format!("{activation_error}；旧内核重新启动失败：{restart}"))
        }
    }
}

async fn rollback_rejected_core(
    process: Arc<MihomoProcess>,
    transaction: CoreUpdateTransaction,
    rejection: String,
) -> CoreUpdateError {
    if let Err(error) = stop_process(process.clone()).await {
        let backup = transaction.preserve_for_manual_recovery();
        return CoreUpdateError::Runtime(format!(
            "{rejection}；停止候选内核失败，未强制替换运行中的文件：{error}；旧内核备份保留在 {}",
            backup.display()
        ));
    }
    let rollback = tokio::task::spawn_blocking(move || transaction.rollback())
        .await
        .map_err(|error| CoreUpdateError::Runtime(format!("回滚任务异常结束：{error}")))
        .and_then(|result| result);
    if let Err(error) = rollback {
        return CoreUpdateError::Runtime(format!("{rejection}；回滚旧内核失败：{error}"));
    }
    match restart_process(process).await {
        Ok(()) => CoreUpdateError::Runtime(format!("{rejection}；已自动恢复并启动旧内核")),
        Err(error) => {
            CoreUpdateError::Runtime(format!("{rejection}；旧内核已恢复但重新启动失败：{error}"))
        }
    }
}

pub(super) fn versions_match(left: &str, right: &str) -> bool {
    left.trim().trim_start_matches('v') == right.trim().trim_start_matches('v')
}
