#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! Native `ZenClash` executable bootstrap and managed Mihomo discovery.

use std::{
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tracing_subscriber::{EnvFilter, filter::Directive};
use zenclash_core::{
    AppInstanceLock, AppPreferences, AppPreferencesStore, ControlledConfigStore,
    CoreInitializationOutcome, CoreKind, CoreSession, EffectiveConfigIntent, LogMonitor,
    MihomoClient, MihomoEndpoint, MihomoLaunchConfig, MihomoProcess, MihomoRuntimeResources,
    ProfileStore, ServiceHealthKind, TrafficHistoryStore, TrafficMonitor, YamlOverrideStore,
    bundled_recovery_profile,
};
use zenclash_ui::{app, assets::Assets};

mod startup;

const DEFAULT_TRACING_FILTER: &str = "zenclash=info,zenclash_core=info,zenclash_ui=info";
const MANAGED_CONTROLLER_ATTEMPTS: usize = 3;
const QUIET_NETWORK_TARGETS: [&str; 6] = [
    "tokio_tungstenite=warn",
    "tungstenite=warn",
    "reqwest=warn",
    "hyper=warn",
    "h2=warn",
    "rustls=warn",
];

struct RecoveredCore {
    kind: CoreKind,
    binary: Option<PathBuf>,
    endpoint: MihomoEndpoint,
    process: Option<Arc<MihomoProcess>>,
    profile: Option<PathBuf>,
    startup_notice: Option<String>,
}

struct BootstrappedCore {
    endpoint: MihomoEndpoint,
    process: Option<Arc<MihomoProcess>>,
    profile: Option<PathBuf>,
    startup_notice: Option<String>,
}

struct CoreStartupState {
    kind: CoreKind,
    endpoint: MihomoEndpoint,
    process: Option<Arc<MihomoProcess>>,
    profile: Option<PathBuf>,
    notice: Option<String>,
    error: Option<String>,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_filter(std::env::var("RUST_LOG").ok().as_deref()))
        .init();

    if let Err(error) = run() {
        tracing::error!(%error, "ZenClash startup failed");
        eprintln!(
            "{}",
            zenclash_i18n::text_with("startup.failure", &[("error", error.to_string())],)
        );
    }
}

fn tracing_filter(requested: Option<&str>) -> EnvFilter {
    let mut filter = requested
        .and_then(|value| EnvFilter::try_new(value).ok())
        .unwrap_or_else(|| EnvFilter::new(DEFAULT_TRACING_FILTER));
    for value in QUIET_NETWORK_TARGETS {
        if let Ok(directive) = value.parse::<Directive>() {
            // Protocol frame dumps can recursively enter a controller's `/logs`
            // WebSocket. Keep those targets quiet even when application debug
            // logging is requested through `RUST_LOG`.
            filter = filter.add_directive(directive);
        }
    }
    filter
}

fn append_startup_notice(target: &mut Option<String>, notice: String) {
    if let Some(current) = target {
        current.push_str(&zenclash_i18n::text("startup.separator"));
        current.push_str(&notice);
    } else {
        *target = Some(notice);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("zenclash-io")
            .build()?,
    );
    let _runtime_guard = runtime.enter();
    runtime.block_on(zenclash_core::configure_service_ipc());
    let preferences_store = match AppPreferencesStore::discover() {
        Ok(store) => Some(store),
        Err(error) => {
            tracing::warn!(%error, "failed to discover preferences; using defaults");
            None
        }
    };
    let _instance_lock = if let Some(store) = preferences_store.as_ref() {
        Some(AppInstanceLock::acquire(
            store.path().with_file_name("instance.lock"),
        )?)
    } else {
        tracing::warn!("application data directory unavailable; instance locking is disabled");
        None
    };
    let (preferences, preferences_recovery_notice) = load_preferences(preferences_store.as_ref())?;
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
    let runtime_handle = runtime.handle().clone();
    let restart_after_exit = Arc::new(parking_lot::Mutex::new(None));
    let cancelled = Arc::new(AtomicBool::new(false));
    let cleanup: StartupCleanup = Arc::new(parking_lot::Mutex::new(None));
    let inputs = StartupInputs {
        preferences_store,
        preferences,
        requested_core,
        environment_core,
        controlled_config_store,
        profile_store,
        override_store,
        recovery_notices,
    };
    let pending_services =
        prepare_pending_services(&inputs, &runtime_handle, restart_after_exit.clone())?;
    let (finished_sender, finished_receiver) = tokio::sync::oneshot::channel();
    let worker_runtime = runtime.clone();
    let worker_cancelled = cancelled.clone();
    let worker_cleanup = cleanup.clone();
    let worker_restart = restart_after_exit.clone();
    let startup_task = runtime.spawn_blocking(move || {
        let result = prepare_application(
            &worker_runtime,
            inputs,
            worker_cancelled,
            worker_cleanup,
            worker_restart,
        )
        .map_err(|error| error.to_string());
        let _ = finished_sender.send(());
        result
    });

    gpui_kit::application().with_assets(Assets).run(move |cx| {
        app::init(cx);
        app::create_main_window_with_pending_startup(pending_services, startup_task, cx);
        cx.activate(true);
    });
    // GPUI's native quit observers have a 200 ms deadline. Finish on Tokio after
    // the event loop returns, before dropping the runtime or allowing restart.
    cancelled.store(true, Ordering::Release);
    let shutdown_result = runtime.block_on(async {
        let _ = finished_receiver.await;
        let completed = cleanup.lock().take();
        let mut failures = Vec::new();
        if let Some(history) = completed.as_ref().and_then(|owner| owner.history.as_ref())
            && let Err(error) = history.shutdown().await
        {
            failures.push(error);
        }
        // The native event loop has already exited: always stop the owned child,
        // even when history persistence failed. Any failure prevents restart.
        if let Some(owner) = completed
            && let Err(error) = owner.core.shutdown().await
        {
            failures.push(error.to_string());
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    });
    shutdown_result.map_err(std::io::Error::other)?;
    drop(_instance_lock);
    if let Some(request) = restart_after_exit.lock().take() {
        spawn_restarted_process(&request).map_err(|error| {
            std::io::Error::other(zenclash_i18n::text_with(
                "startup.restart_failed",
                &[
                    ("path", request.executable.display().to_string()),
                    ("error", error.to_string()),
                ],
            ))
        })?;
    }
    Ok(())
}

struct StartupOwner {
    core: CoreSession,
    history: Option<Arc<app::TrafficHistorySession>>,
}
type StartupCleanup = Arc<parking_lot::Mutex<Option<StartupOwner>>>;
struct StartupInputs {
    preferences_store: Option<AppPreferencesStore>,
    preferences: AppPreferences,
    requested_core: CoreKind,
    environment_core: Option<CoreKind>,
    controlled_config_store: ControlledConfigStore,
    profile_store: ProfileStore,
    override_store: YamlOverrideStore,
    recovery_notices: Vec<String>,
}

fn prepare_pending_services(
    inputs: &StartupInputs,
    runtime: &tokio::runtime::Handle,
    restart_after_exit: Arc<parking_lot::Mutex<Option<app::RestartRequest>>>,
) -> Result<app::AppServices, Box<dyn std::error::Error>> {
    let pending = prepare_disconnected_startup(inputs.requested_core, None)?;
    Ok(app::AppServices {
        initializing: true,
        await_service_handoff: false,
        profile_store: Some(inputs.profile_store.clone()),
        override_store: Some(inputs.override_store.clone()),
        preferences_store: inputs.preferences_store.clone(),
        preferences: inputs.preferences.clone(),
        core_kind: inputs.requested_core,
        core_session: pending.session,
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
    })
}

fn ensure_startup_active(cancelled: &AtomicBool) -> Result<(), Box<dyn std::error::Error>> {
    if cancelled.load(Ordering::Acquire) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "application closed during startup",
        )
        .into());
    }
    Ok(())
}
fn prepare_application(
    runtime: &tokio::runtime::Runtime,
    inputs: StartupInputs,
    cancelled: Arc<AtomicBool>,
    cleanup: StartupCleanup,
    restart_after_exit: Arc<parking_lot::Mutex<Option<app::RestartRequest>>>,
) -> Result<app::BootstrappedApplication, Box<dyn std::error::Error>> {
    let StartupInputs {
        preferences_store,
        mut preferences,
        requested_core,
        environment_core,
        controlled_config_store,
        profile_store,
        override_store,
        mut recovery_notices,
    } = inputs;
    ensure_startup_active(&cancelled)?;
    let external = std::env::var_os("ZENCLASH_CONTROLLER").is_some();
    let managed_mihomo = !external && requested_core == CoreKind::Mihomo;
    let mut await_service_handoff = false;
    let mut startup_service_health = None;
    let prepared = (|| -> Result<PreparedStartup, Box<dyn std::error::Error>> {
        let layers = startup::prepare_configuration_layers(
            managed_mihomo,
            &profile_store,
            &controlled_config_store,
            &override_store,
        )
        .map_err(|error| {
            tracing::warn!(%error, "startup configuration layers could not be confirmed");
            if managed_mihomo {
                Box::new(std::io::Error::other(startup::blocked_message(
                    startup::StartupBlocked::ConfigurationUnknown,
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
        let resources = if managed_mihomo {
            let root = project_root()?;
            let selected = std::env::var_os("ZENCLASH_CONFIG")
                .map(PathBuf::from)
                .or(profile_store.active_path()?)
                .or_else(|| {
                    let candidate = root.join("platforms/common/default.yaml");
                    candidate.is_file().then_some(candidate)
                });
            Some(runtime.block_on(MihomoRuntimeResources::prepare(root, selected))?)
        } else {
            None
        };
        let mut health = resources.as_ref().map(|resources| {
            runtime.block_on(zenclash_core::startup_service_health_for(
                resources.home_dir().to_path_buf(),
                preferred_binary.clone(),
            ))
        });
        let mut tun_enabled = if health.is_some() && !configuration_unknown {
            resources.as_ref().and_then(|resources| {
                controlled_config_store
                    .effective_json_with_overrides(resources.config_file(), &override_paths)
                    .ok()
                    .and_then(|value| startup::effective_tun_enabled(&value))
            })
        } else {
            None
        };
        let elevated = zenclash_core::current_process_elevated();
        let explicit_local = std::env::args_os().any(|argument| argument == "--continue-local");
        await_service_handoff = cfg!(windows)
            && tun_enabled == Some(true)
            && !explicit_local
            && health != Some(ServiceHealthKind::NotInstalled);
        if cfg!(windows)
            && tun_enabled == Some(true)
            && !elevated
            && !explicit_local
            && health != Some(ServiceHealthKind::NotInstalled)
        {
            // Match Verge's bounded wait for an automatically starting service.
            if health != Some(ServiceHealthKind::Ready) {
                runtime.block_on(async {
                    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
                    while tokio::time::Instant::now() < deadline {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        if cancelled.load(Ordering::Acquire) {
                            break;
                        }
                        if let Some(resources) = &resources {
                            health = Some(
                                zenclash_core::startup_service_health_for(
                                    resources.home_dir().to_path_buf(),
                                    preferred_binary.clone(),
                                )
                                .await,
                            );
                        }
                        if health == Some(ServiceHealthKind::Ready) {
                            break;
                        }
                    }
                });
            }
        }
        ensure_startup_active(&cancelled)?;
        if (health == Some(ServiceHealthKind::NotInstalled) || explicit_local)
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
        let local_store = if managed_mihomo && !elevated {
            controlled_config_store.without_startup_tun()
        } else {
            controlled_config_store.clone()
        };
        startup_service_health = health.clone();
        startup::route_startup_with_policy(
            requested_core,
            external,
            health.clone(),
            tun_enabled,
            explicit_local,
            || {
                ensure_startup_active(&cancelled)?;
                prepare_service_startup(
                    runtime,
                    resources.as_ref(),
                    preferred_binary.clone(),
                    &controlled_config_store,
                    &override_paths,
                )
            },
            || {
                ensure_startup_active(&cancelled)?;
                if managed_mihomo {
                    runtime.block_on(zenclash_core::check_sidecar_available())?;
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
                prepare_legacy_startup(
                    runtime,
                    &mut preferences,
                    requested_core,
                    preferred_binary.as_deref(),
                    environment_core.is_none(),
                    &local_store,
                    &override_paths,
                )
            },
        )
        .or_else(|failure| {
            // A lost/refused service Start must be checked afresh, never inferred idle.
            if cfg!(windows)
                && health == Some(ServiceHealthKind::Ready)
                && matches!(&failure, startup::StartupFailure::Operation(_))
                && runtime
                    .block_on(zenclash_core::check_sidecar_available())
                    .is_ok()
            {
                if let startup::StartupFailure::Operation(error) = &failure {
                    // Preserve the readable Start refusal, rather than publishing
                    // cached Ready health for the resulting Sidecar session.
                    startup_service_health =
                        Some(ServiceHealthKind::Unavailable(error.to_string()));
                }
                recovery_notices.push(zenclash_i18n::text("startup.local_fallback"));
                ensure_startup_active(&cancelled).map_err(startup::StartupFailure::Operation)?;
                return prepare_legacy_startup(
                    runtime,
                    &mut preferences,
                    requested_core,
                    preferred_binary.as_deref(),
                    environment_core.is_none(),
                    &local_store,
                    &override_paths,
                )
                .map_err(startup::StartupFailure::Operation);
            }
            Err(failure)
        })
        .map_err(|failure| match failure {
            startup::StartupFailure::Operation(error) => error,
            startup::StartupFailure::Blocked(reason) => {
                Box::new(std::io::Error::other(startup::blocked_message(reason)))
                    as Box<dyn std::error::Error>
            }
        })
    })()
    .or_else(|error| {
        if managed_mihomo {
            prepare_offline_startup_with_binary(
                requested_core,
                error.to_string(),
                preferences
                    .core_binaries
                    .path(requested_core)
                    .map(Path::to_path_buf),
            )
        } else {
            prepare_disconnected_startup(requested_core, Some(error.to_string()))
        }
    })?;
    let PreparedStartup {
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
    *cleanup.lock() = Some(StartupOwner {
        core: core_session.clone(),
        history: None,
    });
    let controlled_config_store = controlled_config_store.with_runtime_tun_policy(&client);
    remember_working_core(
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
    let traffic_history_store = TrafficHistoryStore::discover()
        .inspect_err(|error| tracing::warn!(%error, "failed to discover traffic-history database"))
        .ok();
    let runtime_handle = runtime.handle().clone();
    let traffic_history_session = traffic_history_store.clone().map(|store| {
        app::TrafficHistorySession::start(&runtime_handle, client.clone(), store, &preferences)
    });
    *cleanup.lock() = Some(StartupOwner {
        core: core_session.clone(),
        history: traffic_history_session.clone(),
    });
    Ok(app::BootstrappedApplication {
        services: app::AppServices {
            initializing: false,
            await_service_handoff,
            profile_store: Some(profile_store),
            override_store: Some(override_store),
            preferences_store,
            preferences,
            core_kind,
            core_session,
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
        },
        initialization,
    })
}

struct PreparedStartup {
    kind: CoreKind,
    client: MihomoClient,
    session: CoreSession,
    profile: Option<PathBuf>,
    notice: Option<String>,
    error: Option<String>,
    initialization: Option<CoreInitializationOutcome>,
}

#[cfg(test)]
fn prepare_offline_startup(
    kind: CoreKind,
    error: String,
) -> Result<PreparedStartup, Box<dyn std::error::Error>> {
    prepare_offline_startup_with_binary(kind, error, None)
}

fn prepare_offline_startup_with_binary(
    kind: CoreKind,
    error: String,
    preferred_binary: Option<PathBuf>,
) -> Result<PreparedStartup, Box<dyn std::error::Error>> {
    // A blocked service must not start a competing kernel, but the user still
    // needs the window to inspect service health and request maintenance.
    tracing::warn!(%error, "core startup blocked; opening UI without a controller");
    let current = std::env::current_dir()?;
    let home = MihomoRuntimeResources::recovery_home(&project_root()?);
    let home = if home.is_absolute() {
        home
    } else {
        current.join(home)
    };
    let binary = preferred_binary.map(|path| {
        if path.is_absolute() {
            path
        } else {
            current.join(path)
        }
    });
    let session = CoreSession::open_offline(kind, home, binary)?;
    Ok(PreparedStartup {
        kind,
        client: session.client().clone(),
        session,
        profile: None,
        notice: None,
        error: Some(error),
        initialization: None,
    })
}

fn prepare_disconnected_startup(
    kind: CoreKind,
    error: Option<String>,
) -> Result<PreparedStartup, Box<dyn std::error::Error>> {
    let client = MihomoClient::new(MihomoEndpoint::new("127.0.0.1:0", "zenclash-offline"))?
        .with_core_kind(kind)?;
    let session = CoreSession::open(kind, client.clone())?;
    Ok(PreparedStartup {
        kind,
        client,
        session,
        profile: None,
        notice: None,
        error,
        initialization: None,
    })
}

fn prepare_legacy_startup(
    runtime: &tokio::runtime::Runtime,
    preferences: &mut AppPreferences,
    requested_core: CoreKind,
    preferred_binary: Option<&Path>,
    allow_core_recovery: bool,
    controlled_config_store: &ControlledConfigStore,
    override_paths: &[PathBuf],
) -> Result<PreparedStartup, Box<dyn std::error::Error>> {
    let initial = bootstrap_core(
        runtime,
        requested_core,
        preferred_binary,
        controlled_config_store,
        override_paths,
        None,
        true,
    );
    let startup = match initial {
        Ok(bootstrapped) => CoreStartupState {
            kind: requested_core,
            endpoint: bootstrapped.endpoint,
            process: bootstrapped.process,
            profile: bootstrapped.profile,
            notice: bootstrapped.startup_notice,
            error: None,
        },
        Err(initial_error) if allow_core_recovery => {
            match recover_core(
                runtime,
                preferences,
                requested_core,
                preferred_binary,
                controlled_config_store,
                override_paths,
                &initial_error,
            ) {
                Ok(recovered) => {
                    let source = recovered.binary.as_ref().map_or_else(
                        || zenclash_i18n::text("startup.automatic_discovery"),
                        |path| path.display().to_string(),
                    );
                    let mut notice = zenclash_i18n::text_with(
                        "startup.core_recovered",
                        &[
                            ("requested", requested_core.to_string()),
                            ("recovered", recovered.kind.to_string()),
                            ("source", source),
                            ("error", initial_error.to_string()),
                        ],
                    );
                    if let Some(listener_notice) = recovered.startup_notice {
                        notice.push_str(&zenclash_i18n::text("startup.separator"));
                        notice.push_str(&listener_notice);
                    }
                    tracing::warn!(requested = %requested_core, fallback = %recovered.kind, %initial_error, "recovered with last usable core");
                    CoreStartupState {
                        kind: recovered.kind,
                        endpoint: recovered.endpoint,
                        process: recovered.process,
                        profile: recovered.profile,
                        notice: Some(notice),
                        error: None,
                    }
                }
                Err(recovery_error) => recover_safe_profile(
                    runtime,
                    preferences,
                    requested_core,
                    preferred_binary,
                    controlled_config_store,
                    true,
                    &recovery_error,
                )
                .unwrap_or_else(|error| offline_core_state(requested_core, &error)),
            }
        }
        Err(error) => {
            if std::env::var_os("ZENCLASH_CONFIG").is_none() {
                recover_safe_profile(
                    runtime,
                    preferences,
                    requested_core,
                    preferred_binary,
                    controlled_config_store,
                    false,
                    &error,
                )
                .unwrap_or_else(|recovery| offline_core_state(requested_core, &recovery))
            } else {
                offline_core_state(requested_core, &error)
            }
        }
    };
    let CoreStartupState {
        kind: core_kind,
        endpoint,
        process: mihomo_process,
        profile: profile_path,
        notice: startup_notice,
        error: startup_error,
    } = startup;
    let client = match &mihomo_process {
        Some(process) => MihomoClient::from_process(process.clone())?,
        None => MihomoClient::new(endpoint)?.with_core_kind(core_kind)?,
    };
    let core_session = CoreSession::open_with_config(
        core_kind,
        client.clone(),
        profile_path.clone(),
        override_paths.to_vec(),
    )?;
    if startup_error.is_none()
        && mihomo_process.is_none()
        && core_kind.capabilities().full_config_reload
    {
        if let Some(profile) = profile_path.as_ref()
            && let Err(error) = runtime.block_on(core_session.apply(
                controlled_config_store,
                EffectiveConfigIntent::ActivateProfile {
                    profile: profile.clone(),
                    overrides: override_paths.to_vec(),
                },
            ))
        {
            tracing::warn!(%error, core = %core_kind, "initial core configuration synchronization failed");
        }
    } else if !core_kind.capabilities().full_config_reload {
        tracing::info!(core = %core_kind, "skipping unsupported full configuration hot reload");
    }
    // The shared binding now owns this bootstrap child.
    drop(mihomo_process);
    Ok(PreparedStartup {
        kind: core_kind,
        client,
        session: core_session,
        profile: profile_path,
        notice: startup_notice,
        error: startup_error,
        initialization: None,
    })
}

fn prepare_service_startup(
    runtime: &tokio::runtime::Runtime,
    resources: Option<&MihomoRuntimeResources>,
    core_source: Option<PathBuf>,
    store: &ControlledConfigStore,
    overrides: &[PathBuf],
) -> Result<PreparedStartup, Box<dyn std::error::Error>> {
    let resources = resources
        .ok_or_else(|| std::io::Error::other(zenclash_i18n::text("core_page.service.unknown")))?;
    let client = runtime
        .block_on(MihomoClient::connect_service_with_core(
            resources.home_dir().to_path_buf(),
            core_source,
        ))
        .map_err(|error| std::io::Error::other(startup::connection_message(&error)))?;
    let session = CoreSession::open(CoreKind::Mihomo, client.clone())?;
    let result = runtime.block_on(session.initialize_service_runtime(
        store,
        resources.config_file().to_path_buf(),
        overrides.to_vec(),
    ));
    let (initialization, error) = match result {
        Ok(outcome) => {
            // A saved Commit awaiting acknowledgement has a shared, dynamic
            // confirmation token; it must not become an immutable startup error.
            let error = outcome
                .failure()
                .filter(|_| !outcome.commit_pending())
                .map(ToString::to_string);
            (Some(outcome), error)
        }
        Err(error) => (None, Some(error.to_string())),
    };
    if cfg!(windows)
        && error.is_some()
        && initialization
            .as_ref()
            .is_none_or(|outcome| outcome.saved().is_none())
    {
        // Release the uncertain service owner before the caller's fresh idle
        // check. A failed Stop/Release keeps this session reachable by the GUI.
        if runtime.block_on(session.shutdown()).is_ok()
            && runtime
                .block_on(zenclash_core::check_sidecar_available())
                .is_ok()
        {
            return Err(std::io::Error::other(error.as_deref().unwrap_or_default()).into());
        }
    }
    let notice = initialization.as_ref().and_then(|outcome| {
        let changes = outcome
            .listener_fallbacks()
            .iter()
            .map(|fallback| {
                format!(
                    "{} {}→{}",
                    fallback.listener, fallback.original, fallback.current
                )
            })
            .collect::<Vec<_>>()
            .join("、");
        (!changes.is_empty())
            .then(|| zenclash_i18n::text_with("startup.listener_fallback", &[("changes", changes)]))
    });
    let profile = session.committed_profile_snapshot().profile_path;
    // Even an uncertain Start is owned by this session and reaches both quit paths.
    Ok(PreparedStartup {
        kind: CoreKind::Mihomo,
        client,
        session,
        profile,
        notice,
        error,
        initialization,
    })
}
fn spawn_restarted_process(request: &app::RestartRequest) -> std::io::Result<()> {
    let mut command = Command::new(&request.executable);
    if request.continue_local {
        command.arg("--continue-local");
    }
    command.spawn()?;
    Ok(())
}

fn load_preferences(
    store: Option<&AppPreferencesStore>,
) -> Result<(AppPreferences, Option<String>), zenclash_core::AppPreferencesError> {
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

fn bootstrap_core(
    runtime: &tokio::runtime::Runtime,
    core_kind: CoreKind,
    preferred_binary: Option<&std::path::Path>,
    controlled_config_store: &ControlledConfigStore,
    override_paths: &[PathBuf],
    profile_override: Option<&Path>,
    apply_persisted_layers: bool,
) -> std::io::Result<BootstrappedCore> {
    let project_root = project_root()?;
    let selected_profile = profile_override
        .map(Path::to_path_buf)
        .or_else(|| selected_profile(&project_root));

    if std::env::var_os("ZENCLASH_CONTROLLER").is_some() {
        let profile_path = selected_profile.or_else(|| {
            let candidate = project_root.join("platforms/common/default.yaml");
            candidate.is_file().then_some(candidate)
        });
        return Ok(BootstrappedCore {
            endpoint: MihomoEndpoint::from_env(),
            process: None,
            profile: profile_path,
            startup_notice: None,
        });
    }

    let discovered = match MihomoLaunchConfig::discover_for_kind_with_binary_and_config(
        &project_root,
        core_kind,
        preferred_binary,
        selected_profile.as_deref(),
    ) {
        Ok(launch) => launch,
        Err(error) => {
            return Err(std::io::Error::other(zenclash_i18n::text_with(
                "startup.explicit_core",
                &[
                    ("core", core_kind.to_string()),
                    ("error", error.to_string()),
                ],
            )));
        }
    };
    let profile_path = selected_profile.unwrap_or_else(|| discovered.config_file.clone());
    zenclash_core::verify_ordinary_local_executable(&discovered.binary)
        .map_err(std::io::Error::other)?;
    let (effective_path, listener_fallbacks) = if apply_persisted_layers {
        let effective_path = controlled_config_store
            .materialize_with_overrides_for_core(&profile_path, override_paths, core_kind)
            .map_err(std::io::Error::other)?;
        let listener_fallbacks = controlled_config_store
            .resolve_startup_listener_conflicts()
            .map_err(std::io::Error::other)?;
        (effective_path, listener_fallbacks)
    } else {
        (profile_path.clone(), Vec::new())
    };
    let listener_notice = (!listener_fallbacks.is_empty()).then(|| {
        let changes = listener_fallbacks
            .iter()
            .map(|fallback| {
                tracing::warn!(
                    listener = %fallback.listener,
                    original = fallback.original,
                    current = fallback.current,
                    "proxy listener was moved for this managed-core session"
                );
                format!(
                    "{} {}→{}",
                    fallback.listener, fallback.original, fallback.current
                )
            })
            .collect::<Vec<_>>()
            .join("、");
        zenclash_i18n::text_with("startup.listener_fallback", &[("changes", changes)])
    });
    let launch = match MihomoLaunchConfig::for_kind(
        core_kind,
        discovered.binary,
        effective_path,
        discovered.home_dir,
    ) {
        Ok(launch) => launch,
        Err(error) => {
            return Err(std::io::Error::other(zenclash_i18n::text_with(
                "startup.active_invalid",
                &[
                    ("core", core_kind.to_string()),
                    ("error", error.to_string()),
                ],
            )));
        }
    };
    launch.validate_config().map_err(|error| {
        std::io::Error::other(zenclash_i18n::text_with(
            "startup.active_precheck",
            &[
                ("core", core_kind.to_string()),
                ("error", error.to_string()),
            ],
        ))
    })?;
    for attempt in 1..=MANAGED_CONTROLLER_ATTEMPTS {
        let controller = (if core_kind == CoreKind::Mihomo {
            MihomoEndpoint::local_ipc(&launch.home_dir).map_err(std::io::Error::other)
        } else {
            allocate_managed_controller()
        })
        .map_err(|error| {
            std::io::Error::other(zenclash_i18n::text_with(
                "startup.controller_allocation",
                &[
                    ("core", core_kind.to_string()),
                    ("error", error.to_string()),
                ],
            ))
        })?;
        let launch = launch.clone().with_controller_endpoint(controller);
        let endpoint = launch.endpoint.clone();
        let process = MihomoProcess::spawn(launch).map_err(|error| {
            std::io::Error::other(zenclash_i18n::text_with(
                "startup.managed_spawn",
                &[
                    ("core", core_kind.to_string()),
                    ("error", error.to_string()),
                ],
            ))
        })?;
        match runtime.block_on(process.wait_until_ready(Duration::from_secs(20))) {
            Ok(()) => {
                tracing::info!(core = %core_kind, controller = %endpoint.controller, attempt, "managed core is ready");
                return Ok(BootstrappedCore {
                    endpoint,
                    process: Some(process),
                    profile: Some(profile_path),
                    startup_notice: listener_notice,
                });
            }
            Err(error) => {
                let controller_conflict = is_controller_listener_conflict(&error.to_string());
                tracing::error!(%error, core = %core_kind, attempt, "managed core failed to become ready");
                for line in process.snapshot().logs.iter().rev().take(12).rev() {
                    tracing::error!("{line}");
                }
                if let Err(stop_error) = process.stop() {
                    return Err(std::io::Error::other(zenclash_i18n::text_with(
                        "startup.managed_not_ready_stop",
                        &[
                            ("core", core_kind.to_string()),
                            ("error", error.to_string()),
                            ("stop_error", stop_error.to_string()),
                        ],
                    )));
                }
                if controller_conflict && attempt < MANAGED_CONTROLLER_ATTEMPTS {
                    tracing::warn!(core = %core_kind, attempt, "managed controller port was taken before core bind; retrying");
                    continue;
                }
                return Err(std::io::Error::other(zenclash_i18n::text_with(
                    "startup.managed_not_ready",
                    &[
                        ("core", core_kind.to_string()),
                        ("error", error.to_string()),
                    ],
                )));
            }
        }
    }
    Err(std::io::Error::other(zenclash_i18n::text_with(
        "startup.controller_contended",
        &[
            ("core", core_kind.to_string()),
            ("count", MANAGED_CONTROLLER_ATTEMPTS.to_string()),
        ],
    )))
}

fn is_controller_listener_conflict(error: &str) -> bool {
    let normalized = error.to_ascii_lowercase();
    normalized.contains("external controller listen error")
        && normalized.contains("address already in use")
}

fn project_root() -> std::io::Result<PathBuf> {
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .ok_or_else(|| std::io::Error::other(zenclash_i18n::text("startup.workspace")))?
        .to_path_buf())
}

fn selected_profile(project_root: &Path) -> Option<PathBuf> {
    std::env::var_os("ZENCLASH_CONFIG")
        .map(PathBuf::from)
        .or_else(|| {
            ProfileStore::discover()
                .and_then(|store| store.active_path())
                .inspect_err(|error| tracing::warn!(%error, "failed to load active profile"))
                .ok()
                .flatten()
        })
        .or_else(|| {
            let candidate = project_root.join("platforms/common/default.yaml");
            candidate.is_file().then_some(candidate)
        })
}

fn offline_core_state(requested_core: CoreKind, error: &std::io::Error) -> CoreStartupState {
    tracing::error!(%error, core = %requested_core, "all eligible cores failed; opening recovery UI without a controller");
    let profile = project_root()
        .ok()
        .and_then(|project_root| selected_profile(&project_root));
    CoreStartupState {
        kind: requested_core,
        endpoint: MihomoEndpoint::new("127.0.0.1:0", "zenclash-offline"),
        process: None,
        profile,
        notice: None,
        error: Some(zenclash_i18n::text_with(
            "startup.offline",
            &[("error", error.to_string())],
        )),
    }
}

fn recover_core(
    runtime: &tokio::runtime::Runtime,
    preferences: &AppPreferences,
    requested_core: CoreKind,
    requested_binary: Option<&std::path::Path>,
    controlled_config_store: &ControlledConfigStore,
    override_paths: &[PathBuf],
    initial_error: &std::io::Error,
) -> std::io::Result<RecoveredCore> {
    let mut candidates = Vec::new();
    if let Some(kind) = preferences.last_known_good_core {
        let binary = preferences.last_known_good_binary.clone();
        if kind != requested_core || binary.as_deref() != requested_binary {
            candidates.push((kind, binary));
        }
    }
    if requested_core != CoreKind::Mihomo || requested_binary.is_some() {
        candidates.push((CoreKind::Mihomo, None));
    }
    if requested_core != CoreKind::Meow || requested_binary.is_some() {
        candidates.push((CoreKind::Meow, None));
    }

    let mut failures = vec![initial_error.to_string()];
    for (kind, binary) in candidates {
        match bootstrap_core(
            runtime,
            kind,
            binary.as_deref(),
            controlled_config_store,
            override_paths,
            None,
            true,
        ) {
            Ok(bootstrapped) => {
                let actual_binary = bootstrapped
                    .process
                    .as_ref()
                    .map(|process| process.snapshot().binary)
                    .or(binary);
                return Ok(RecoveredCore {
                    kind,
                    binary: actual_binary,
                    endpoint: bootstrapped.endpoint,
                    process: bootstrapped.process,
                    profile: bootstrapped.profile,
                    startup_notice: bootstrapped.startup_notice,
                });
            }
            Err(error) => failures.push(error.to_string()),
        }
    }
    Err(std::io::Error::other(zenclash_i18n::text_with(
        "startup.all_cores_failed",
        &[(
            "errors",
            failures.join(&zenclash_i18n::text("startup.separator")),
        )],
    )))
}

fn recover_safe_profile(
    runtime: &tokio::runtime::Runtime,
    preferences: &AppPreferences,
    requested_core: CoreKind,
    requested_binary: Option<&Path>,
    controlled_config_store: &ControlledConfigStore,
    allow_alternate_cores: bool,
    cause: &std::io::Error,
) -> std::io::Result<CoreStartupState> {
    let recovery_profile = bundled_recovery_profile()
        .unwrap_or(project_root()?.join("platforms/common/recovery.yaml"));
    if !recovery_profile.is_file() {
        return Err(std::io::Error::other(zenclash_i18n::text_with(
            "startup.recovery_missing",
            &[
                ("path", recovery_profile.display().to_string()),
                ("cause", cause.to_string()),
            ],
        )));
    }
    let mut candidates = vec![(requested_core, requested_binary.map(Path::to_path_buf))];
    if allow_alternate_cores {
        if let Some(kind) = preferences.last_known_good_core {
            let candidate = (kind, preferences.last_known_good_binary.clone());
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
        for kind in [CoreKind::Mihomo, CoreKind::Meow] {
            let candidate = (kind, None);
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }

    let mut failures = vec![cause.to_string()];
    for (kind, binary) in candidates {
        match bootstrap_core(
            runtime,
            kind,
            binary.as_deref(),
            controlled_config_store,
            &[],
            Some(&recovery_profile),
            false,
        ) {
            Ok(bootstrapped) => {
                let mut notice = zenclash_i18n::text_with(
                    "startup.recovery_active",
                    &[
                        ("core", kind.display_name().to_owned()),
                        ("cause", cause.to_string()),
                    ],
                );
                if let Some(listener_notice) = bootstrapped.startup_notice {
                    notice.push_str(&zenclash_i18n::text("startup.separator"));
                    notice.push_str(&listener_notice);
                }
                tracing::warn!(core = %kind, %cause, "started with the packaged recovery profile");
                return Ok(CoreStartupState {
                    kind,
                    endpoint: bootstrapped.endpoint,
                    process: bootstrapped.process,
                    profile: bootstrapped.profile,
                    notice: Some(notice),
                    error: None,
                });
            }
            Err(error) => failures.push(error.to_string()),
        }
    }
    Err(std::io::Error::other(zenclash_i18n::text_with(
        "startup.recovery_failed",
        &[(
            "errors",
            failures.join(&zenclash_i18n::text("startup.separator")),
        )],
    )))
}

fn remember_working_core(
    store: Option<&AppPreferencesStore>,
    current: &mut AppPreferences,
    kind: CoreKind,
    binary: Option<&Path>,
) {
    let (Some(store), Some(binary)) = (store, binary) else {
        return;
    };
    let binary = binary.to_path_buf();
    match store.update(|preferences| {
        preferences.last_known_good_core = Some(kind);
        preferences.last_known_good_binary = Some(binary);
    }) {
        Ok(updated) => *current = updated,
        Err(error) => tracing::warn!(%error, "failed to remember last working core"),
    }
}

fn allocate_managed_controller() -> std::io::Result<MihomoEndpoint> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    MihomoEndpoint::with_random_secret(format!("127.0.0.1:{port}")).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tracing_tests {
    use super::{
        append_startup_notice, is_controller_listener_conflict, offline_core_state,
        prepare_offline_startup, project_root, tracing_filter,
    };
    use zenclash_core::{CoreKind, ServiceHealthKind};

    #[test]
    fn pending_gui_has_no_controller_owner_or_applied_configuration() {
        let pending = super::prepare_disconnected_startup(CoreKind::Mihomo, None).unwrap();
        assert_eq!(pending.client.endpoint().unwrap().controller, "127.0.0.1:0");
        assert!(!pending.session.is_managed());
        assert!(pending.profile.is_none());
        assert!(pending.initialization.is_none());
        assert!(pending.error.is_none());
    }

    #[test]
    fn pending_gui_preserves_ready_configuration_libraries() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-pending-libraries-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let inputs = super::StartupInputs {
            preferences_store: None,
            preferences: zenclash_core::AppPreferences::default(),
            requested_core: CoreKind::Mihomo,
            environment_core: None,
            controlled_config_store: zenclash_core::ControlledConfigStore::new(
                root.join("controlled"),
            ),
            profile_store: zenclash_core::ProfileStore::new(root.join("profiles")).unwrap(),
            override_store: zenclash_core::YamlOverrideStore::new(root.join("overrides")).unwrap(),
            recovery_notices: Vec::new(),
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let services = super::prepare_pending_services(
            &inputs,
            runtime.handle(),
            std::sync::Arc::new(parking_lot::Mutex::new(None)),
        )
        .unwrap();
        let profile_root = services
            .profile_store
            .as_ref()
            .map(|store| store.root().to_path_buf());
        let override_root = services
            .override_store
            .as_ref()
            .map(|store| store.root().to_path_buf());
        drop(services);
        runtime.shutdown_timeout(std::time::Duration::from_secs(1));
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(profile_root, Some(root.join("profiles")), "配置仓库不可用");
        assert_eq!(
            override_root,
            Some(root.join("overrides")),
            "YAML 覆写仓库不可用"
        );
    }

    #[test]
    fn cancelled_bootstrap_returns_before_loading_layers_or_starting_a_core() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-cancelled-startup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let controlled = zenclash_core::ControlledConfigStore::new(root.join("controlled"));
        std::fs::create_dir_all(root.join("controlled")).unwrap();
        let damaged = root.join("controlled/override.yaml");
        std::fs::write(&damaged, "tun: [invalid").unwrap();
        let inputs = super::StartupInputs {
            preferences_store: None,
            preferences: zenclash_core::AppPreferences::default(),
            requested_core: CoreKind::Mihomo,
            environment_core: None,
            controlled_config_store: controlled,
            profile_store: zenclash_core::ProfileStore::new(root.join("profiles")).unwrap(),
            override_store: zenclash_core::YamlOverrideStore::new(root.join("overrides")).unwrap(),
            recovery_notices: Vec::new(),
        };
        let cleanup = std::sync::Arc::new(parking_lot::Mutex::new(None));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = super::prepare_application(
            &runtime,
            inputs,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            cleanup.clone(),
            std::sync::Arc::new(parking_lot::Mutex::new(None)),
        );
        let error = result.err().expect("cancelled bootstrap must stop");
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert!(cleanup.lock().is_none());
        assert_eq!(std::fs::read_to_string(damaged).unwrap(), "tun: [invalid");
        assert!(root.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn blocked_service_startup_keeps_the_gui_available_without_a_kernel() {
        for reason in [
            super::startup::StartupBlocked::Health(ServiceHealthKind::Unavailable(
                "service stopped".into(),
            )),
            super::startup::StartupBlocked::Health(ServiceHealthKind::Unknown),
            super::startup::StartupBlocked::ConfigurationUnknown,
        ] {
            let prepared = prepare_offline_startup(
                CoreKind::Mihomo,
                super::startup::blocked_message(reason.clone()),
            )
            .unwrap();
            assert_eq!(
                prepared.client.endpoint().unwrap().controller,
                "127.0.0.1:0"
            );
            assert!(!prepared.session.is_managed());
            assert!(prepared.session.is_offline_recovery());
            assert!(prepared.session.runtime_descriptor().binary().is_none());
            assert!(prepared.profile.is_none());
            assert!(prepared.initialization.is_none());
            assert_eq!(
                prepared.error,
                Some(super::startup::blocked_message(reason))
            );
        }
    }

    #[test]
    fn failed_core_preparation_preserves_the_error_for_the_gui() {
        let prepared = prepare_offline_startup(CoreKind::Mihomo, "invalid profile".into()).unwrap();
        assert_eq!(prepared.error.as_deref(), Some("invalid profile"));
        assert_eq!(
            prepared.client.endpoint().unwrap().controller,
            "127.0.0.1:0"
        );
        assert!(prepared.session.runtime_descriptor().binary().is_none());
    }

    #[test]
    fn offline_startup_keeps_the_selected_core_for_service_maintenance() {
        let selected = std::env::temp_dir().join("missing-selected-mihomo");
        let prepared = super::prepare_offline_startup_with_binary(
            CoreKind::Mihomo,
            "invalid profile".into(),
            Some(selected),
        )
        .unwrap();
        let capture = zenclash_core::TrafficCaptureSession::new(
            prepared.session.clone(),
            zenclash_core::ControlledConfigStore::new(
                std::env::temp_dir().join("offline-unused-controlled"),
            ),
            None,
            None,
        );
        let manager = zenclash_core::ServiceManager::new(prepared.session, capture);
        assert!(
            manager
                .request_maintenance(zenclash_core::ServiceOperation::Repair, None)
                .is_ok()
        );
        assert!(manager.request_enable_tun().is_err());
        assert_eq!(prepared.error.as_deref(), Some("invalid profile"));
    }

    #[test]
    fn pending_disconnected_startup_has_no_maintenance_capability() {
        let prepared = super::prepare_disconnected_startup(CoreKind::Mihomo, None).unwrap();
        assert!(!prepared.session.is_offline_recovery());
        assert!(prepared.error.is_none());
    }

    #[test]
    fn application_filter_overrides_verbose_protocol_targets() {
        let filter = tracing_filter(Some("debug,tungstenite=trace,tokio_tungstenite=trace"));
        let filter = filter.to_string();

        assert!(filter.contains("tungstenite=warn"));
        assert!(filter.contains("tokio_tungstenite=warn"));
        assert!(filter.contains("reqwest=warn"));
        assert!(!filter.contains("tungstenite=trace"));
        assert!(!filter.contains("tokio_tungstenite=trace"));
    }

    #[test]
    fn failed_core_recovery_uses_an_impossible_controller_and_keeps_ui_state() {
        let state = offline_core_state(
            CoreKind::Mihomo,
            &std::io::Error::other("no eligible binary"),
        );

        assert_eq!(state.endpoint.controller, "127.0.0.1:0");
        assert!(state.process.is_none());
        assert!(state.notice.is_none());
        assert!(state.error.is_some_and(
            |message| message.contains("9090") && message.contains("no eligible binary")
        ));
    }

    #[test]
    fn packaged_recovery_profile_is_direct_and_exposes_no_proxy_listener() {
        let payload = std::fs::read_to_string(
            project_root()
                .unwrap()
                .join("platforms/common/recovery.yaml"),
        )
        .unwrap();
        let config: serde_yaml::Value = serde_yaml::from_str(&payload).unwrap();

        assert_eq!(
            config.get("mixed-port").and_then(serde_yaml::Value::as_u64),
            Some(0)
        );
        assert_eq!(
            config.get("mode").and_then(serde_yaml::Value::as_str),
            Some("direct")
        );
        assert_eq!(
            config
                .get("rules")
                .and_then(serde_yaml::Value::as_sequence)
                .and_then(|rules| rules.first())
                .and_then(serde_yaml::Value::as_str),
            Some("MATCH,DIRECT")
        );
    }

    #[test]
    fn recovery_notices_are_combined_without_losing_the_primary_reason() {
        let mut notice = Some("活动配置无效".to_owned());
        append_startup_notice(&mut notice, "受控层已隔离".to_owned());

        assert_eq!(
            notice,
            Some(format!(
                "活动配置无效{}受控层已隔离",
                zenclash_i18n::text("startup.separator")
            ))
        );
    }

    #[test]
    fn only_external_controller_bind_failures_are_retryable() {
        assert!(is_controller_listener_conflict(
            "External controller listen error: listen tcp 127.0.0.1:19191: bind: address already in use"
        ));
        assert!(!is_controller_listener_conflict(
            "Start Mixed proxy error: listen tcp 127.0.0.1:7890: address already in use"
        ));
    }
}
