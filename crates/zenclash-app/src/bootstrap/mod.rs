//! Headless multi-stage application bootstrap and recovery orchestration.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};

use zenclash_core::{
    AppDataPaths, AppInstanceLock, AppPreferences, AppPreferencesError, AppPreferencesStore,
    ControlledConfigStore, CoreInitializationOutcome, CoreKind, CoreSession, LogMonitor,
    MihomoClient, MihomoRuntimeResources, ProfileStore, ServiceHealthKind, SystemProxyController,
    SystemProxySession, TrafficCaptureSession, TrafficHistoryStore, TrafficMonitor,
    YamlOverrideStore,
};

use crate::{SafeMode, TrafficHistorySession, shutdown::finish_native_shutdown};

mod core_startup;
mod routing;
#[cfg(test)]
mod tests;

pub use routing::{
    StartupBlocked, StartupCancellation, StartupFailure, StartupLayers, StartupPlatform,
    blocked_message, connection_message, effective_tun_enabled, is_startup_cancelled,
    permits_local_fallback, prepare_configuration_layers, route_startup_with_policy,
    startup_cancelled_error,
};

/// Process arguments prepared only after capture and kernel shutdown succeeds.
#[derive(Clone, Debug)]
pub struct RestartRequest {
    /// Executable to launch again.
    pub executable: PathBuf,
    /// Explicit local-kernel choice for the new session.
    pub continue_local: bool,
}

/// Runtime services supplied to the application shell.
pub struct AppServices {
    /// Defer capture restoration and background commands until bootstrap completes.
    pub initializing: bool,
    /// Watch for delayed Windows service readiness when saved TUN remains requested.
    pub await_service_handoff: bool,
    /// Profile repository opened during bootstrap.
    pub profile_store: Option<ProfileStore>,
    /// YAML override repository opened during bootstrap.
    pub override_store: Option<YamlOverrideStore>,
    /// Persistent application-preference store discovered during process bootstrap.
    pub preferences_store: Option<AppPreferencesStore>,
    /// Single validated preference snapshot used consistently for startup and UI state.
    pub preferences: AppPreferences,
    /// Explicit runtime core selected for this application process.
    pub core_kind: CoreKind,
    /// Serialized runtime-core transition owner.
    pub core_session: CoreSession,
    /// Capture owner shared with final process cleanup, including native quit.
    pub traffic_capture: TrafficCaptureSession,
    /// Native proxy session shared with the runtime page's settings editor.
    pub system_proxy_session: Option<SystemProxySession>,
    /// Typed client following the shared runtime binding.
    pub client: MihomoClient,
    /// Shared reconnecting traffic stream.
    pub traffic_monitor: Arc<TrafficMonitor>,
    /// Shared reconnecting log stream.
    pub log_monitor: Arc<LogMonitor>,
    /// Traffic-history store discovered before the UI starts handling events.
    pub traffic_history_store: Option<TrafficHistoryStore>,
    /// Owned sampler shared with the executable for final shutdown flushing.
    pub traffic_history_session: Option<Arc<TrafficHistorySession>>,
    /// Active Clash/Mihomo YAML path, when known.
    pub profile_path: Option<PathBuf>,
    /// Persistent controlled-config layer merged over the active profile.
    pub controlled_config_store: ControlledConfigStore,
    /// Tokio runtime used for network and blocking bridge tasks.
    pub runtime: tokio::runtime::Handle,
    /// Visible explanation when startup recovered from an unusable preferred core.
    pub startup_notice: Option<String>,
    /// Persistent startup failure shown while the app runs in offline recovery mode.
    pub startup_error: Option<String>,
    /// Deferred application restart request consumed after the instance lock is released.
    pub restart_after_exit: Arc<parking_lot::Mutex<Option<RestartRequest>>>,
    /// Safe Mode policy active for this process.
    pub safe_mode: SafeMode,
}

/// Services and verified startup receipt published by background bootstrap.
pub struct BootstrappedApplication {
    /// Initialized application services.
    pub services: AppServices,
    /// Initialization receipt consumed before profile commands can begin.
    pub initialization: Option<CoreInitializationOutcome>,
}

/// Explicit outcome of running [`BootstrapOrchestrator::prepare_application`].
pub enum StartupOutcome {
    /// Core and runtime services started normally (managed or external).
    Ready(Box<BootstrappedApplication>),
    /// Core startup was blocked or failed; UI opens in offline recovery mode.
    Offline(Box<BootstrappedApplication>, String),
    /// Application shutdown cancelled startup before completion.
    Cancelled,
    /// Unrecoverable bootstrap failure before offline recovery could open.
    Failed(String),
}

impl StartupOutcome {
    /// Converts this outcome into the `Result<BootstrappedApplication, String>`
    /// expected by the UI shell's pending-startup completion handler.
    ///
    /// # Errors
    ///
    /// Returns an error string when startup was cancelled or failed before
    /// offline recovery services could be created.
    pub fn into_application_result(self) -> Result<BootstrappedApplication, String> {
        match self {
            Self::Ready(app) | Self::Offline(app, _) => Ok(*app),
            Self::Cancelled => Err(startup_cancelled_error().to_string()),
            Self::Failed(error) => Err(error),
        }
    }
}

/// Owned runtime handles recorded during bootstrap so native post-exit cleanup
/// can always release traffic capture, flush history, and stop the core.
pub struct BootstrapOwner {
    /// Serialized runtime session.
    pub core: CoreSession,
    /// Owned traffic-capture session, once created.
    pub capture: Option<TrafficCaptureSession>,
    /// Owned traffic-history sampler, once created.
    pub history: Option<Arc<TrafficHistorySession>>,
}

impl BootstrapOwner {
    /// Runs native best-effort shutdown across capture, history, and core.
    ///
    /// # Errors
    ///
    /// Returns all cleanup failures joined together.
    pub async fn finish_native_shutdown(&self) -> Result<(), String> {
        finish_native_shutdown(&self.core, self.capture.as_ref(), self.history.as_deref()).await
    }
}

/// Shared slot populated by [`BootstrapOrchestrator::prepare_application`] for
/// post-event-loop cleanup.
pub type BootstrapCleanup = Arc<parking_lot::Mutex<Option<BootstrapOwner>>>;

/// Validated stores and preferences discovered once during process startup.
pub struct BootstrapInputs {
    /// Persistent preferences store discovered during bootstrap.
    pub preferences_store: Option<AppPreferencesStore>,
    /// Loaded preferences snapshot used consistently across startup and UI.
    pub preferences: AppPreferences,
    /// Requested core kind (env override or saved preference).
    pub requested_core: CoreKind,
    /// Explicit `ZENCLASH_CORE` environment override, if set.
    pub environment_core: Option<CoreKind>,
    /// Controlled-configuration store discovered during bootstrap.
    pub controlled_config_store: ControlledConfigStore,
    /// Managed profile store discovered during bootstrap.
    pub profile_store: ProfileStore,
    /// Managed YAML override store discovered during bootstrap.
    pub override_store: YamlOverrideStore,
    /// Optional traffic-history store discovered during bootstrap.
    pub traffic_history_store: Option<TrafficHistoryStore>,
    /// Recovery notices produced while loading or quarantining initial stores.
    pub recovery_notices: Vec<String>,
    /// Safe Mode policy active for this process.
    pub safe_mode: SafeMode,
}

/// Stateless entry point for ZenClash's headless multi-stage bootstrap.
pub struct BootstrapOrchestrator;

impl BootstrapOrchestrator {
    /// Discovers all persistent stores ONCE for the process, acquires the
    /// single-instance lock, recovers any interrupted backup restore, loads
    /// preferences, and configures the process locale.
    ///
    /// When `safe_mode` is enabled, all stores and the instance lock are
    /// isolated under `<data_root>/safe-mode`.
    ///
    /// # Errors
    ///
    /// Returns an error if the single-instance lock is already held, backup
    /// recovery fails, preferences fail to load, `ZENCLASH_CORE` is invalid, or
    /// required stores cannot be opened.
    pub fn discover_inputs(
        safe_mode: SafeMode,
    ) -> Result<(BootstrapInputs, Option<AppInstanceLock>), Box<dyn std::error::Error>> {
        if safe_mode.is_enabled() {
            return Self::discover_safe_mode_inputs(safe_mode);
        }
        let preferences_store = match AppPreferencesStore::discover() {
            Ok(store) => Some(store),
            Err(error) => {
                tracing::warn!(%error, "failed to discover preferences; using defaults");
                None
            }
        };
        let instance_lock = if let Some(store) = preferences_store.as_ref() {
            Some(AppInstanceLock::acquire(
                store.path().with_file_name("instance.lock"),
            )?)
        } else {
            tracing::warn!("application data directory unavailable; instance locking is disabled");
            None
        };
        if let Some(store) = preferences_store.as_ref()
            && let Some(root) = store.path().parent()
        {
            zenclash_core::BackupManager::new(root).recover_interrupted_restore()?;
        }
        let (preferences, preferences_recovery_notice) =
            load_preferences(preferences_store.as_ref())?;
        zenclash_i18n::set_locale(preferences.language.locale());
        let environment_core = std::env::var("ZENCLASH_CORE")
            .ok()
            .map(|value| value.parse())
            .transpose()?;
        let requested_core = environment_core.unwrap_or(preferences.core_kind);
        let controlled_config_store = ControlledConfigStore::discover()?;
        let recovery_notices = preferences_recovery_notice.into_iter().collect::<Vec<_>>();
        let profile_store = ProfileStore::discover()?;
        let override_store = YamlOverrideStore::discover()?;
        let traffic_history_store = TrafficHistoryStore::discover()
            .inspect_err(
                |error| tracing::warn!(%error, "failed to discover traffic-history database"),
            )
            .ok();
        Ok((
            BootstrapInputs {
                preferences_store,
                preferences,
                requested_core,
                environment_core,
                controlled_config_store,
                profile_store,
                override_store,
                traffic_history_store,
                recovery_notices,
                safe_mode,
            },
            instance_lock,
        ))
    }

    fn discover_safe_mode_inputs(
        safe_mode: SafeMode,
    ) -> Result<(BootstrapInputs, Option<AppInstanceLock>), Box<dyn std::error::Error>> {
        let base_paths = AppDataPaths::discover().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "application data directory unavailable for Safe Mode",
            )
        })?;
        let isolated_root = safe_mode.data_root(base_paths.root());
        std::fs::create_dir_all(&isolated_root)?;
        let isolated = AppDataPaths::new(isolated_root);
        let instance_lock = Some(AppInstanceLock::acquire(
            safe_mode.instance_lock_path(base_paths.root()),
        )?);
        zenclash_core::BackupManager::new(isolated.root()).recover_interrupted_restore()?;
        let preferences_store = Some(AppPreferencesStore::new(isolated.preferences()));
        let (preferences, preferences_recovery_notice) =
            load_preferences(preferences_store.as_ref())?;
        zenclash_i18n::set_locale(preferences.language.locale());
        let environment_core = std::env::var("ZENCLASH_CORE")
            .ok()
            .map(|value| value.parse())
            .transpose()?;
        let requested_core = environment_core.unwrap_or(preferences.core_kind);
        let controlled_config_store = ControlledConfigStore::new(isolated.controlled_config());
        let recovery_notices = preferences_recovery_notice.into_iter().collect::<Vec<_>>();
        let profile_store = ProfileStore::new(isolated.profiles())?;
        let override_store = YamlOverrideStore::new(isolated.yaml_overrides())?;
        let traffic_history_store = Some(TrafficHistoryStore::new(
            isolated.root().join("traffic-history.sqlite3"),
        ));
        tracing::info!(
            data_root = %isolated.root().display(),
            "Safe Mode active: isolated data root configured and host side effects disabled"
        );
        Ok((
            BootstrapInputs {
                preferences_store,
                preferences,
                requested_core,
                environment_core,
                controlled_config_store,
                profile_store,
                override_store,
                traffic_history_store,
                recovery_notices,
                safe_mode,
            },
            instance_lock,
        ))
    }

    /// Builds lightweight placeholder services so the UI window can open
    /// immediately while core and service preparation runs in the background.
    ///
    /// # Errors
    ///
    /// Returns an error if the disconnected placeholder `CoreSession` cannot be created.
    pub fn prepare_pending_services(
        inputs: &BootstrapInputs,
        runtime: &tokio::runtime::Handle,
        restart_after_exit: Arc<parking_lot::Mutex<Option<RestartRequest>>>,
    ) -> Result<AppServices, Box<dyn std::error::Error>> {
        let pending = core_startup::prepare_disconnected_startup(inputs.requested_core, None)?;
        Ok(AppServices {
            initializing: true,
            await_service_handoff: false,
            profile_store: Some(inputs.profile_store.clone()),
            override_store: Some(inputs.override_store.clone()),
            preferences_store: inputs.preferences_store.clone(),
            preferences: inputs.preferences.clone(),
            core_kind: inputs.requested_core,
            core_session: pending.session.clone(),
            traffic_capture: TrafficCaptureSession::new(
                pending.session,
                inputs.controlled_config_store.clone(),
                None,
                None,
            ),
            system_proxy_session: None,
            traffic_monitor: TrafficMonitor::start_with_client(runtime, pending.client.clone()),
            log_monitor: LogMonitor::start_with_client(
                runtime,
                pending.client.clone(),
                zenclash_core::MihomoLogLevel::Info,
            ),
            client: pending.client,
            traffic_history_store: None,
            traffic_history_session: None,
            profile_path: None,
            controlled_config_store: inputs.controlled_config_store.clone(),
            runtime: runtime.clone(),
            startup_notice: Some(zenclash_i18n::text("startup.initializing")),
            startup_error: None,
            restart_after_exit,
            safe_mode: inputs.safe_mode,
        })
    }

    /// Runs the full cancellable multi-stage startup state machine on a
    /// blocking worker and returns an explicit [`StartupOutcome`].
    #[must_use]
    pub fn prepare_application(
        runtime: &tokio::runtime::Runtime,
        inputs: BootstrapInputs,
        cancellation: StartupCancellation,
        cleanup: BootstrapCleanup,
        restart_after_exit: Arc<parking_lot::Mutex<Option<RestartRequest>>>,
    ) -> StartupOutcome {
        match Self::prepare_application_inner(
            runtime,
            inputs,
            &cancellation,
            &cleanup,
            restart_after_exit,
        ) {
            Ok(app) => {
                if let Some(error) = app.services.startup_error.clone() {
                    StartupOutcome::Offline(Box::new(app), error)
                } else {
                    StartupOutcome::Ready(Box::new(app))
                }
            }
            Err(error) if is_startup_cancelled(error.as_ref()) || cancellation.is_cancelled() => {
                StartupOutcome::Cancelled
            }
            Err(error) => StartupOutcome::Failed(error.to_string()),
        }
    }

    fn prepare_application_inner(
        runtime: &tokio::runtime::Runtime,
        inputs: BootstrapInputs,
        cancellation: &StartupCancellation,
        cleanup: &BootstrapCleanup,
        restart_after_exit: Arc<parking_lot::Mutex<Option<RestartRequest>>>,
    ) -> Result<BootstrappedApplication, Box<dyn std::error::Error>> {
        let BootstrapInputs {
            preferences_store,
            mut preferences,
            requested_core,
            environment_core,
            controlled_config_store,
            profile_store,
            override_store,
            traffic_history_store,
            mut recovery_notices,
            safe_mode,
        } = inputs;
        cancellation.ensure_active()?;
        let external = std::env::var_os("ZENCLASH_CONTROLLER").is_some();
        let managed_mihomo = !external && requested_core == CoreKind::Mihomo;
        let mut await_service_handoff = false;
        let mut startup_service_health = None;
        let prepared =
            (|| -> Result<core_startup::PreparedStartup, Box<dyn std::error::Error>> {
                let layers = prepare_configuration_layers(
                    managed_mihomo,
                    &profile_store,
                    &controlled_config_store,
                    &override_store,
                )
                .map_err(|error| {
                    tracing::warn!(%error, "startup configuration layers could not be confirmed");
                    if managed_mihomo {
                        Box::new(std::io::Error::other(blocked_message(
                            StartupBlocked::ConfigurationUnknown,
                        ))) as Box<dyn std::error::Error>
                    } else {
                        error
                    }
                })?;
                recovery_notices.extend(layers.notices);
                let configuration_unknown = layers.unknown;
                let override_paths = layers.overrides;
                let preferred_binary = preferences
                    .core_binaries
                    .path(requested_core)
                    .map(Path::to_path_buf);
                let resources =
                    if managed_mihomo {
                        let root = core_startup::project_root()?;
                        let selected = std::env::var_os("ZENCLASH_CONFIG")
                            .map(PathBuf::from)
                            .or(profile_store.active_path()?)
                            .or_else(|| {
                                let candidate =
                                    zenclash_core::WorkspaceResources::new(&root).default_profile();
                                candidate.is_file().then_some(candidate)
                            });
                        Some(runtime.block_on(cancellation.run_until_cancelled(
                            MihomoRuntimeResources::prepare(root, selected),
                        ))??)
                    } else {
                        None
                    };
                let mut health = if !safe_mode.allows_privileged_service() {
                    // Safe Mode must never probe or touch the privileged service.
                    managed_mihomo.then_some(ServiceHealthKind::NotInstalled)
                } else if let Some(resources) = resources.as_ref() {
                    Some(runtime.block_on(cancellation.run_until_cancelled(
                        zenclash_core::startup_service_health_for(
                            resources.home_dir().to_path_buf(),
                            preferred_binary.clone(),
                        ),
                    ))?)
                } else {
                    None
                };
                let mut tun_enabled = if !safe_mode.allows_tun() {
                    Some(false)
                } else if health.is_some() && !configuration_unknown {
                    resources.as_ref().and_then(|resources| {
                        controlled_config_store
                            .effective_json_with_overrides(resources.config_file(), &override_paths)
                            .ok()
                            .and_then(|value| effective_tun_enabled(&value))
                    })
                } else {
                    None
                };
                let elevated = zenclash_core::current_process_elevated();
                let explicit_local = !safe_mode.allows_privileged_service()
                    || std::env::args_os().any(|argument| argument == "--continue-local");
                await_service_handoff = cfg!(windows)
                    && safe_mode.allows_privileged_service()
                    && tun_enabled == Some(true)
                    && !explicit_local
                    && health != Some(ServiceHealthKind::NotInstalled);
                if cfg!(windows)
                    && safe_mode.allows_privileged_service()
                    && tun_enabled == Some(true)
                    && !elevated
                    && !explicit_local
                    && health != Some(ServiceHealthKind::NotInstalled)
                    && health != Some(ServiceHealthKind::Ready)
                {
                    // Bounded wait for an automatically starting service.
                    runtime.block_on(async {
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
                        while tokio::time::Instant::now() < deadline {
                            tokio::select! {
                                biased;
                                () = cancellation.cancelled() => break,
                                () = tokio::time::sleep(Duration::from_millis(200)) => {}
                            }
                            if cancellation.is_cancelled() {
                                break;
                            }
                            if let Some(resources) = &resources {
                                let Ok(observed) = cancellation
                                    .run_until_cancelled(zenclash_core::startup_service_health_for(
                                        resources.home_dir().to_path_buf(),
                                        preferred_binary.clone(),
                                    ))
                                    .await
                                else {
                                    break;
                                };
                                health = Some(observed);
                            }
                            if health == Some(ServiceHealthKind::Ready) {
                                break;
                            }
                        }
                    });
                }
                cancellation.ensure_active()?;
                if safe_mode.allows_tun()
                    && (health == Some(ServiceHealthKind::NotInstalled) || explicit_local)
                    && tun_enabled == Some(true)
                    && !elevated
                {
                    let resources = resources
                        .as_ref()
                        .ok_or_else(|| std::io::Error::other("missing runtime resources"))?;
                    let update = controlled_config_store.prepare_json_update(
                        resources.config_file(),
                        &serde_json::json!({"tun":{"enable":false}}),
                    )?;
                    controlled_config_store.commit(&update)?;
                    recovery_notices.push(zenclash_i18n::text(if explicit_local {
                        "startup.tun_disabled_local_choice"
                    } else {
                        "startup.tun_disabled_missing_service"
                    }));
                    tun_enabled = Some(false);
                }
                let local_store = if managed_mihomo && (!elevated || !safe_mode.allows_tun()) {
                    controlled_config_store.without_startup_tun()
                } else {
                    controlled_config_store.clone()
                };
                startup_service_health = health.clone();
                route_startup_with_policy(
                    requested_core,
                    external,
                    health.clone(),
                    tun_enabled,
                    explicit_local,
                    || {
                        cancellation.ensure_active()?;
                        core_startup::prepare_service_startup(
                            runtime,
                            resources.as_ref(),
                            preferred_binary.clone(),
                            &controlled_config_store,
                            &override_paths,
                            cancellation,
                        )
                    },
                    || {
                        cancellation.ensure_active()?;
                        if managed_mihomo && safe_mode.allows_privileged_service() {
                            runtime
                                .block_on(cancellation.run_until_cancelled(
                                    zenclash_core::check_sidecar_available(),
                                ))??;
                            if explicit_local || health != Some(ServiceHealthKind::NotInstalled) {
                                recovery_notices.push(zenclash_i18n::text(if explicit_local {
                                    "startup.local_choice"
                                } else {
                                    "startup.local_fallback"
                                }));
                                if tun_enabled == Some(true) && !elevated {
                                    recovery_notices
                                        .push(zenclash_i18n::text("startup.local_tun_disabled"));
                                }
                            }
                        }
                        let legacy = core_startup::LegacyStartupContext {
                            runtime,
                            preferences: &preferences,
                            requested_core,
                            preferred_binary: preferred_binary.as_deref(),
                            allow_core_recovery: environment_core.is_none(),
                            profile_store: Some(&profile_store),
                            controlled_config_store: &local_store,
                            override_paths: &override_paths,
                            cancellation,
                        };
                        core_startup::prepare_legacy_startup(&legacy)
                    },
                )
                .or_else(|failure| {
                    if matches!(
                        &failure,
                        StartupFailure::Operation(error) if is_startup_cancelled(error.as_ref())
                    ) || cancellation.is_cancelled()
                    {
                        return Err(failure);
                    }
                    // A lost/refused service Start must be checked afresh, never inferred idle.
                    if cfg!(windows)
                        && safe_mode.allows_privileged_service()
                        && health == Some(ServiceHealthKind::Ready)
                        && matches!(&failure, StartupFailure::Operation(_))
                        && matches!(
                            runtime.block_on(
                                cancellation
                                    .run_until_cancelled(zenclash_core::check_sidecar_available()),
                            ),
                            Ok(Ok(()))
                        )
                    {
                        if let StartupFailure::Operation(error) = &failure {
                            // Preserve the readable Start refusal, rather than publishing
                            // cached Ready health for the resulting Sidecar session.
                            startup_service_health =
                                Some(ServiceHealthKind::Unavailable(error.to_string()));
                        }
                        recovery_notices.push(zenclash_i18n::text("startup.local_fallback"));
                        cancellation
                            .ensure_active()
                            .map_err(|error| StartupFailure::Operation(error.into()))?;
                        let legacy = core_startup::LegacyStartupContext {
                            runtime,
                            preferences: &preferences,
                            requested_core,
                            preferred_binary: preferred_binary.as_deref(),
                            allow_core_recovery: environment_core.is_none(),
                            profile_store: Some(&profile_store),
                            controlled_config_store: &local_store,
                            override_paths: &override_paths,
                            cancellation,
                        };
                        return core_startup::prepare_legacy_startup(&legacy)
                            .map_err(StartupFailure::Operation);
                    }
                    Err(failure)
                })
                .map_err(|failure| match failure {
                    StartupFailure::Operation(error) => error,
                    StartupFailure::Blocked(reason) => {
                        Box::new(std::io::Error::other(blocked_message(reason)))
                            as Box<dyn std::error::Error>
                    }
                })
            })()
            .or_else(|error| {
                core_startup::recover_offline_startup(
                    managed_mihomo,
                    requested_core,
                    preferences
                        .core_binaries
                        .path(requested_core)
                        .map(Path::to_path_buf),
                    cancellation,
                    error,
                )
            })?;
        let core_startup::PreparedStartup {
            kind: core_kind,
            client,
            session: core_session,
            profile: profile_path,
            mut notice,
            error: startup_error,
            initialization,
        } = prepared;
        if let Some(health) = startup_service_health {
            core_session.record_startup_service_health(health);
        }
        *cleanup.lock() = Some(BootstrapOwner {
            core: core_session.clone(),
            capture: None,
            history: None,
        });
        cancellation.ensure_active()?;
        let controlled_config_store = controlled_config_store.with_runtime_tun_policy(&client);
        let system_proxy_session = if safe_mode.allows_system_proxy() {
            let proxy_controller = SystemProxyController::default();
            core_session.attach_pac_server(proxy_controller.pac_server());
            preferences_store
                .clone()
                .map(|store| SystemProxySession::new(store, proxy_controller))
        } else {
            None
        };
        let traffic_capture = TrafficCaptureSession::new(
            core_session.clone(),
            controlled_config_store.clone(),
            system_proxy_session.clone(),
            profile_path.clone(),
        );
        core_startup::remember_working_core(
            preferences_store.as_ref(),
            &mut preferences,
            core_kind,
            core_session.runtime_descriptor().binary(),
        );
        for recovery_notice in recovery_notices {
            append_startup_notice(&mut notice, recovery_notice);
        }
        let startup_notice = notice;
        let traffic = TrafficMonitor::start_with_client(runtime.handle(), client.clone());
        let logs = LogMonitor::start_with_client(
            runtime.handle(),
            client.clone(),
            zenclash_core::MihomoLogLevel::Info,
        );
        let runtime_handle = runtime.handle().clone();
        let traffic_history_session = traffic_history_store.clone().map(|store| {
            TrafficHistorySession::start(&runtime_handle, client.clone(), store, &preferences)
        });
        *cleanup.lock() = Some(BootstrapOwner {
            core: core_session.clone(),
            capture: Some(traffic_capture.clone()),
            history: traffic_history_session.clone(),
        });
        Ok(BootstrappedApplication {
            services: AppServices {
                initializing: false,
                await_service_handoff,
                profile_store: Some(profile_store),
                override_store: Some(override_store),
                preferences_store,
                preferences,
                core_kind,
                core_session,
                traffic_capture,
                system_proxy_session,
                client,
                traffic_monitor: traffic,
                log_monitor: logs,
                traffic_history_store,
                traffic_history_session,
                profile_path,
                controlled_config_store,
                runtime: runtime_handle,
                startup_notice,
                startup_error,
                restart_after_exit,
                safe_mode,
            },
            initialization,
        })
    }
}

/// Appends a localized startup notice separated by `startup.separator`.
pub fn append_startup_notice(target: &mut Option<String>, notice: String) {
    if let Some(current) = target {
        current.push_str(&zenclash_i18n::text("startup.separator"));
        current.push_str(&notice);
    } else {
        *target = Some(notice);
    }
}

/// Loads application preferences, quarantining corrupted JSON files with a
/// user-facing recovery notice.
///
/// # Errors
///
/// Returns an [`AppPreferencesError`] if reading or quarantining preferences fails.
pub fn load_preferences(
    store: Option<&AppPreferencesStore>,
) -> Result<(AppPreferences, Option<String>), AppPreferencesError> {
    let Some(store) = store else {
        return Ok((AppPreferences::default(), None));
    };
    if let Some(path) = store.quarantine_invalid_preferences()? {
        return Ok((
            AppPreferences::default(),
            Some(zenclash_i18n::text_with(
                "startup.quarantine.preferences",
                &[("path", path.display().to_string())],
            )),
        ));
    }
    Ok((store.load()?, None))
}

/// Spawns a restarted `ZenClash` process after the current instance releases
/// its lock and shuts down cleanly.
///
/// # Errors
///
/// Returns an I/O error if spawning the executable fails.
pub fn spawn_restarted_process(request: &RestartRequest) -> std::io::Result<()> {
    let mut command = Command::new(&request.executable);
    if request.continue_local {
        command.arg("--continue-local");
    }
    command.spawn()?;
    Ok(())
}
