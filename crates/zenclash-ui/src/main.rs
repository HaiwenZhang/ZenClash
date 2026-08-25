#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! Native `ZenClash` executable bootstrap and managed Mihomo discovery.

use std::{net::TcpListener, path::PathBuf, sync::Arc, time::Duration};

use gpui::Application;
use gpui_component_assets::Assets;
use zenclash_core::{
    LogMonitor, MihomoClient, MihomoEndpoint, MihomoLaunchConfig, MihomoProcess, ProfileStore,
    TrafficMonitor,
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
    let (endpoint, mihomo_process, profile_path) = bootstrap_mihomo(&runtime)?;
    let client = MihomoClient::new(endpoint.clone())?;
    let traffic = TrafficMonitor::start(runtime.handle(), endpoint.clone());
    let logs = LogMonitor::start(runtime.handle(), endpoint, "debug");
    let runtime_handle = runtime.handle().clone();

    Application::new().with_assets(Assets).run(move |cx| {
        app::init(cx);
        app::create_main_window(
            app::AppServices {
                client,
                traffic_monitor: traffic,
                log_monitor: logs,
                mihomo_process,
                profile_path,
                runtime: runtime_handle,
            },
            cx,
        );
        cx.activate(true);
    });
    Ok(())
}

fn bootstrap_mihomo(
    runtime: &tokio::runtime::Runtime,
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

    let discovered = match MihomoLaunchConfig::discover(&project_root) {
        Ok(launch) => launch,
        Err(error) => {
            tracing::warn!(%error, "Mihomo binary discovery failed; using controller-only mode");
            let profile_path =
                selected_profile.unwrap_or_else(|| project_root.join("examples/19facdf022b.yaml"));
            return Ok((MihomoEndpoint::from_env(), None, Some(profile_path)));
        }
    };
    let profile_path = selected_profile.unwrap_or_else(|| discovered.config_file.clone());
    let mut launch =
        match MihomoLaunchConfig::new(discovered.binary, &profile_path, discovered.home_dir) {
            Ok(launch) => launch,
            Err(error) => {
                tracing::warn!(%error, "active profile is invalid; using controller-only mode");
                return Ok((MihomoEndpoint::from_env(), None, Some(profile_path)));
            }
        };
    match allocate_managed_controller() {
        Ok(controller) => launch = launch.with_controller_override(controller),
        Err(error) => {
            tracing::warn!(%error, "failed to allocate an isolated Mihomo controller port");
        }
    }
    let endpoint = launch.endpoint.clone();

    match MihomoProcess::spawn(launch.clone()) {
        Ok(process) => {
            match runtime.block_on(process.wait_until_ready(Duration::from_secs(20))) {
                Ok(()) => {
                    tracing::info!(controller = %endpoint.controller, "managed Mihomo is ready");
                }
                Err(error) => {
                    tracing::error!(%error, "managed Mihomo failed to become ready");
                    for line in process.snapshot().logs.iter().rev().take(12).rev() {
                        tracing::error!("{line}");
                    }
                    if let Err(stop_error) = process.stop() {
                        tracing::error!(%stop_error, "failed to stop unready managed Mihomo");
                    }
                }
            }
            Ok((endpoint, Some(process), Some(launch.config_file)))
        }
        Err(error) => {
            tracing::error!(%error, "failed to start managed Mihomo");
            Ok((endpoint, None, Some(launch.config_file)))
        }
    }
}

fn allocate_managed_controller() -> std::io::Result<String> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(format!("127.0.0.1:{port}"))
}
