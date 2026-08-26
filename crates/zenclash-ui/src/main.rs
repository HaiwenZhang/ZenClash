#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! Native `ZenClash` executable bootstrap and managed Mihomo discovery.

use std::{net::TcpListener, path::PathBuf, sync::Arc, time::Duration};

use gpui::Application;
use gpui_component_assets::Assets;
use zenclash_core::{
    AppPreferencesStore, ControlledConfigStore, CoreKind, LogMonitor, MihomoClient, MihomoEndpoint,
    MihomoLaunchConfig, MihomoProcess, ProfileStore, TrafficMonitor, YamlOverrideStore,
};
use zenclash_ui::app;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "zenclash=info".into()),
        )
        .init();

    if let Err(error) = run() {
        tracing::error!(%error, "ZenClash startup failed");
        eprintln!("ZenClash 启动失败：{error}");
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("zenclash-io")
        .build()?;
    let _runtime_guard = runtime.enter();
    let core_kind = selected_core_kind()?;
    let controlled_config_store = ControlledConfigStore::discover()?;
    let override_paths = YamlOverrideStore::discover()?.load_enabled_paths()?;
    let (endpoint, mihomo_process, profile_path) = bootstrap_core(
        &runtime,
        core_kind,
        &controlled_config_store,
        &override_paths,
    )?;
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
    let logs = LogMonitor::start(runtime.handle(), endpoint, "debug");
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
            },
            cx,
        );
        cx.activate(true);
    });
    Ok(())
}

fn selected_core_kind() -> Result<CoreKind, Box<dyn std::error::Error>> {
    if let Ok(value) = std::env::var("ZENCLASH_CORE") {
        return Ok(value.parse()?);
    }
    match AppPreferencesStore::discover().and_then(|store| store.load()) {
        Ok(preferences) => Ok(preferences.core_kind),
        Err(error) => {
            tracing::warn!(%error, "failed to load selected core; using Mihomo");
            Ok(CoreKind::Mihomo)
        }
    }
}

fn bootstrap_core(
    runtime: &tokio::runtime::Runtime,
    core_kind: CoreKind,
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

    let discovered = match MihomoLaunchConfig::discover_for_kind(&project_root, core_kind) {
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

fn allocate_managed_controller() -> std::io::Result<String> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(format!("127.0.0.1:{port}"))
}
