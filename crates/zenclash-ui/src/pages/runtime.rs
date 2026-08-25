use std::{
    collections::{HashSet, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use gpui::{
    div, prelude::FluentBuilder, px, App, AppContext, Context, Entity, EventEmitter, Focusable,
    InteractiveElement, IntoElement, ParentElement, PathPromptOptions, Render, Styled, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    scroll::ScrollableElement,
    switch::Switch,
    v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable,
};
use serde_json::{json, Value};
use zenclash_core::{
    format_speed, merge_profile_overrides, ConnectionsSnapshot, LogMonitor, MihomoClient,
    MihomoProcess, ProfileCatalog, ProfileStore, ProviderCatalog, RuleCatalog, RuntimeConfig,
    SubStoreClient, SubStoreItem, SubStoreSnapshot, SystemNetworkSnapshot, SystemProxyManager,
    SystemProxyStatus, TrafficMonitor, VersionInfo,
};

use crate::app::{HideTrafficIcon, SetDarkTheme, SetLightTheme, ShowTrafficIcon};

use super::Page;

mod common;
mod connections;
mod dns;
mod lifecycle;
mod loader;
mod logs;
mod mihomo;
mod network;
mod overrides;
mod profiles;
mod resources;
mod rules;
mod settings;
mod sniffer;
mod state;
mod substore;
mod system_proxy;
mod traffic;
mod tun;
mod view;

use common::{
    compact_text, empty_dash, empty_state, format_bytes, format_port, format_profile_age,
    format_proxy, info_row, message_banner, metric, normalized_fraction, setting_card,
    setting_switch, yes_no,
};
use loader::load_page;
use state::{PageTaskToken, RuntimeData};

/// Stateful GPUI page host for Mihomo runtime, configuration, and diagnostics.
pub struct RuntimePage {
    page: Page,
    client: MihomoClient,
    runtime: tokio::runtime::Handle,
    traffic_monitor: Arc<TrafficMonitor>,
    log_monitor: Arc<LogMonitor>,
    process: Option<Arc<MihomoProcess>>,
    profile_path: Option<PathBuf>,
    profile_store: Option<ProfileStore>,
    profile_catalog: ProfileCatalog,
    subscription_name: Entity<InputState>,
    subscription_url: Entity<InputState>,
    subscription_user_agent: Entity<InputState>,
    override_paths: Vec<PathBuf>,
    data: RuntimeData,
    traffic_samples: VecDeque<u64>,
    navigation_generation: u64,
    load_generation: u64,
    loading: bool,
    mutating: bool,
    closing_connections: HashSet<String>,
    error: Option<String>,
    notice: Option<String>,
    focus_handle: gpui::FocusHandle,
}

/// Runtime services shared by the native Mihomo management pages.
pub struct RuntimePageServices {
    /// Typed Mihomo controller client.
    pub client: MihomoClient,
    /// Tokio runtime used for controller and filesystem work.
    pub runtime: tokio::runtime::Handle,
    /// Shared live traffic stream.
    pub traffic_monitor: Arc<TrafficMonitor>,
    /// Shared live log stream.
    pub log_monitor: Arc<LogMonitor>,
    /// Managed Mihomo process, when `ZenClash` launched the core.
    pub process: Option<Arc<MihomoProcess>>,
    /// Active YAML path used by the managed core.
    pub profile_path: Option<PathBuf>,
}

/// Event emitted after a managed profile becomes the active Mihomo config.
#[derive(Clone, Debug)]
pub struct ProfileActivated {
    /// Managed YAML path accepted by Mihomo.
    pub path: PathBuf,
}

impl EventEmitter<ProfileActivated> for RuntimePage {}
