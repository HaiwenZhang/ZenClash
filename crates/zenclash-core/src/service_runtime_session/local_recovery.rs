//! Ordinary recovery after a confirmed service stop and complete cache export.

use super::*;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

pub(crate) struct LocalRecoveryResult {
    pub(crate) failure: Option<MihomoError>,
    pub(crate) bundle: Arc<ServiceRuntimeBundle>,
}

impl<T: RuntimeTransport> RuntimeSession<T> {
    // The caller owns capture publication, transition, binding mutation and a lease
    // covering ordinary launch/store paths. The publish callback retains binding mutation.
    pub(crate) async fn recover_local_runtime<C, F>(
        self: &Arc<Self>,
        store: &crate::ControlledConfigStore,
        original: crate::MihomoLaunchConfig,
        cancelled: Arc<AtomicBool>,
        timeout: Duration,
        publish: C,
    ) -> MihomoResult<LocalRecoveryResult>
    where
        C: FnOnce(Arc<crate::MihomoProcess>) -> F + Send + 'static,
        F: Future<Output = MihomoResult<OwnedMutexGuard<()>>> + Send + 'static,
    {
        if original.kind != crate::CoreKind::Mihomo || original.home_dir != self.source_home {
            return Err(unknown());
        }
        let binary = original.binary.clone();
        tokio::task::spawn_blocking(move || crate::verify_ordinary_local_executable(&binary))
            .await
            .map_err(|_| unknown())??;
        if cancelled.load(Ordering::Acquire) {
            return Err(unknown());
        }
        let bundle = self.stop_and_export().await?;
        let mutation = store.lock_service_tun_mutation().await;
        self.recover_held_local_runtime(
            (store, mutation),
            original,
            bundle,
            cancelled,
            timeout,
            publish,
        )
        .await
    }

    // Initial handover failures have no accepted service snapshot to export. Their
    // pre-authorisation bundle must remain authoritative even if sources disappear.
    pub(crate) async fn recover_held_local_runtime<C, F>(
        self: &Arc<Self>,
        admission: (&crate::ControlledConfigStore, OwnedMutexGuard<()>),
        original: crate::MihomoLaunchConfig,
        bundle: Arc<ServiceRuntimeBundle>,
        cancelled: Arc<AtomicBool>,
        timeout: Duration,
        publish: C,
    ) -> MihomoResult<LocalRecoveryResult>
    where
        C: FnOnce(Arc<crate::MihomoProcess>) -> F + Send + 'static,
        F: Future<Output = MihomoResult<OwnedMutexGuard<()>>> + Send + 'static,
    {
        let (store, mutation) = admission;
        if !store.owns_service_tun_mutation(&mutation) {
            return Err(unknown());
        }
        if original.kind != crate::CoreKind::Mihomo || original.home_dir != self.source_home {
            return Err(unknown());
        }
        let binary = original.binary.clone();
        tokio::task::spawn_blocking(move || crate::verify_ordinary_local_executable(&binary))
            .await
            .map_err(|_| unknown())??;
        if cancelled.load(Ordering::Acquire) {
            return Err(unknown());
        }
        // Confirm Stop/Release before changing GeoData in the shared original home.
        // An uncertain Release never reaches publication or ordinary Start.
        self.release_owned().await?;
        let held = Arc::new(bundle.with_delta(&serde_json::json!({"tun":{"enable":false}}))?);
        // No Local kernel is active while this binding is a service owner.
        let lease = store
            .acquire_write_lease_for_paths(vec![self.source_home.clone()])
            .await
            .map_err(|_| unknown())?;
        let ((config, local_payload), lease, mutation) = bundle
            .materialize_local_runtime_admitted(store, None, lease, mutation)
            .await?;
        let process = tokio::task::spawn_blocking(move || {
            let mut launch =
                crate::MihomoLaunchConfig::new(original.binary, config, original.home_dir)?;
            if original.controller_override.is_some() {
                launch = launch.with_controller_endpoint(original.endpoint);
            }
            Ok::<_, MihomoError>(crate::MihomoProcess::prepare_stopped(launch))
        })
        .await
        .map_err(|_| unknown())??;
        bundle
            .with_local_geodata_admitted(
                store,
                self.source_home.clone(),
                lease,
                mutation,
                move |recovery| async move {
                    recovery.validate_local_process(&process).await?;
                    if cancelled.load(Ordering::Acquire) {
                        return Err(unknown());
                    }
                    let _mutation = publish(process.clone()).await?;
                    let started = recovery
                        .restart_local_process(&process, timeout, Some(cancelled.clone()))
                        .await;
                    let started = started.and_then(|()| {
                        if cancelled.load(Ordering::Acquire) {
                            Err(unknown())
                        } else {
                            Ok(())
                        }
                    });
                    match started {
                        Ok(()) => Ok(LocalRecoveryResult {
                            // A live published Local child still uses activated GeoData when saving
                            // fails. Preserve those resources and prohibit native maintenance.
                            failure: recovery
                                .persist_local_payload(local_payload)
                                .await
                                .err()
                                .map(|error| {
                                    MihomoError::Process(zenclash_i18n::text_with(
                                        "core_page.service.local_save_failed",
                                        &[("error", error.to_string())],
                                    ))
                                }),
                            bundle: held,
                        }),
                        Err(error) => match process.stop_async().await {
                            Ok(()) => Err(error),
                            // Keep activated GeoData if the published child cannot be confirmed stopped.
                            // Returning success to the filesystem transaction prevents unsafe rollback;
                            // the application outcome still carries the failure and blocks maintenance.
                            Err(stop) => Ok(LocalRecoveryResult {
                                failure: Some(stop),
                                bundle: held,
                            }),
                        },
                    }
                },
            )
            .await
    }
}
