use gpui::{AppContext, Entity, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};
use zenclash_core::{WebDavBackup, WebDavSettings, WebDavSettingsStore};

use super::super::Context;

mod actions;
mod view;
mod workflow;

pub(in crate::pages::runtime) struct WebDavUiState {
    store: Option<WebDavSettingsStore>,
    url: Entity<InputState>,
    directory: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    max_backups: Entity<InputState>,
    backup_cron: Entity<InputState>,
    accept_invalid_certificates: bool,
    backups: Vec<WebDavBackup>,
    verified: bool,
    dirty: bool,
    _input_subscriptions: Vec<Subscription>,
}

impl WebDavUiState {
    pub(in crate::pages::runtime) fn discover(
        window: &mut Window,
        cx: &mut Context<super::super::RuntimePage>,
    ) -> (Self, Option<String>) {
        let (store, settings, error) = match WebDavSettingsStore::discover() {
            Ok(store) => match store.load() {
                Ok(settings) => (Some(store), settings, None),
                Err(error) => (
                    Some(store),
                    WebDavSettings::default(),
                    Some(format!("WebDAV 设置读取失败：{error}")),
                ),
            },
            Err(error) => (
                None,
                WebDavSettings::default(),
                Some(format!("WebDAV 设置目录不可用：{error}")),
            ),
        };
        let url = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.url)
                .placeholder("https://dav.example.com/remote.php/dav/files/user")
        });
        let directory = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.directory)
                .placeholder("zenclash")
        });
        let username = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.username)
                .placeholder("可选用户名")
        });
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.password)
                .placeholder("可选密码或应用密码")
                .masked(true)
        });
        let max_backups = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.max_backups.to_string())
                .placeholder("0 表示不限")
        });
        let backup_cron = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.backup_cron)
                .placeholder("30 3 * * *")
        });
        let input_subscriptions = [
            &url,
            &directory,
            &username,
            &password,
            &max_backups,
            &backup_cron,
        ]
        .into_iter()
        .map(|input| {
            cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.webdav.verified = false;
                    this.webdav.dirty = true;
                    cx.notify();
                }
            })
        })
        .collect();
        let state = Self {
            store,
            url,
            directory,
            username,
            password,
            max_backups,
            backup_cron,
            accept_invalid_certificates: settings.accept_invalid_certificates,
            backups: Vec::new(),
            verified: false,
            dirty: false,
            _input_subscriptions: input_subscriptions,
        };
        (state, error)
    }

    fn settings(&self, cx: &gpui::App) -> Result<WebDavSettings, String> {
        let max_backups = parse_max_backups(&input_text(&self.max_backups, cx))?;
        Ok(WebDavSettings {
            version: 1,
            url: input_text(&self.url, cx).trim().to_owned(),
            directory: input_text(&self.directory, cx).trim().to_owned(),
            username: input_text(&self.username, cx).trim().to_owned(),
            password: input_text(&self.password, cx),
            max_backups,
            accept_invalid_certificates: self.accept_invalid_certificates,
            backup_cron: input_text(&self.backup_cron, cx).trim().to_owned(),
        })
    }

    fn store(&self) -> Result<WebDavSettingsStore, String> {
        self.store
            .clone()
            .ok_or_else(|| "WebDAV 设置目录不可用，请检查应用数据目录权限".into())
    }
}

fn input_text(input: &Entity<InputState>, cx: &gpui::App) -> String {
    input.read(cx).text().to_string()
}

fn parse_max_backups(input: &str) -> Result<usize, String> {
    if input.trim().is_empty() {
        return Ok(0);
    }
    input
        .trim()
        .parse::<usize>()
        .map_err(|error| format!("保留份数必须是 0 到 100 的整数：{error}"))
}

#[cfg(test)]
mod tests {
    use super::parse_max_backups;

    #[test]
    fn blank_retention_means_unlimited() {
        assert_eq!(parse_max_backups("  ").unwrap(), 0);
    }

    #[test]
    fn invalid_retention_is_reported() {
        assert!(parse_max_backups("daily").unwrap_err().contains("整数"));
    }
}
