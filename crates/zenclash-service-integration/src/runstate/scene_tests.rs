// ZenClash adaptations, 2026-10-04. GPL-3.0-only; see NOTICE.md.
use super::*;

struct BlockingAuthorization {
    started: Arc<Notify>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl RunStateEnv for BlockingAuthorization {
    async fn probe_service_version(&self) -> Result<ServiceVersionReply> {
        bail!("fixture service absent")
    }
    async fn trusted_install_evidence(&self) -> Result<bool> {
        Ok(false)
    }
    fn is_elevated(&self) -> bool {
        false
    }
    fn set_pac_available(&self, _: bool) {}
    fn publish(&self, _: &RunState) {}
    fn run_privileged(&self, _: PendingAction) -> Result<()> {
        self.started.notify_one();
        self.release
            .lock()
            .map_err(|_| anyhow::anyhow!("fixture lock poisoned"))?
            .recv_timeout(Duration::from_secs(2))
            .context("async worker could not release a blocking authorization")
    }
}

#[tokio::test]
async fn authorization_wait_does_not_block_the_async_ui_worker() -> Result<()> {
    let (release, receiver) = std::sync::mpsc::channel();
    let started = Arc::new(Notify::new());
    let store = Arc::new(RunStateStore::new(BlockingAuthorization {
        started: Arc::clone(&started),
        release: std::sync::Mutex::new(receiver),
    }));
    let operation = tokio::spawn(async move { store.perform(PendingAction::Install).await });
    started.notified().await;
    release
        .send(())
        .context("blocking authorization finished before the async worker ran")?;
    operation.await??;
    Ok(())
}
