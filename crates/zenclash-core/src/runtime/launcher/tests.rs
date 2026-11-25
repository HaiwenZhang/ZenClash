use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use parking_lot::Mutex;
use tokio::sync::Notify;

use super::*;
use crate::mihomo::errors::MihomoError;

struct FakeLease {
    running: AtomicBool,
    fail_stop: AtomicBool,
    stop_calls: AtomicUsize,
    pid: Option<u32>,
    descriptor: CoreLaunchDescriptor,
}

impl FakeLease {
    fn new(running: bool, fail_stop: bool, pid: Option<u32>) -> Arc<Self> {
        Self::with_descriptor(running, fail_stop, pid, sample_descriptor("default"))
    }

    fn with_descriptor(
        running: bool,
        fail_stop: bool,
        pid: Option<u32>,
        descriptor: CoreLaunchDescriptor,
    ) -> Arc<Self> {
        Arc::new(Self {
            running: AtomicBool::new(running),
            fail_stop: AtomicBool::new(fail_stop),
            stop_calls: AtomicUsize::new(0),
            pid,
            descriptor,
        })
    }
}

fn sample_descriptor(tag: &str) -> CoreLaunchDescriptor {
    CoreLaunchDescriptor {
        kind: CoreKind::Mihomo,
        binary: PathBuf::from(format!("/fake/{tag}/mihomo")),
        config_file: PathBuf::from(format!("/fake/{tag}/config.yaml")),
        home_dir: PathBuf::from(format!("/fake/{tag}")),
        endpoint: MihomoEndpoint::new(format!("http://127.0.0.1:9090/{tag}"), "secret"),
        controller_override: None,
    }
}

impl CoreProcessLease for FakeLease {
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
            self.descriptor.binary.clone(),
            self.descriptor.home_dir.clone(),
        )
    }

    fn native_controller(&self) -> MihomoResult<Option<NativeController>> {
        Ok(None)
    }

    fn stop_process(&self) -> MihomoResult<()> {
        self.stop_calls.fetch_add(1, Ordering::Relaxed);
        if self.fail_stop.load(Ordering::Acquire) {
            return Err(MihomoError::Process("stop still pending".into()));
        }
        self.running.store(false, Ordering::Release);
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        self.running
            .load(Ordering::Acquire)
            .then_some(self.pid)
            .flatten()
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    fn into_any_arc(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

#[test]
fn leftover_tracker_blocks_new_start_until_previous_lease_exits() {
    let tracker = LeftoverLeaseTracker::new();
    let lease = FakeLease::new(true, true, Some(4242));
    let id = tracker.register(lease.clone());
    assert_eq!(id, 1);
    assert!(tracker.has_pending());
    assert!(tracker.ensure_reclaimed_before_start().is_err());
    assert_eq!(tracker.pending_count(), 1);
    assert_eq!(lease.stop_calls.load(Ordering::Relaxed), 1);

    lease.fail_stop.store(false, Ordering::Release);
    tracker.ensure_reclaimed_before_start().unwrap();
    assert!(!tracker.has_pending());
    assert_eq!(lease.stop_calls.load(Ordering::Relaxed), 2);
}

#[test]
fn leftover_tracker_skips_stop_for_already_exited_lease() {
    let tracker = LeftoverLeaseTracker::new();
    let lease = FakeLease::new(false, true, None);
    tracker.register(lease.clone());
    tracker.reclaim_all().unwrap();
    assert!(!tracker.has_pending());
    assert_eq!(lease.stop_calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn local_and_service_launchers_gate_on_leftovers_before_spawning() {
    let leftovers = LeftoverLeaseTracker::new();
    let stuck = FakeLease::new(true, true, Some(1001));
    leftovers.register(stuck.clone());

    let local_spawns = Arc::new(AtomicUsize::new(0));
    let local_counter = Arc::clone(&local_spawns);
    let local = LocalLauncher::with_blocking(leftovers.clone(), move |descriptor| {
        local_counter.fetch_add(1, Ordering::Relaxed);
        Ok(FakeLease::with_descriptor(
            true,
            false,
            Some(2001),
            descriptor,
        ))
    });
    assert_eq!(local.backend_name(), "local");

    let service_spawns = Arc::new(AtomicUsize::new(0));
    let service_counter = Arc::clone(&service_spawns);
    let service = ServiceLauncher::with_blocking(
        PathBuf::from("/fake/home"),
        Some(PathBuf::from("/fake/mihomo")),
        leftovers.clone(),
        move |descriptor| {
            service_counter.fetch_add(1, Ordering::Relaxed);
            Ok(FakeLease::with_descriptor(
                true,
                false,
                Some(3001),
                descriptor,
            ))
        },
    );
    assert_eq!(service.backend_name(), "service");
    assert_eq!(service.source_home(), Path::new("/fake/home"));
    assert_eq!(service.core_source(), Some(Path::new("/fake/mihomo")));

    let desc = sample_descriptor("alpha");
    assert!(local.launch(&desc).await.is_err());
    assert!(service.launch(&desc).await.is_err());
    assert_eq!(local_spawns.load(Ordering::Relaxed), 0);
    assert_eq!(service_spawns.load(Ordering::Relaxed), 0);

    // Allow leftover to exit; both launchers now succeed cleanly.
    stuck.fail_stop.store(false, Ordering::Release);
    let local_lease = local.launch(&desc).await.unwrap();
    assert_eq!(local_lease.pid(), Some(2001));
    assert_eq!(local_spawns.load(Ordering::Relaxed), 1);

    let service_lease = service.launch(&desc).await.unwrap();
    assert_eq!(service_lease.pid(), Some(3001));
    assert_eq!(service_spawns.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn reconciler_coalesces_redundant_intents_and_restarts_on_crash_or_change() {
    let leftovers = LeftoverLeaseTracker::new();
    let reconciler = IntentReconciler::new(leftovers.clone());
    let launched = Arc::new(Mutex::new(Vec::<Arc<FakeLease>>::new()));
    let launched_ref = Arc::clone(&launched);
    let launcher = LocalLauncher::with_blocking(leftovers, move |descriptor| {
        let lease = FakeLease::with_descriptor(true, false, Some(500), descriptor);
        launched_ref.lock().push(Arc::clone(&lease));
        Ok(lease)
    });

    // Compile-time check that reconcile futures remain Send.
    fn assert_send<T: Send>(_: &T) {}
    let fut = reconciler.reconcile(DesiredCoreIntent::Stopped, &launcher);
    assert_send(&fut);
    assert_eq!(
        fut.await.unwrap(),
        ReconcileOutcome::Coalesced { revision: 1 }
    );

    // First Running applies.
    let desc_a = sample_descriptor("a");
    assert_eq!(
        reconciler
            .reconcile(DesiredCoreIntent::running(desc_a.clone()), &launcher)
            .await
            .unwrap(),
        ReconcileOutcome::Applied { revision: 2 }
    );
    assert_eq!(launched.lock().len(), 1);

    // Identical Running while lease is alive coalesces without restarting.
    assert_eq!(
        reconciler
            .reconcile(DesiredCoreIntent::running(desc_a.clone()), &launcher)
            .await
            .unwrap(),
        ReconcileOutcome::Coalesced { revision: 3 }
    );
    assert_eq!(launched.lock().len(), 1);

    // If the lease crashes externally, identical Running does NOT coalesce; it relaunches.
    launched.lock()[0].running.store(false, Ordering::Release);
    assert_eq!(
        reconciler
            .reconcile(DesiredCoreIntent::running(desc_a), &launcher)
            .await
            .unwrap(),
        ReconcileOutcome::Applied { revision: 4 }
    );
    assert_eq!(launched.lock().len(), 2);

    // Changing descriptor stops old lease and launches new one.
    let desc_b = sample_descriptor("b");
    assert_eq!(
        reconciler
            .reconcile(DesiredCoreIntent::running(desc_b), &launcher)
            .await
            .unwrap(),
        ReconcileOutcome::Applied { revision: 5 }
    );
    assert_eq!(launched.lock().len(), 3);
    assert!(!launched.lock()[1].is_running());
    assert!(launched.lock()[2].is_running());
}

#[tokio::test]
async fn reconciler_supersedes_queued_and_in_flight_intents_without_leaking_leases() {
    let leftovers = LeftoverLeaseTracker::new();
    let reconciler = IntentReconciler::new(leftovers.clone());

    let first_launch_entered = Arc::new(Notify::new());
    let release_first_launch = Arc::new(Notify::new());
    let launched_tags = Arc::new(Mutex::new(Vec::<(String, Arc<FakeLease>)>::new()));

    let entered = Arc::clone(&first_launch_entered);
    let release = Arc::clone(&release_first_launch);
    let tags = Arc::clone(&launched_tags);
    let call_count = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&call_count);

    let launcher = Arc::new(LocalLauncher::new(leftovers, move |descriptor| {
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        let tags = Arc::clone(&tags);
        let n = calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            if n == 0 {
                entered.notify_one();
                release.notified().await;
            }
            let tag = descriptor.home_dir.to_string_lossy().into_owned();
            let lease = FakeLease::with_descriptor(true, false, Some(600 + n as u32), descriptor);
            tags.lock().push((tag, Arc::clone(&lease)));
            Ok(lease as Arc<dyn CoreProcessLease>)
        })
    }));

    // Start rev 1 (blocks inside launch).
    let r1 = reconciler.clone();
    let l1 = Arc::clone(&launcher);
    let task1 = tokio::spawn(async move {
        r1.reconcile(
            DesiredCoreIntent::running(sample_descriptor("one")),
            l1.as_ref(),
        )
        .await
    });

    first_launch_entered.notified().await;

    // While rev 1 is in flight, queue rev 2 ("two") then rev 3 ("three").
    let r2 = reconciler.clone();
    let l2 = Arc::clone(&launcher);
    let rev2 = r2
        .submit(DesiredCoreIntent::running(sample_descriptor("two")))
        .unwrap();
    assert_eq!(rev2, 2);

    let rev3 = reconciler
        .submit(DesiredCoreIntent::running(sample_descriptor("three")))
        .unwrap();
    assert_eq!(rev3, 3);

    // Start a task pinned to rev 2 while rev 1 still holds the worker gate.
    let task2 = tokio::spawn(async move { r2.reconcile_revision(Some(rev2), l2.as_ref()).await });

    // Release rev 1: it must detect superseding, stop its stale lease, and return Superseded.
    release_first_launch.notify_one();
    let out1 = task1.await.unwrap().unwrap();
    assert_eq!(
        out1,
        ReconcileOutcome::Superseded {
            revision: 1,
            latest_revision: 3,
        }
    );

    // Rev 2 must immediately observe superseding by rev 3 and NEVER launch "two".
    let out2 = task2.await.unwrap().unwrap();
    assert_eq!(
        out2,
        ReconcileOutcome::Superseded {
            revision: 2,
            latest_revision: 3,
        }
    );

    // Worker steps once to apply latest (rev 3 = "three").
    let out3 = reconciler
        .reconcile_latest(launcher.as_ref())
        .await
        .unwrap();
    assert_eq!(out3, ReconcileOutcome::Applied { revision: 3 });

    let recorded = launched_tags.lock();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].0, "/fake/one");
    assert!(
        !recorded[0].1.is_running(),
        "superseded lease must be stopped"
    );
    assert_eq!(recorded[1].0, "/fake/three");
    assert!(recorded[1].1.is_running(), "latest lease must stay active");
}

#[tokio::test]
async fn reconciler_gates_new_running_launch_on_unreclaimed_leftover_lease() {
    let leftovers = LeftoverLeaseTracker::new();
    let reconciler = IntentReconciler::new(leftovers.clone());
    let launched = Arc::new(Mutex::new(Vec::<Arc<FakeLease>>::new()));
    let launched_ref = Arc::clone(&launched);

    let launcher = LocalLauncher::with_blocking(leftovers.clone(), move |descriptor| {
        let lease = FakeLease::with_descriptor(true, false, Some(700), descriptor);
        launched_ref.lock().push(Arc::clone(&lease));
        Ok(lease)
    });

    // Start initial lease A.
    assert_eq!(
        reconciler
            .reconcile(
                DesiredCoreIntent::running(sample_descriptor("a")),
                &launcher,
            )
            .await
            .unwrap(),
        ReconcileOutcome::Applied { revision: 1 }
    );
    let first_lease = Arc::clone(&launched.lock()[0]);

    // Make lease A fail to confirm exit.
    first_lease.fail_stop.store(true, Ordering::Release);
    assert!(
        reconciler
            .reconcile(DesiredCoreIntent::Stopped, &launcher)
            .await
            .is_err()
    );
    assert!(leftovers.has_pending());
    assert!(reconciler.active_lease().is_none());

    // Attempt to start lease B while A is still an unreclaimed leftover: MUST fail without spawning B.
    assert!(
        reconciler
            .reconcile(
                DesiredCoreIntent::running(sample_descriptor("b")),
                &launcher,
            )
            .await
            .is_err()
    );
    assert_eq!(
        launched.lock().len(),
        1,
        "dual kernel launch must be blocked"
    );

    // Once leftover A confirms exit, starting B reclaims A and launches B.
    first_lease.fail_stop.store(false, Ordering::Release);
    assert_eq!(
        reconciler
            .reconcile(
                DesiredCoreIntent::running(sample_descriptor("b")),
                &launcher,
            )
            .await
            .unwrap(),
        ReconcileOutcome::Applied { revision: 4 }
    );
    assert!(!leftovers.has_pending());
    assert_eq!(launched.lock().len(), 2);
    assert!(!first_lease.is_running());
    assert!(launched.lock()[1].is_running());
}

#[tokio::test]
async fn reconciler_closed_state_stops_active_lease_and_rejects_future_intents() {
    let leftovers = LeftoverLeaseTracker::new();
    let reconciler = IntentReconciler::new(leftovers.clone());
    let launched = Arc::new(Mutex::new(Vec::<Arc<FakeLease>>::new()));
    let launched_ref = Arc::clone(&launched);

    let launcher = LocalLauncher::with_blocking(leftovers, move |descriptor| {
        let lease = FakeLease::with_descriptor(true, false, Some(800), descriptor);
        launched_ref.lock().push(Arc::clone(&lease));
        Ok(lease)
    });

    reconciler
        .reconcile(
            DesiredCoreIntent::running(sample_descriptor("run")),
            &launcher,
        )
        .await
        .unwrap();
    assert_eq!(launched.lock().len(), 1);

    // Close stops the active lease.
    assert_eq!(
        reconciler
            .reconcile(DesiredCoreIntent::Closed, &launcher)
            .await
            .unwrap(),
        ReconcileOutcome::Applied { revision: 2 }
    );
    assert!(reconciler.is_closed());
    assert!(!launched.lock()[0].is_running());
    assert!(reconciler.active_lease().is_none());

    // Repeat Closed coalesces.
    assert_eq!(
        reconciler
            .reconcile(DesiredCoreIntent::Closed, &launcher)
            .await
            .unwrap(),
        ReconcileOutcome::Coalesced { revision: 3 }
    );

    // Subsequent Running or Stopped intents are rejected.
    assert!(
        reconciler
            .reconcile(
                DesiredCoreIntent::running(sample_descriptor("after-close")),
                &launcher,
            )
            .await
            .is_err()
    );
    assert!(
        reconciler
            .reconcile(DesiredCoreIntent::Stopped, &launcher)
            .await
            .is_err()
    );
    assert_eq!(launched.lock().len(), 1);
}

#[tokio::test]
async fn reconciler_batch_submit_jumps_straight_to_latest_revision() {
    let leftovers = LeftoverLeaseTracker::new();
    let reconciler = IntentReconciler::new(leftovers.clone());
    let launched_homes = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let homes_ref = Arc::clone(&launched_homes);

    let launcher = LocalLauncher::with_blocking(leftovers, move |descriptor| {
        homes_ref.lock().push(descriptor.home_dir.clone());
        Ok(FakeLease::with_descriptor(
            true,
            false,
            Some(900),
            descriptor,
        ))
    });

    assert_eq!(
        reconciler
            .submit(DesiredCoreIntent::running(sample_descriptor("r1")))
            .unwrap(),
        1
    );
    assert_eq!(
        reconciler
            .submit(DesiredCoreIntent::running(sample_descriptor("r2")))
            .unwrap(),
        2
    );
    assert_eq!(
        reconciler
            .submit(DesiredCoreIntent::running(sample_descriptor("r3")))
            .unwrap(),
        3
    );
    assert_eq!(reconciler.latest_revision(), 3);
    assert_eq!(reconciler.applied_revision(), 0);

    assert_eq!(
        reconciler.reconcile_latest(&launcher).await.unwrap(),
        ReconcileOutcome::Applied { revision: 3 }
    );
    assert_eq!(reconciler.applied_revision(), 3);
    assert_eq!(*launched_homes.lock(), vec![PathBuf::from("/fake/r3")]);

    // Reconciling latest again with no change coalesces.
    assert_eq!(
        reconciler.reconcile_latest(&launcher).await.unwrap(),
        ReconcileOutcome::Coalesced { revision: 3 }
    );
    assert_eq!(launched_homes.lock().len(), 1);
}
