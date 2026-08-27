#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! Native `ZenClash` executable bootstrap and managed Mihomo discovery.

use std::{
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use gpui::Application;
use gpui_component_assets::Assets;
use tracing_subscriber::{filter::Directive, EnvFilter};
use zenclash_core::{
    bundled_recovery_profile, AppInstanceLock, AppPreferences, AppPreferencesStore,
    ControlledConfigStore, CoreKind, LogMonitor, MihomoClient, MihomoEndpoint, MihomoLaunchConfig,
    MihomoProcess, ProfileStore, TrafficMonitor, YamlOverrideStore,
};
use zenclash_ui::app;

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
        eprintln!("ZenClash 启动失败：{error}");
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
        current.push('；');
        current.push_str(&notice);
    } else {
        *target = Some(notice);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("zenclash-io")
        .build()?;
    let _runtime_guard = runtime.enter();
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
    let (mut preferences, preferences_recovery_notice) =
        load_preferences(preferences_store.as_ref())?;
    let environment_core = std::env::var("ZENCLASH_CORE")
        .ok()
        .map(|value| value.parse())
        .transpose()?;
    let requested_core = environment_core.unwrap_or(preferences.core_kind);
    let controlled_config_store = ControlledConfigStore::discover()?;
    let mut recovery_notices = preferences_recovery_notice.into_iter().collect::<Vec<_>>();
    let profile_store = ProfileStore::discover()?;
    if let Some(path) = profile_store.quarantine_invalid_index()? {
        recovery_notices.push(format!(
            "损坏的配置索引已隔离到 {}，托管 YAML 文件仍保留",
            path.display()
        ));
    }
    if let Some(path) = controlled_config_store.quarantine_invalid_patch()? {
        recovery_notices.push(format!(
            "损坏的受控配置已隔离到 {}，本次从空受控层恢复",
            path.display()
        ));
    }
    let override_store = YamlOverrideStore::discover()?;
    if let Some(path) = override_store.quarantine_invalid_manifest()? {
        recovery_notices.push(format!(
            "损坏的 YAML 覆写清单已隔离到 {}，托管 YAML 文件仍保留",
            path.display()
        ));
    }
    let override_paths = override_store.load_enabled_paths()?;
    let preferred_binary = preferences.core_binaries.path(requested_core);
    let initial = bootstrap_core(
        &runtime,
        requested_core,
        preferred_binary,
        &controlled_config_store,
        &override_paths,
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
        Err(initial_error) if environment_core.is_none() => {
            match recover_core(
                &runtime,
                &preferences,
                requested_core,
                preferred_binary,
                &controlled_config_store,
                &override_paths,
                &initial_error,
            ) {
                Ok(recovered) => {
                    let source = recovered
                        .binary
                        .as_ref()
                        .map_or_else(|| "自动发现".to_owned(), |path| path.display().to_string());
                    let mut notice = format!(
                        "首选 {} 启动失败，已明确恢复到 {}（{}）。首选项没有被覆盖，请在“设置 → 运行内核”重新检测或选择文件。原因：{}",
                        requested_core, recovered.kind, source, initial_error
                    );
                    if let Some(listener_notice) = recovered.startup_notice {
                        notice.push('；');
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
                    &runtime,
                    &preferences,
                    requested_core,
                    preferred_binary,
                    &controlled_config_store,
                    true,
                    &recovery_error,
                )
                .unwrap_or_else(|error| offline_core_state(requested_core, &error)),
            }
        }
        Err(error) => {
            if std::env::var_os("ZENCLASH_CONFIG").is_none() {
                recover_safe_profile(
                    &runtime,
                    &preferences,
                    requested_core,
                    preferred_binary,
                    &controlled_config_store,
                    false,
                    &error,
                )
                .unwrap_or_else(|recovery| offline_core_state(requested_core, &recovery))
            } else {
                offline_core_state(requested_core, &error)
            }
        }
    };
    let mut startup = startup;
    for notice in recovery_notices {
        append_startup_notice(&mut startup.notice, notice);
    }
    let CoreStartupState {
        kind: core_kind,
        endpoint,
        process: mihomo_process,
        profile: profile_path,
        notice: startup_notice,
        error: startup_error,
    } = startup;
    remember_working_core(
        preferences_store.as_ref(),
        &mut preferences,
        core_kind,
        mihomo_process.as_ref(),
    );
    let client = MihomoClient::new(endpoint.clone())?;
    let client = mihomo_process.as_ref().map_or(client.clone(), |process| {
        client.with_config_validator(process.config_validator())
    });
    if startup_error.is_none()
        && mihomo_process.is_none()
        && core_kind.capabilities().full_config_reload
    {
        if let Some(profile) = profile_path.as_ref() {
            if let Err(error) = runtime.block_on(controlled_config_store.reload_with_overrides(
                &client,
                profile,
                override_paths,
            )) {
                tracing::warn!(%error, core = %core_kind, "initial core configuration synchronization failed");
            }
        }
    } else if !core_kind.capabilities().full_config_reload {
        tracing::info!(core = %core_kind, "skipping unsupported full configuration hot reload");
    }
    let traffic = TrafficMonitor::start(runtime.handle(), endpoint.clone());
    let logs = LogMonitor::start(
        runtime.handle(),
        endpoint,
        zenclash_core::MihomoLogLevel::Info,
    );
    let runtime_handle = runtime.handle().clone();
    let restart_after_exit = Arc::new(parking_lot::Mutex::new(None));
    let app_restart_after_exit = Arc::clone(&restart_after_exit);
    let restart_elevated_after_exit = Arc::new(AtomicBool::new(false));
    let app_restart_elevated_after_exit = Arc::clone(&restart_elevated_after_exit);

    Application::new().with_assets(Assets).run(move |cx| {
        app::init(cx);
        app::create_main_window(
            app::AppServices {
                preferences_store,
                preferences,
                core_kind,
                client,
                traffic_monitor: traffic,
                log_monitor: logs,
                mihomo_process,
                profile_path,
                controlled_config_store,
                runtime: runtime_handle,
                startup_notice,
                startup_error,
                restart_after_exit: app_restart_after_exit,
                restart_elevated_after_exit: app_restart_elevated_after_exit,
            },
            cx,
        );
        cx.activate(true);
    });
    drop(_instance_lock);
    if let Some(executable) = restart_after_exit.lock().take() {
        spawn_restarted_process(
            &executable,
            restart_elevated_after_exit.load(Ordering::Acquire),
        )
        .map_err(|error| {
            std::io::Error::other(format!(
                "ZenClash 已退出，但无法从 {} 重新启动：{error}",
                executable.display()
            ))
        })?;
    }
    Ok(())
}

fn spawn_restarted_process(executable: &Path, elevated: bool) -> std::io::Result<()> {
    if elevated {
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;

            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let script = "Start-Process -FilePath $args[0] -Verb RunAs";
            let mut command = Command::new("powershell.exe");
            command
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-WindowStyle",
                    "Hidden",
                    "-Command",
                    script,
                ])
                .arg(executable)
                .creation_flags(CREATE_NO_WINDOW);
            command.spawn()?;
            return Ok(());
        }
        #[cfg(not(target_os = "windows"))]
        return Err(std::io::Error::other("当前平台不支持管理员权限重启"));
    }
    Command::new(executable).spawn()?;
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
            Some(format!(
                "损坏或不兼容的应用设置已隔离到 {}，本次从默认设置恢复",
                path.display()
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
            return Err(std::io::Error::other(format!(
                "无法启动显式选择的 {core_kind}：{error}"
            )));
        }
    };
    let profile_path = selected_profile.unwrap_or_else(|| discovered.config_file.clone());
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
        format!(
            "检测到监听端口被其他进程占用，本次运行已临时改用 {changes}；订阅和持久设置未修改。"
        )
    });
    let launch = match MihomoLaunchConfig::for_kind(
        core_kind,
        discovered.binary,
        effective_path,
        discovered.home_dir,
    ) {
        Ok(launch) => launch,
        Err(error) => {
            return Err(std::io::Error::other(format!(
                "{core_kind} 的活动配置无效：{error}"
            )));
        }
    };
    launch.validate_config().map_err(|error| {
        std::io::Error::other(format!("{core_kind} 的活动配置未通过内核预检：{error}"))
    })?;
    for attempt in 1..=MANAGED_CONTROLLER_ATTEMPTS {
        let controller = allocate_managed_controller().map_err(|error| {
            std::io::Error::other(format!(
                "无法为托管 {core_kind} 分配隔离控制器，已拒绝连接固定或无鉴权控制器：{error}"
            ))
        })?;
        let launch = launch.clone().with_controller_endpoint(controller);
        let endpoint = launch.endpoint.clone();
        let process = MihomoProcess::spawn(launch)
            .map_err(|error| std::io::Error::other(format!("无法启动托管 {core_kind}：{error}")))?;
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
                    return Err(std::io::Error::other(format!(
                        "托管 {core_kind} 未能就绪：{error}；停止失败：{stop_error}"
                    )));
                }
                if controller_conflict && attempt < MANAGED_CONTROLLER_ATTEMPTS {
                    tracing::warn!(core = %core_kind, attempt, "managed controller port was taken before core bind; retrying");
                    continue;
                }
                return Err(std::io::Error::other(format!(
                    "托管 {core_kind} 未能就绪：{error}"
                )));
            }
        }
    }
    Err(std::io::Error::other(format!(
        "托管 {core_kind} 控制器连续 {MANAGED_CONTROLLER_ATTEMPTS} 次被抢占"
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
        .ok_or_else(|| std::io::Error::other("无法从 Cargo 清单路径确定 ZenClash 工作区"))?
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
        error: Some(format!(
            "所有可用内核都启动失败，ZenClash 已进入离线恢复模式，未连接默认 9090 控制器。请在“设置 → 运行内核”重新检测或选择可执行文件，然后重启应用。原因：{error}"
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
    Err(std::io::Error::other(format!(
        "首选内核与恢复内核均启动失败：{}",
        failures.join("；")
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
        return Err(std::io::Error::other(format!(
            "活动配置启动失败，且内置恢复配置缺失：{}；原始原因：{cause}",
            recovery_profile.display()
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
                let mut notice = format!(
                    "活动配置未能启动，已使用内置直连恢复配置运行 {}；原活动选择和源文件均未改写。请在“配置”页导入或切换到有效配置。原因：{cause}",
                    kind.display_name()
                );
                if let Some(listener_notice) = bootstrapped.startup_notice {
                    notice.push('；');
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
    Err(std::io::Error::other(format!(
        "活动配置与内置恢复配置均启动失败：{}",
        failures.join("；")
    )))
}

fn remember_working_core(
    store: Option<&AppPreferencesStore>,
    current: &mut AppPreferences,
    kind: CoreKind,
    process: Option<&Arc<MihomoProcess>>,
) {
    let (Some(store), Some(process)) = (store, process) else {
        return;
    };
    let binary = process.snapshot().binary;
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
        append_startup_notice, is_controller_listener_conflict, offline_core_state, project_root,
        tracing_filter,
    };
    use zenclash_core::CoreKind;

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
        assert!(state.error.is_some_and(|message| {
            message.contains("离线恢复模式") && message.contains("运行内核")
        }));
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

        assert_eq!(notice.as_deref(), Some("活动配置无效；受控层已隔离"));
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
