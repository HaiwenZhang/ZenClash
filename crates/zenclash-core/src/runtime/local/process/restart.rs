//! Restart orchestration and controller readiness probing for [`ManagedCoreProcess`].

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use super::{
    ManagedCoreProcess, input,
    spawn::{local_launch_payload, spawn_child, stop_child},
    unix_timestamp_secs,
};
use crate::{CoreKind, MihomoEndpoint, MihomoError, MihomoResult, VersionInfo};

impl ManagedCoreProcess {
    /// Checks a single startup snapshot before stopping and restarting the child.
    ///
    /// Local Mihomo receives the exact checked bytes, even if the source changes.
    /// Existing bounded logs remain available and new stdout/stderr collectors
    /// append to the same history.
    ///
    /// # Errors
    ///
    /// Returns an error when the old process cannot be stopped or the new child
    /// and its output collectors cannot be started. After a spawn failure the
    /// process remains stopped so callers can report and retry safely.
    pub fn restart(&self) -> MihomoResult<()> {
        let lease = crate::platform::data_coordinator::DataWriteLease::shared(self.write_scopes())?;
        self.restart_with_write_lease(&lease, None)
    }

    pub(super) fn restart_with_write_lease(
        &self,
        lease: &crate::platform::data_coordinator::DataWriteLease,
        cancelled: Option<&AtomicBool>,
    ) -> MihomoResult<()> {
        let payload = local_launch_payload(&self.config)?;
        self.restart_payload_with_write_lease(lease, cancelled, payload.as_deref())
    }

    pub(crate) fn read_launch_payload(&self) -> MihomoResult<String> {
        local_launch_payload(&self.config)?.ok_or_else(|| {
            MihomoError::Process(zenclash_i18n::text(
                "core_page.errors.runtime_kind_mismatch",
            ))
        })
    }

    pub(super) fn restart_payload_with_write_lease(
        &self,
        lease: &crate::platform::data_coordinator::DataWriteLease,
        cancelled: Option<&AtomicBool>,
        payload: Option<&str>,
    ) -> MihomoResult<()> {
        if let Some(payload) = payload {
            if self.kind() != CoreKind::Mihomo {
                return Err(MihomoError::Process(zenclash_i18n::text(
                    "core_page.errors.runtime_kind_mismatch",
                )));
            }
            crate::mihomo::tun::admission::ensure_local_yaml(self.kind(), payload)?;
        }
        let validator = self.config_validator().with_write_lease(lease);
        let validation = match payload {
            Some(payload) => validator.validate_payload(payload),
            None => validator.validate_file(&self.config.config_file),
        };
        validation.map_err(|error| {
            MihomoError::Process(format!("重启前配置预检失败，当前内核保持运行：{error}"))
        })?;
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(MihomoError::Process("内核重启在配置预检后已取消".into()));
        }
        let input = payload
            .map(|payload| input::snapshot(&self.config.home_dir, payload))
            .transpose()?;
        let mut child_slot = self.child.lock();
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(MihomoError::Process("内核重启在配置预检后已取消".into()));
        }
        stop_child(&mut child_slot)?;
        self.started_at_secs.store(0, Ordering::Relaxed);
        let mut execution = self.execution.lock();
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            execution.take();
            return Err(MihomoError::Process("内核重启在创建子进程前已取消".into()));
        }
        if execution.is_none() {
            *execution = self.reserve_execution()?;
        }
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            execution.take();
            return Err(MihomoError::Process("内核重启在创建子进程前已取消".into()));
        }
        let child = match spawn_child(
            &self.config,
            self.logs.clone(),
            input,
            self.recovery_asset_root.read().as_deref(),
        ) {
            Ok(child) => child,
            Err(error) => {
                execution.take();
                return Err(error);
            }
        };
        *self.last_exit_reason.write() = None;
        self.started_at_secs
            .store(unix_timestamp_secs(), Ordering::Relaxed);
        *child_slot = Some(child);
        Ok(())
    }

    /// Restarts the managed child away from an async caller and waits for its controller.
    ///
    /// This is the single lifecycle transition for runtime callers. Blocking
    /// process operations run on Tokio's blocking pool before readiness is
    /// checked asynchronously.
    ///
    /// # Errors
    ///
    /// Returns validation, stop, spawn, task, early-exit, or readiness errors.
    pub async fn restart_and_wait(self: &Arc<Self>, timeout: Duration) -> MihomoResult<()> {
        self.restart_and_wait_until(timeout, None).await
    }

    pub(crate) async fn restart_and_wait_until(
        self: &Arc<Self>,
        timeout: Duration,
        cancelled: Option<Arc<AtomicBool>>,
    ) -> MihomoResult<()> {
        let lease =
            crate::platform::data_coordinator::DataWriteLease::shared_async(self.write_scopes())
                .await
                .map_err(|error| MihomoError::Process(error.to_string()))?;
        self.restart_and_wait_until_with_lease(timeout, cancelled, &lease)
            .await
    }

    pub(crate) async fn restart_and_wait_until_with_lease(
        self: &Arc<Self>,
        timeout: Duration,
        cancelled: Option<Arc<AtomicBool>>,
        lease: &crate::platform::data_coordinator::DataWriteLease,
    ) -> MihomoResult<()> {
        self.restart_and_wait_with_input(timeout, cancelled, lease, None)
            .await
    }

    pub(crate) async fn restart_and_wait_with_payload(
        self: &Arc<Self>,
        timeout: Duration,
        cancelled: Option<Arc<AtomicBool>>,
        lease: &crate::platform::data_coordinator::DataWriteLease,
        payload: Arc<str>,
    ) -> MihomoResult<()> {
        self.restart_and_wait_with_input(timeout, cancelled, lease, Some(payload))
            .await
    }

    async fn restart_and_wait_with_input(
        self: &Arc<Self>,
        timeout: Duration,
        cancelled: Option<Arc<AtomicBool>>,
        lease: &crate::platform::data_coordinator::DataWriteLease,
        payload: Option<Arc<str>>,
    ) -> MihomoResult<()> {
        if cancelled
            .as_ref()
            .is_some_and(|cancelled| cancelled.load(Ordering::Acquire))
        {
            return Err(MihomoError::Process(
                "内核重启在停止阶段开始前已取消".into(),
            ));
        }
        let process = self.clone();
        let access = crate::platform::data_coordinator::DataWriteAccess::new(&self.config.home_dir)
            .authorized(lease);
        let write_lease = access.acquire()?;
        let worker_cancelled = cancelled.clone();
        tokio::task::spawn_blocking(move || match payload.as_deref() {
            Some(payload) => process.restart_payload_with_write_lease(
                &write_lease,
                worker_cancelled.as_deref(),
                Some(payload),
            ),
            None => process.restart_with_write_lease(&write_lease, worker_cancelled.as_deref()),
        })
        .await
        .map_err(|error| MihomoError::Process(format!("内核重启后台任务异常结束：{error}")))??;
        let ready = self.wait_until_ready_until(timeout, cancelled).await;
        if let Err(error) = ready {
            let stop = self.stop_async().await;
            return Err(match stop {
                Ok(()) => MihomoError::Process(format!("{error}；已停止未就绪内核")),
                Err(stop) => MihomoError::Process(format!("{error}；停止未就绪内核失败：{stop}")),
            });
        }
        Ok(())
    }

    /// Polls the real `/version` endpoint until it responds, the child exits,
    /// or the supplied timeout elapses.
    ///
    /// # Errors
    ///
    /// Returns the last process log when the selected core exits early, or a timeout error.
    pub async fn wait_until_ready(&self, timeout: Duration) -> MihomoResult<()> {
        self.wait_until_ready_until(timeout, None).await
    }

    async fn wait_until_ready_until(
        &self,
        timeout: Duration,
        cancelled: Option<Arc<AtomicBool>>,
    ) -> MihomoResult<()> {
        let http = reqwest::Client::builder()
            .no_proxy()
            .user_agent(concat!("ZenClash/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if cancelled
                .as_ref()
                .is_some_and(|cancelled| cancelled.load(Ordering::Acquire))
            {
                return Err(MihomoError::Process("等待内核控制器时收到停止请求".into()));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(self.readiness_timeout_error());
            }
            if tokio::time::timeout(readiness_attempt_timeout(remaining), async {
                if let Some(controller) = self.native_controller()? {
                    let response = controller
                        .request("GET", "/version", None, remaining)
                        .await
                        .map_err(|error| MihomoError::Process(error.to_string()))?;
                    if response.status != 200 {
                        return Err(MihomoError::Process(
                            "Native controller is not ready".into(),
                        ));
                    }
                    serde_json::from_slice::<VersionInfo>(&response.body)?;
                    Ok(())
                } else {
                    probe_http_version(&http, &self.config.endpoint).await
                }
            })
            .await
            .is_ok_and(|result| result.is_ok())
            {
                return Ok(());
            }
            if let Some(error) = self.logs.read().current_listener_error() {
                return Err(MihomoError::Process(error));
            }
            if !self.is_running() {
                let message = self.logs.read().current_last_line().unwrap_or_else(|| {
                    format!("{} 在控制器就绪前退出", self.config.kind.display_name())
                });
                return Err(MihomoError::Process(message));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(self.readiness_timeout_error());
            }
            tokio::time::sleep(remaining.min(Duration::from_millis(100))).await;
        }
    }

    fn readiness_timeout_error(&self) -> MihomoError {
        MihomoError::Process(format!(
            "等待 {} 控制器 {} 超时",
            self.config.kind.display_name(),
            self.config.endpoint.controller
        ))
    }
}

pub(super) fn readiness_attempt_timeout(remaining: Duration) -> Duration {
    remaining.min(Duration::from_millis(750))
}

async fn probe_http_version(http: &reqwest::Client, endpoint: &MihomoEndpoint) -> MihomoResult<()> {
    let url = endpoint.http_url("/version")?;
    let mut request = http.get(url);
    if !endpoint.secret.is_empty() {
        request = request.bearer_auth(&endpoint.secret);
    }
    let response = request.send().await?;
    if !response.status().is_success() {
        return Err(MihomoError::Process(format!(
            "HTTP controller is not ready: {}",
            response.status()
        )));
    }
    let bytes = response.bytes().await?;
    serde_json::from_slice::<VersionInfo>(&bytes)?;
    Ok(())
}
