//! Headless core, service, legacy local, and recovery startup routines.

use std::{
    net::TcpListener,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use zenclash_core::{
    AppPreferences, AppPreferencesStore, ControlledConfigStore, CoreInitializationOutcome,
    CoreKind, CoreLaunchConfig, CoreSession, EffectiveConfigIntent, ManagedCoreProcess,
    MihomoClient, MihomoEndpoint, MihomoRuntimeResources, ProfileStore, bundled_recovery_profile,
};

use super::routing::{
    StartupCancellation, connection_message, is_startup_cancelled, startup_cancelled_error,
};

const MANAGED_CONTROLLER_ATTEMPTS: usize = 3;

pub(super) struct RecoveredCore {
    pub(super) kind: CoreKind,
    pub(super) binary: Option<PathBuf>,
    pub(super) endpoint: MihomoEndpoint,
    pub(super) process: Option<Arc<ManagedCoreProcess>>,
    pub(super) profile: Option<PathBuf>,
    pub(super) startup_notice: Option<String>,
}

pub(super) struct BootstrappedCore {
    pub(super) endpoint: MihomoEndpoint,
    pub(super) process: Option<Arc<ManagedCoreProcess>>,
    pub(super) profile: Option<PathBuf>,
    pub(super) startup_notice: Option<String>,
}

pub(super) struct CoreStartupState {
    pub(super) kind: CoreKind,
    pub(super) endpoint: MihomoEndpoint,
    pub(super) process: Option<Arc<ManagedCoreProcess>>,
    pub(super) profile: Option<PathBuf>,
    pub(super) notice: Option<String>,
    pub(super) error: Option<String>,
}

pub(super) struct PreparedStartup {
    pub(super) kind: CoreKind,
    pub(super) client: MihomoClient,
    pub(super) session: CoreSession,
    pub(super) profile: Option<PathBuf>,
    pub(super) notice: Option<String>,
    pub(super) error: Option<String>,
    pub(super) initialization: Option<CoreInitializationOutcome>,
}

pub(super) struct LegacyStartupContext<'a> {
    pub(super) runtime: &'a tokio::runtime::Runtime,
    pub(super) preferences: &'a AppPreferences,
    pub(super) requested_core: CoreKind,
    pub(super) preferred_binary: Option<&'a Path>,
    pub(super) allow_core_recovery: bool,
    pub(super) profile_store: Option<&'a ProfileStore>,
    pub(super) controlled_config_store: &'a ControlledConfigStore,
    pub(super) override_paths: &'a [PathBuf],
    pub(super) cancellation: &'a StartupCancellation,
}

#[cfg(test)]
pub(super) fn prepare_offline_startup(
    kind: CoreKind,
    error: String,
) -> Result<PreparedStartup, Box<dyn std::error::Error>> {
    prepare_offline_startup_with_binary(kind, error, None)
}

pub(super) fn prepare_offline_startup_with_binary(
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

pub(super) fn prepare_disconnected_startup(
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

pub(super) fn recover_offline_startup(
    managed_mihomo: bool,
    requested_core: CoreKind,
    preferred_binary: Option<PathBuf>,
    cancellation: &StartupCancellation,
    error: Box<dyn std::error::Error>,
) -> Result<PreparedStartup, Box<dyn std::error::Error>> {
    if is_startup_cancelled(error.as_ref()) {
        return Err(error);
    }
    if cancellation.is_cancelled() {
        return Err(Box::new(startup_cancelled_error()));
    }
    if managed_mihomo {
        prepare_offline_startup_with_binary(requested_core, error.to_string(), preferred_binary)
    } else {
        prepare_disconnected_startup(requested_core, Some(error.to_string()))
    }
}

pub(super) fn prepare_legacy_startup(
    context: &LegacyStartupContext<'_>,
) -> Result<PreparedStartup, Box<dyn std::error::Error>> {
    context.cancellation.ensure_active()?;
    let initial = bootstrap_core(
        context,
        context.requested_core,
        context.preferred_binary,
        context.override_paths,
        None,
        true,
    );
    let startup = match initial {
        Ok(bootstrapped) => CoreStartupState {
            kind: context.requested_core,
            endpoint: bootstrapped.endpoint,
            process: bootstrapped.process,
            profile: bootstrapped.profile,
            notice: bootstrapped.startup_notice,
            error: None,
        },
        Err(initial_error) if initial_error.kind() == std::io::ErrorKind::Interrupted => {
            return Err(initial_error.into());
        }
        Err(initial_error) if context.allow_core_recovery => {
            context.cancellation.ensure_active()?;
            match recover_core(context, &initial_error) {
                Ok(recovered) => {
                    let source = recovered.binary.as_ref().map_or_else(
                        || zenclash_i18n::text("startup.automatic_discovery"),
                        |path| path.display().to_string(),
                    );
                    let mut notice = zenclash_i18n::text_with(
                        "startup.core_recovered",
                        &[
                            ("requested", context.requested_core.to_string()),
                            ("recovered", recovered.kind.to_string()),
                            ("source", source),
                            ("error", initial_error.to_string()),
                        ],
                    );
                    if let Some(listener_notice) = recovered.startup_notice {
                        notice.push_str(&zenclash_i18n::text("startup.separator"));
                        notice.push_str(&listener_notice);
                    }
                    tracing::warn!(
                        requested = %context.requested_core,
                        fallback = %recovered.kind,
                        %initial_error,
                        "recovered with last usable core"
                    );
                    CoreStartupState {
                        kind: recovered.kind,
                        endpoint: recovered.endpoint,
                        process: recovered.process,
                        profile: recovered.profile,
                        notice: Some(notice),
                        error: None,
                    }
                }
                Err(recovery_error) if recovery_error.kind() == std::io::ErrorKind::Interrupted => {
                    return Err(recovery_error.into());
                }
                Err(recovery_error) => match recover_safe_profile(context, true, &recovery_error) {
                    Ok(state) => state,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                        return Err(error.into());
                    }
                    Err(error) => {
                        offline_core_state(context.requested_core, context.profile_store, &error)
                    }
                },
            }
        }
        Err(error) => {
            context.cancellation.ensure_active()?;
            if std::env::var_os("ZENCLASH_CONFIG").is_none() {
                match recover_safe_profile(context, false, &error) {
                    Ok(state) => state,
                    Err(recovery) if recovery.kind() == std::io::ErrorKind::Interrupted => {
                        return Err(recovery.into());
                    }
                    Err(recovery) => {
                        offline_core_state(context.requested_core, context.profile_store, &recovery)
                    }
                }
            } else {
                offline_core_state(context.requested_core, context.profile_store, &error)
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
        context.override_paths.to_vec(),
    )?;
    if startup_error.is_none()
        && mihomo_process.is_none()
        && let Some(profile) = profile_path.as_ref()
        && let Err(error) = context.runtime.block_on(core_session.apply(
            context.controlled_config_store,
            EffectiveConfigIntent::ActivateProfile {
                profile: profile.clone(),
                overrides: context.override_paths.to_vec(),
            },
        ))
    {
        tracing::warn!(%error, core = %core_kind, "initial core configuration synchronization failed");
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

pub(super) fn prepare_service_startup(
    runtime: &tokio::runtime::Runtime,
    resources: Option<&MihomoRuntimeResources>,
    core_source: Option<PathBuf>,
    store: &ControlledConfigStore,
    overrides: &[PathBuf],
    cancellation: &StartupCancellation,
) -> Result<PreparedStartup, Box<dyn std::error::Error>> {
    cancellation.ensure_active()?;
    let resources = resources
        .ok_or_else(|| std::io::Error::other(zenclash_i18n::text("core_page.service.unknown")))?;
    let client = runtime
        .block_on(
            cancellation.run_until_cancelled(MihomoClient::connect_service_with_core(
                resources.home_dir().to_path_buf(),
                core_source,
            )),
        )?
        .map_err(|error| std::io::Error::other(connection_message(&error)))?;
    let session = CoreSession::open(CoreKind::Mihomo, client.clone())?;
    let cancel_watch = {
        let session = session.clone();
        let cancellation = cancellation.clone();
        runtime.spawn(async move {
            cancellation.cancelled().await;
            session.request_shutdown();
        })
    };
    let result = runtime.block_on(session.initialize_service_runtime(
        store,
        resources.config_file().to_path_buf(),
        overrides.to_vec(),
    ));
    cancel_watch.abort();
    if cancellation.is_cancelled() {
        let _ = runtime.block_on(session.shutdown());
        return Err(startup_cancelled_error().into());
    }
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

pub(super) async fn wait_for_managed_core_ready(
    process: &ManagedCoreProcess,
    timeout: Duration,
    cancellation: &StartupCancellation,
) -> std::io::Result<Result<(), zenclash_core::MihomoError>> {
    cancellation
        .run_until_cancelled(Box::pin(process.wait_until_ready(timeout)))
        .await
}

fn bootstrap_core(
    context: &LegacyStartupContext<'_>,
    core_kind: CoreKind,
    preferred_binary: Option<&Path>,
    override_paths: &[PathBuf],
    profile_override: Option<&Path>,
    apply_persisted_layers: bool,
) -> std::io::Result<BootstrappedCore> {
    context.cancellation.ensure_active()?;
    let project_root = project_root()?;
    let selected_profile = profile_override
        .map(Path::to_path_buf)
        .or_else(|| selected_profile(&project_root, context.profile_store));

    if std::env::var_os("ZENCLASH_CONTROLLER").is_some() {
        let profile_path = selected_profile.or_else(|| {
            let candidate =
                zenclash_core::WorkspaceResources::new(project_root.as_path()).default_profile();
            candidate.is_file().then_some(candidate)
        });
        return Ok(BootstrappedCore {
            endpoint: MihomoEndpoint::from_env(),
            process: None,
            profile: profile_path,
            startup_notice: None,
        });
    }

    let discovered = match CoreLaunchConfig::discover_for_kind_with_binary_and_config(
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
        let effective_path = context
            .controlled_config_store
            .materialize_with_overrides_for_core(&profile_path, override_paths, core_kind)
            .map_err(std::io::Error::other)?;
        let listener_fallbacks = context
            .controlled_config_store
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
    let launch = match CoreLaunchConfig::for_kind(
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
        context.cancellation.ensure_active()?;
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
        let process = ManagedCoreProcess::spawn(launch).map_err(|error| {
            std::io::Error::other(zenclash_i18n::text_with(
                "startup.managed_spawn",
                &[
                    ("core", core_kind.to_string()),
                    ("error", error.to_string()),
                ],
            ))
        })?;
        match context.runtime.block_on(wait_for_managed_core_ready(
            &process,
            Duration::from_secs(20),
            context.cancellation,
        )) {
            Ok(Ok(())) => {
                tracing::info!(
                    core = %core_kind,
                    controller = %endpoint.controller,
                    attempt,
                    "managed core is ready"
                );
                return Ok(BootstrappedCore {
                    endpoint,
                    process: Some(process),
                    profile: Some(profile_path),
                    startup_notice: listener_notice,
                });
            }
            Err(cancelled) => {
                tracing::info!(
                    core = %core_kind,
                    attempt,
                    "stopping managed core because startup was cancelled"
                );
                if let Err(stop_error) = process.stop() {
                    tracing::warn!(%stop_error, core = %core_kind, "failed to stop cancelled managed core");
                }
                return Err(cancelled);
            }
            Ok(Err(error)) => {
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
                context.cancellation.ensure_active()?;
                if controller_conflict && attempt < MANAGED_CONTROLLER_ATTEMPTS {
                    tracing::warn!(
                        core = %core_kind,
                        attempt,
                        "managed controller port was taken before core bind; retrying"
                    );
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

pub(super) fn is_controller_listener_conflict(error: &str) -> bool {
    let normalized = error.to_ascii_lowercase();
    normalized.contains("external controller listen error")
        && normalized.contains("address already in use")
}

pub(super) fn project_root() -> std::io::Result<PathBuf> {
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .ok_or_else(|| std::io::Error::other(zenclash_i18n::text("startup.workspace")))?
        .to_path_buf())
}

fn selected_profile(project_root: &Path, profile_store: Option<&ProfileStore>) -> Option<PathBuf> {
    std::env::var_os("ZENCLASH_CONFIG")
        .map(PathBuf::from)
        .or_else(|| {
            profile_store.and_then(|store| {
                store
                    .active_path()
                    .inspect_err(|error| tracing::warn!(%error, "failed to load active profile"))
                    .ok()
                    .flatten()
            })
        })
        .or_else(|| {
            let candidate = zenclash_core::WorkspaceResources::new(project_root).default_profile();
            candidate.is_file().then_some(candidate)
        })
}

pub(super) fn offline_core_state(
    requested_core: CoreKind,
    profile_store: Option<&ProfileStore>,
    error: &std::io::Error,
) -> CoreStartupState {
    tracing::error!(
        %error,
        core = %requested_core,
        "all eligible Mihomo binaries failed; opening recovery UI without a controller"
    );
    let profile = project_root()
        .ok()
        .and_then(|project_root| selected_profile(&project_root, profile_store));
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

pub(super) fn recover_core(
    context: &LegacyStartupContext<'_>,
    initial_error: &std::io::Error,
) -> std::io::Result<RecoveredCore> {
    context.cancellation.ensure_active()?;
    let mut candidates = Vec::new();
    if let Some(kind) = context.preferences.last_known_good_core {
        let binary = context.preferences.last_known_good_binary.clone();
        if kind != context.requested_core || binary.as_deref() != context.preferred_binary {
            candidates.push((kind, binary));
        }
    }
    if context.requested_core != CoreKind::Mihomo || context.preferred_binary.is_some() {
        candidates.push((CoreKind::Mihomo, None));
    }

    let mut failures = vec![initial_error.to_string()];
    for (kind, binary) in candidates {
        context.cancellation.ensure_active()?;
        match bootstrap_core(
            context,
            kind,
            binary.as_deref(),
            context.override_paths,
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
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => return Err(error),
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

pub(super) fn recover_safe_profile(
    context: &LegacyStartupContext<'_>,
    allow_binary_fallback: bool,
    cause: &std::io::Error,
) -> std::io::Result<CoreStartupState> {
    context.cancellation.ensure_active()?;
    let recovery_profile = bundled_recovery_profile()
        .unwrap_or(zenclash_core::WorkspaceResources::new(project_root()?).recovery_profile());
    if !recovery_profile.is_file() {
        return Err(std::io::Error::other(zenclash_i18n::text_with(
            "startup.recovery_missing",
            &[
                ("path", recovery_profile.display().to_string()),
                ("cause", cause.to_string()),
            ],
        )));
    }
    let mut candidates = vec![(
        context.requested_core,
        context.preferred_binary.map(Path::to_path_buf),
    )];
    if allow_binary_fallback {
        if let Some(kind) = context.preferences.last_known_good_core {
            let candidate = (kind, context.preferences.last_known_good_binary.clone());
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
        let default_candidate = (CoreKind::Mihomo, None);
        if !candidates.contains(&default_candidate) {
            candidates.push(default_candidate);
        }
    }

    let mut failures = vec![cause.to_string()];
    for (kind, binary) in candidates {
        context.cancellation.ensure_active()?;
        match bootstrap_core(
            context,
            kind,
            binary.as_deref(),
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
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => return Err(error),
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

pub(super) fn remember_working_core(
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
