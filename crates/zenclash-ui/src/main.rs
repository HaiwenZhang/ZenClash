#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! Native `ZenClash` executable bootstrap and managed Mihomo discovery.

use std::{net::TcpListener, path::PathBuf, sync::Arc, time::Duration};

use gpui::Application;
use gpui_component_assets::Assets;
use tracing_subscriber::{filter::Directive, EnvFilter};
use zenclash_core::{
    AppPreferences, AppPreferencesStore, ControlledConfigStore, CoreKind, LogMonitor, MihomoClient,
    MihomoEndpoint, MihomoLaunchConfig, MihomoProcess, ProfileStore, TrafficMonitor,
    YamlOverrideStore,
};
use zenclash_ui::app;

const DEFAULT_TRACING_FILTER: &str = "zenclash=info,zenclash_core=info,zenclash_ui=info";
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

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("zenclash-io")
        .build()?;
    let _runtime_guard = runtime.enter();
    let (preferences_store, preferences) = load_preferences();
    let environment_core = std::env::var("ZENCLASH_CORE")
        .ok()
        .map(|value| value.parse())
        .transpose()?;
    let requested_core = environment_core.unwrap_or(preferences.core_kind);
    let controlled_config_store = ControlledConfigStore::discover()?;
    let override_paths = YamlOverrideStore::discover()?.load_enabled_paths()?;
    let preferred_binary = preferences.core_binaries.path(requested_core);
    let initial = bootstrap_core(
        &runtime,
        requested_core,
        preferred_binary,
        &controlled_config_store,
        &override_paths,
    );
    let (core_kind, endpoint, mihomo_process, profile_path, startup_notice) = match initial {
        Ok((endpoint, process, profile)) => (requested_core, endpoint, process, profile, None),
        Err(initial_error) if environment_core.is_none() => {
            let recovered = recover_core(
                &runtime,
                &preferences,
                requested_core,
                preferred_binary,
                &controlled_config_store,
                &override_paths,
                &initial_error,
            )?;
            let source = recovered
                .binary
                .as_ref()
                .map_or_else(|| "自动发现".to_owned(), |path| path.display().to_string());
            let notice = format!(
                "首选 {} 启动失败，已明确恢复到 {}（{}）。首选项没有被覆盖，请在“设置 → 核心舱”重新检测或选择文件。原因：{}",
                requested_core,
                recovered.kind,
                source,
                initial_error
            );
            tracing::warn!(requested = %requested_core, fallback = %recovered.kind, %initial_error, "recovered with last usable core");
            (
                recovered.kind,
                recovered.endpoint,
                recovered.process,
                recovered.profile,
                Some(notice),
            )
        }
        Err(error) => return Err(error.into()),
    };
    remember_working_core(
        preferences_store.as_ref(),
        core_kind,
        mihomo_process.as_ref(),
    );
    let client = MihomoClient::new(endpoint.clone())?;
    if core_kind.capabilities().full_config_reload {
        if let Some(profile) = profile_path.as_ref() {
            if let Err(error) = runtime.block_on(controlled_config_store.reload_with_overrides(
                &client,
                profile,
                override_paths,
            )) {
                tracing::warn!(%error, core = %core_kind, "initial core configuration synchronization failed");
            }
        }
    } else {
        tracing::info!(core = %core_kind, "skipping unsupported full configuration hot reload");
    }
    let traffic = TrafficMonitor::start(runtime.handle(), endpoint.clone());
    let logs = LogMonitor::start(
        runtime.handle(),
        endpoint,
        zenclash_core::MihomoLogLevel::Info,
    );
    let runtime_handle = runtime.handle().clone();

    Application::new().with_assets(Assets).run(move |cx| {
        app::init(cx);
        app::create_main_window(
            app::AppServices {
                core_kind,
                client,
                traffic_monitor: traffic,
                log_monitor: logs,
                mihomo_process,
                profile_path,
                controlled_config_store,
                runtime: runtime_handle,
                startup_notice,
            },
            cx,
        );
        cx.activate(true);
    });
    Ok(())
}

fn load_preferences() -> (Option<AppPreferencesStore>, AppPreferences) {
    match AppPreferencesStore::discover() {
        Ok(store) => match store.load() {
            Ok(preferences) => (Some(store), preferences),
            Err(error) => {
                tracing::warn!(%error, "failed to load selected core; using defaults");
                (Some(store), AppPreferences::default())
            }
        },
        Err(error) => {
            tracing::warn!(%error, "failed to discover preferences; using defaults");
            (None, AppPreferences::default())
        }
    }
}

fn bootstrap_core(
    runtime: &tokio::runtime::Runtime,
    core_kind: CoreKind,
    preferred_binary: Option<&std::path::Path>,
    controlled_config_store: &ControlledConfigStore,
    override_paths: &[PathBuf],
) -> std::io::Result<(MihomoEndpoint, Option<Arc<MihomoProcess>>, Option<PathBuf>)> {
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .ok_or_else(|| std::io::Error::other("无法从 Cargo 清单路径确定 ZenClash 工作区"))?
        .to_path_buf();
    let selected_profile = std::env::var_os("ZENCLASH_CONFIG")
        .map(PathBuf::from)
        .or_else(|| {
            ProfileStore::discover()
                .and_then(|store| store.active_path())
                .inspect_err(|error| tracing::warn!(%error, "failed to load active profile"))
                .ok()
                .flatten()
        });

    if std::env::var_os("ZENCLASH_CONTROLLER").is_some() {
        let profile_path =
            selected_profile.unwrap_or_else(|| project_root.join("examples/19facdf022b.yaml"));
        return Ok((MihomoEndpoint::from_env(), None, Some(profile_path)));
    }

    let discovered = match MihomoLaunchConfig::discover_for_kind_with_binary(
        &project_root,
        core_kind,
        preferred_binary,
    ) {
        Ok(launch) => launch,
        Err(error) => {
            return Err(std::io::Error::other(format!(
                "无法启动显式选择的 {core_kind}：{error}"
            )));
        }
    };
    let profile_path = selected_profile.unwrap_or_else(|| discovered.config_file.clone());
    let effective_path = controlled_config_store
        .materialize_with_overrides(&profile_path, override_paths)
        .map_err(std::io::Error::other)?;
    let mut launch = match MihomoLaunchConfig::for_kind(
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
    match allocate_managed_controller() {
        Ok(controller) => launch = launch.with_controller_override(controller),
        Err(error) => {
            tracing::warn!(%error, core = %core_kind, "failed to allocate an isolated core controller port");
        }
    }
    let endpoint = launch.endpoint.clone();

    match MihomoProcess::spawn(launch) {
        Ok(process) => {
            match runtime.block_on(process.wait_until_ready(Duration::from_secs(20))) {
                Ok(()) => {
                    tracing::info!(core = %core_kind, controller = %endpoint.controller, "managed core is ready");
                }
                Err(error) => {
                    tracing::error!(%error, core = %core_kind, "managed core failed to become ready");
                    for line in process.snapshot().logs.iter().rev().take(12).rev() {
                        tracing::error!("{line}");
                    }
                    if let Err(stop_error) = process.stop() {
                        tracing::error!(%stop_error, core = %core_kind, "failed to stop unready managed core");
                    }
                    return Err(std::io::Error::other(format!(
                        "托管 {core_kind} 未能就绪：{error}"
                    )));
                }
            }
            Ok((endpoint, Some(process), Some(profile_path)))
        }
        Err(error) => Err(std::io::Error::other(format!(
            "无法启动托管 {core_kind}：{error}"
        ))),
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
        ) {
            Ok((endpoint, process, profile)) => {
                let actual_binary = process
                    .as_ref()
                    .map(|process| process.snapshot().binary)
                    .or(binary);
                return Ok(RecoveredCore {
                    kind,
                    binary: actual_binary,
                    endpoint,
                    process,
                    profile,
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

fn remember_working_core(
    store: Option<&AppPreferencesStore>,
    kind: CoreKind,
    process: Option<&Arc<MihomoProcess>>,
) {
    let (Some(store), Some(process)) = (store, process) else {
        return;
    };
    let binary = process.snapshot().binary;
    if let Err(error) = store.update(|preferences| {
        preferences.last_known_good_core = Some(kind);
        preferences.last_known_good_binary = Some(binary);
    }) {
        tracing::warn!(%error, "failed to remember last working core");
    }
}

fn allocate_managed_controller() -> std::io::Result<String> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(format!("127.0.0.1:{port}"))
}

#[cfg(test)]
mod tracing_tests {
    use super::tracing_filter;

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
}
