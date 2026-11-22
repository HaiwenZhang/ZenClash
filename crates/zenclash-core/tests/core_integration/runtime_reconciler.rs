use std::{
    any::Any,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
};

use anyhow::Result;
use tokio::sync::Notify;
use zenclash_core::{
    mihomo::{CoreConfigValidator, MihomoEndpoint, MihomoError, MihomoResult},
    platform::{AppInstanceLock, AppInstanceLockError, CoreKind},
    runtime::{
        CoreLaunchDescriptor, CoreProcessLauncher, CoreProcessLease, DesiredCoreIntent,
        IntentReconciler, KernelOwner, LeftoverLeaseTracker, LocalLauncher, ReconcileOutcome,
    },
    service::NativeController,
};

use crate::TempDir;

struct MockLease {
    descriptor: CoreLaunchDescriptor,
    pid: u32,
    running: AtomicBool,
    fail_stop: Arc<AtomicBool>,
    stop_calls: AtomicUsize,
}

impl MockLease {
    fn new(descriptor: CoreLaunchDescriptor, pid: u32, fail_stop: Arc<AtomicBool>) -> Self {
        Self {
            descriptor,
            pid,
            running: AtomicBool::new(true),
            fail_stop,
            stop_calls: AtomicUsize::new(0),
        }
    }
}

impl CoreProcessLease for MockLease {
    fn kind(&self) -> CoreKind {
        self.descriptor.kind
    }

    fn endpoint(&self) -> MihomoEndpoint {
        self.descriptor.endpoint.clone()
    }

    fn launch_descriptor(&self) -> CoreLaunchDescriptor {
        self.descriptor.clone()
    }

    fn write_scopes(&self) -> Vec<PathBuf> {
        vec![self.descriptor.home_dir.clone()]
    }

    fn config_validator(&self) -> CoreConfigValidator {
        CoreConfigValidator::new(
            self.descriptor.kind,
            &self.descriptor.binary,
            &self.descriptor.home_dir,
        )
    }

    fn native_controller(&self) -> MihomoResult<Option<NativeController>> {
        Ok(None)
    }

    fn stop_process(&self) -> MihomoResult<()> {
        self.stop_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_stop.load(Ordering::SeqCst) {
            return Err(MihomoError::Process("simulated stubborn child".into()));
        }
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        self.running.load(Ordering::SeqCst).then_some(self.pid)
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    fn into_any_arc(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

fn descriptor(root: &std::path::Path, tag: &str) -> CoreLaunchDescriptor {
    CoreLaunchDescriptor {
        kind: CoreKind::Mihomo,
        binary: root.join("mihomo"),
        config_file: root.join(format!("{tag}.yaml")),
        home_dir: root.join(format!("home-{tag}")),
        endpoint: MihomoEndpoint::new("http://127.0.0.1:19090", ""),
        controller_override: None,
    }
}

#[tokio::test]
async fn intent_reconciler_handles_start_coalesce_supersede_leftover_and_close() -> Result<()> {
    let temp = TempDir::new("reconciler")?;
    let leftovers = LeftoverLeaseTracker::new();
    let next_pid = Arc::new(AtomicU32::new(1000));
    let launch_count = Arc::new(AtomicUsize::new(0));
    let fail_stop = Arc::new(AtomicBool::new(false));
    let gate_entered = Arc::new(Notify::new());
    let gate_release = Arc::new(Notify::new());
    let hold_next_launch = Arc::new(AtomicBool::new(false));

    let launcher = LocalLauncher::new(leftovers.clone(), {
        let next_pid = Arc::clone(&next_pid);
        let launch_count = Arc::clone(&launch_count);
        let fail_stop = Arc::clone(&fail_stop);
        let gate_entered = Arc::clone(&gate_entered);
        let gate_release = Arc::clone(&gate_release);
        let hold_next_launch = Arc::clone(&hold_next_launch);
        move |desc| {
            let next_pid = Arc::clone(&next_pid);
            let launch_count = Arc::clone(&launch_count);
            let fail_stop = Arc::clone(&fail_stop);
            let gate_entered = Arc::clone(&gate_entered);
            let gate_release = Arc::clone(&gate_release);
            let hold_next_launch = Arc::clone(&hold_next_launch);
            Box::pin(async move {
                if hold_next_launch.swap(false, Ordering::SeqCst) {
                    gate_entered.notify_one();
                    gate_release.notified().await;
                }
                launch_count.fetch_add(1, Ordering::SeqCst);
                let pid = next_pid.fetch_add(1, Ordering::SeqCst);
                let lease: Arc<dyn CoreProcessLease> =
                    Arc::new(MockLease::new(desc, pid, fail_stop));
                Ok(lease)
            })
        }
    });
    assert_eq!(launcher.backend_name(), "local");

    let reconciler = IntentReconciler::new(leftovers.clone());
    let desc_v1 = descriptor(temp.path(), "v1");
    let desc_v2 = descriptor(temp.path(), "v2");
    let desc_v3 = descriptor(temp.path(), "v3");
    let desc_v4 = descriptor(temp.path(), "v4");

    // 1. Start initial desired intent.
    let started = reconciler
        .reconcile(DesiredCoreIntent::running(desc_v1.clone()), &launcher)
        .await?;
    assert_eq!(started, ReconcileOutcome::Applied { revision: 1 });
    assert_eq!(launch_count.load(Ordering::SeqCst), 1);
    let active = reconciler.active_lease().expect("active lease after start");
    assert!(active.is_running());
    assert_eq!(active.pid(), Some(1000));
    assert!(KernelOwner::Managed(active).is_managed());

    // 2. Identical desired intent coalesces into a no-op without launching again.
    let coalesced = reconciler
        .reconcile(DesiredCoreIntent::running(desc_v1), &launcher)
        .await?;
    assert_eq!(coalesced, ReconcileOutcome::Coalesced { revision: 2 });
    assert_eq!(launch_count.load(Ordering::SeqCst), 1);

    // 3. Submitting a newer intent while an earlier launch is in flight supersedes the older one.
    hold_next_launch.store(true, Ordering::SeqCst);
    let in_flight_reconciler = reconciler.clone();
    let in_flight_launcher = launcher.clone();
    let in_flight_desc = desc_v2.clone();
    let in_flight = tokio::spawn(async move {
        in_flight_reconciler
            .reconcile(
                DesiredCoreIntent::running(in_flight_desc),
                &in_flight_launcher,
            )
            .await
    });
    gate_entered.notified().await;
    let superseding_rev = reconciler.submit(DesiredCoreIntent::running(desc_v3.clone()))?;
    assert_eq!(superseding_rev, 4);
    gate_release.notify_one();

    let first_outcome = in_flight.await??;
    assert_eq!(
        first_outcome,
        ReconcileOutcome::Superseded {
            revision: 3,
            latest_revision: 4,
        }
    );
    let applied_latest = reconciler.reconcile_latest(&launcher).await?;
    assert_eq!(applied_latest, ReconcileOutcome::Applied { revision: 4 });
    assert_eq!(
        reconciler
            .active_lease()
            .expect("active lease after supersede")
            .launch_descriptor(),
        desc_v3
    );

    // 4. Failing to stop registers a leftover lease that blocks new launches until reclaimed.
    fail_stop.store(true, Ordering::SeqCst);
    let stop_error = reconciler
        .reconcile(DesiredCoreIntent::Stopped, &launcher)
        .await;
    assert!(stop_error.is_err());
    assert!(leftovers.has_pending());
    assert_eq!(leftovers.pending_count(), 1);
    assert!(reconciler.active_lease().is_none());

    // Trying to launch while leftover still cannot be stopped must fail (no dual-kernel).
    let blocked = reconciler
        .reconcile(DesiredCoreIntent::running(desc_v4.clone()), &launcher)
        .await;
    assert!(blocked.is_err());
    assert_eq!(leftovers.pending_count(), 1);

    // Allowing stop to succeed lets the next reconcile reclaim the leftover and launch cleanly.
    fail_stop.store(false, Ordering::SeqCst);
    let recovered = reconciler
        .reconcile(DesiredCoreIntent::running(desc_v4.clone()), &launcher)
        .await?;
    assert!(matches!(recovered, ReconcileOutcome::Applied { .. }));
    assert!(!leftovers.has_pending());
    assert_eq!(
        reconciler
            .active_lease()
            .expect("active lease after leftover recovery")
            .launch_descriptor(),
        desc_v4
    );

    // 5. Closing permanently stops the active lease and rejects further Running intents.
    let closed = reconciler
        .reconcile(DesiredCoreIntent::Closed, &launcher)
        .await?;
    assert!(matches!(closed, ReconcileOutcome::Applied { .. }));
    assert!(reconciler.is_closed());
    assert!(reconciler.active_lease().is_none());
    assert!(
        reconciler
            .submit(DesiredCoreIntent::running(desc_v2))
            .is_err()
    );

    Ok(())
}

#[test]
fn core_kind_capabilities_and_app_instance_lock_enforce_mutual_exclusion() -> Result<()> {
    assert_eq!("mihomo".parse::<CoreKind>()?, CoreKind::Mihomo);
    assert_eq!("  MiHoMo \n".parse::<CoreKind>()?, CoreKind::Mihomo);
    assert!("unsupported-core".parse::<CoreKind>().is_err());
    assert!("".parse::<CoreKind>().is_err());

    let kind = CoreKind::Mihomo;
    assert_eq!(kind.display_name(), "Mihomo");
    assert_eq!(kind.executable_stem(), "mihomo");
    assert_eq!(kind.home_environment_variable(), "ZENCLASH_MIHOMO_HOME");
    let caps = kind.capabilities();
    assert!(caps.config_validation);
    assert!(caps.full_config_reload);
    assert!(caps.rule_toggle);
    assert!(caps.ruleset_conversion);

    let temp = TempDir::new("instance-lock")?;
    let lock_path = temp.path().join("zenclash.lock");

    let first_lock = AppInstanceLock::acquire(&lock_path)?;
    let contended = AppInstanceLock::acquire(&lock_path);
    assert!(matches!(
        contended,
        Err(AppInstanceLockError::AlreadyRunning(_))
    ));
    drop(first_lock);

    // After releasing the first lock, a subsequent acquisition succeeds immediately.
    let second_lock = AppInstanceLock::acquire(&lock_path)?;
    drop(second_lock);

    Ok(())
}
