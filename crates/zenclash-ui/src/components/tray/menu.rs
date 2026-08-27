use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
};

use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

use super::{EnvironmentShell, TrayCommand, TrayMenuState};

static NEXT_MENU_GENERATION: AtomicU64 = AtomicU64::new(0);

fn select_profile_command(profile: &super::TrayProfile) -> TrayCommand {
    TrayCommand::SelectProfile {
        id: profile.id.clone(),
    }
}

struct MenuAssembler {
    commands: HashMap<String, TrayCommand>,
    generation: u64,
    next_id: usize,
}

impl MenuAssembler {
    fn new() -> Self {
        Self {
            commands: HashMap::new(),
            generation: NEXT_MENU_GENERATION.fetch_add(1, Ordering::Relaxed),
            next_id: 0,
        }
    }

    fn register(&mut self, command: TrayCommand) -> String {
        let id = format!("zenclash-tray-{}-{}", self.generation, self.next_id);
        self.next_id += 1;
        self.commands.insert(id.clone(), command);
        id
    }

    fn item(&mut self, label: impl AsRef<str>, command: TrayCommand) -> MenuItem {
        MenuItem::with_id(self.register(command), label, true, None)
    }

    fn check(
        &mut self,
        label: impl AsRef<str>,
        checked: bool,
        command: TrayCommand,
    ) -> CheckMenuItem {
        CheckMenuItem::with_id(self.register(command), label, true, checked, None)
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "keeping native tray item creation in menu order makes identifiers and submenu ownership auditable"
)]
pub(super) fn build_menu(
    state: &TrayMenuState,
) -> Result<(Menu, HashMap<String, TrayCommand>), String> {
    let menu = Menu::new();
    let mut builder = MenuAssembler::new();

    let show_window = builder.item("显示主窗口", TrayCommand::ShowWindow);
    let floating = builder.item(
        if state.floating_visible {
            "隐藏悬浮窗"
        } else {
            "显示悬浮窗"
        },
        TrayCommand::ToggleFloatingWindow,
    );
    let rule = builder.check(
        "规则模式",
        state.mode.eq_ignore_ascii_case("rule"),
        TrayCommand::SetRuleMode,
    );
    let global = builder.check(
        "全局模式",
        state.mode.eq_ignore_ascii_case("global"),
        TrayCommand::SetGlobalMode,
    );
    let direct = builder.check(
        "直连模式",
        state.mode.eq_ignore_ascii_case("direct"),
        TrayCommand::SetDirectMode,
    );
    let separator_1 = PredefinedMenuItem::separator();
    let system_proxy = builder.check(
        "系统代理",
        state.system_proxy,
        TrayCommand::SetSystemProxy {
            enabled: !state.system_proxy,
            port: state.mixed_port,
        },
    );
    let tun = builder.check("TUN", state.tun, TrayCommand::SetTun(!state.tun));

    menu.append_items(&[
        &show_window,
        &floating,
        &rule,
        &global,
        &direct,
        &separator_1,
        &system_proxy,
        &tun,
    ])
    .map_err(|error| error.to_string())?;

    if !state.groups.is_empty() {
        let separator = PredefinedMenuItem::separator();
        menu.append(&separator).map_err(|error| error.to_string())?;
        for group in &state.groups {
            let label = if group.now.is_empty() {
                group.name.clone()
            } else {
                format!("{} · {}", group.name, group.now)
            };
            let submenu = Submenu::new(label, true);
            let delay_test = builder.item(
                "测试本组延迟",
                TrayCommand::TestGroup {
                    group: group.name.clone(),
                    proxies: group
                        .proxies
                        .iter()
                        .map(|proxy| proxy.name.clone())
                        .collect(),
                    test_url: group.test_url.clone(),
                },
            );
            let separator = PredefinedMenuItem::separator();
            submenu
                .append_items(&[&delay_test, &separator])
                .map_err(|error| error.to_string())?;
            for proxy in &group.proxies {
                let delay = match proxy.delay {
                    Some(0) => "  （超时）".to_owned(),
                    Some(delay) => format!("  （{delay} ms）"),
                    None => String::new(),
                };
                let item = builder.check(
                    format!("{}{delay}", proxy.name),
                    proxy.name == group.now,
                    TrayCommand::SelectProxy {
                        group: group.name.clone(),
                        proxy: proxy.name.clone(),
                    },
                );
                submenu.append(&item).map_err(|error| error.to_string())?;
            }
            menu.append(&submenu).map_err(|error| error.to_string())?;
        }
    }

    let separator_2 = PredefinedMenuItem::separator();
    menu.append(&separator_2)
        .map_err(|error| error.to_string())?;
    let profiles = Submenu::new("配置文件", true);
    for profile in &state.profiles {
        let item = builder.check(
            &profile.name,
            profile.active,
            select_profile_command(profile),
        );
        profiles.append(&item).map_err(|error| error.to_string())?;
    }
    if state.profiles.is_empty() {
        let current_profile = builder.check(
            if state.profile_name.is_empty() {
                "当前配置"
            } else {
                state.profile_name.as_str()
            },
            true,
            TrayCommand::OpenProfiles,
        );
        profiles
            .append(&current_profile)
            .map_err(|error| error.to_string())?;
    }
    let profile_separator = PredefinedMenuItem::separator();
    let open_profiles = builder.item("打开订阅管理…", TrayCommand::OpenProfiles);
    profiles
        .append_items(&[&profile_separator, &open_profiles])
        .map_err(|error| error.to_string())?;
    menu.append(&profiles).map_err(|error| error.to_string())?;

    if !state.directories.is_empty() {
        let directories = Submenu::new("打开目录", true);
        for (label, path) in &state.directories {
            let item = builder.item(label, TrayCommand::OpenDirectory(path.clone()));
            directories
                .append(&item)
                .map_err(|error| error.to_string())?;
        }
        menu.append(&directories)
            .map_err(|error| error.to_string())?;
    }

    if state.mixed_port > 0 {
        let copy_environment = Submenu::new("复制代理环境变量", true);
        for shell in EnvironmentShell::ALL {
            let item = builder.item(
                shell.label(),
                TrayCommand::CopyEnvironment {
                    port: state.mixed_port,
                    shell,
                },
            );
            copy_environment
                .append(&item)
                .map_err(|error| error.to_string())?;
        }
        menu.append(&copy_environment)
            .map_err(|error| error.to_string())?;
    }

    let separator_3 = PredefinedMenuItem::separator();
    let light_mode = builder.item("轻量模式（隐藏窗口）", TrayCommand::LightMode);
    let restart = builder.item("重新启动 ZenClash", TrayCommand::Restart);
    let quit = builder.item("退出 ZenClash", TrayCommand::Quit);
    menu.append_items(&[&separator_3, &light_mode, &restart, &quit])
        .map_err(|error| error.to_string())?;

    Ok((menu, builder.commands))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_registration_produces_unique_ids() {
        let mut builder = MenuAssembler::new();

        let first = builder.register(TrayCommand::ShowWindow);
        let second = builder.register(TrayCommand::Quit);

        assert_ne!(first, second);
    }

    #[test]
    fn menu_rebuild_uses_a_new_identifier_generation() {
        let first = MenuAssembler::new().register(TrayCommand::ShowWindow);
        let second = MenuAssembler::new().register(TrayCommand::ShowWindow);

        assert_ne!(first, second);
    }

    #[test]
    fn profile_entries_create_real_selection_commands() {
        let profile = super::super::TrayProfile {
            id: "airport".into(),
            name: "主订阅".into(),
            active: true,
        };

        let command = select_profile_command(&profile);

        assert!(matches!(command, TrayCommand::SelectProfile { id } if id == "airport"));
    }
}
