use gpui::{AnyWindowHandle, AppContext, Context, Entity, EventEmitter, Subscription, Window};
use zenclash_core::{
    ControlledConfigStore, ControllerCatalog, ControllerEntry, ControllerStore, MihomoClient,
    MihomoEndpoint, ProfileCatalog, ProfileStore, SsidRules,
};

mod remote;
mod view;
mod wifi;
use gpui_component::input::InputState;
use remote::RemoteWorkspace;
pub(super) use wifi::read_ssid;

pub(super) enum TargetEvent {
    Local,
    Remote,
}

pub(super) struct ControllersPage {
    runtime: tokio::runtime::Handle,
    controlled: ControlledConfigStore,
    window: AnyWindowHandle,
    store: Option<ControllerStore>,
    catalog: ControllerCatalog,
    remote: Option<RemoteWorkspace>,
    editing: Option<String>,
    name: Entity<InputState>,
    address: Entity<InputState>,
    secret: Entity<InputState>,
    manager: bool,
    busy: bool,
    error: Option<String>,
    notice: Option<String>,
    generation: u64,
    closed: bool,
    pub(super) ssid_rules: Option<SsidRules>,
    profiles: ProfileCatalog,
    ssid: Entity<InputState>,
    ssid_profile: Option<String>,
    pub(super) wifi: wifi::WifiMonitor,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TargetEvent> for ControllersPage {}

impl ControllersPage {
    pub(super) fn new(
        runtime: tokio::runtime::Handle,
        controlled: ControlledConfigStore,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let handle = window.window_handle();
        let mut input = |cx: &mut Context<Self>, placeholder: &str, masked| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(zenclash_i18n::text(placeholder))
                    .masked(masked)
            })
        };
        let mut this = Self {
            runtime,
            controlled,
            window: handle,
            store: None,
            catalog: ControllerCatalog::default(),
            remote: None,
            editing: None,
            name: input(cx, "controllers.name", false),
            address: input(cx, "controllers.address", false),
            secret: input(cx, "controllers.secret", true),
            manager: true,
            busy: true,
            error: None,
            notice: None,
            generation: 0,
            closed: false,
            ssid_rules: None,
            profiles: ProfileCatalog::default(),
            ssid: input(cx, "ssid.name", false),
            ssid_profile: None,
            wifi: wifi::WifiMonitor::default(),
            _subscriptions: Vec::new(),
        };
        this.load(cx);
        this.start_updates(cx);
        this
    }

    pub(super) fn target_label(&self) -> String {
        self.remote.as_ref().map_or_else(
            || zenclash_i18n::text("controllers.local"),
            |remote| remote.entry.name.clone(),
        )
    }

    pub(super) fn is_remote(&self) -> bool {
        self.remote.is_some()
    }

    pub(super) fn automation_ready(&self) -> bool {
        !self.busy && !self.closed
    }

    pub(super) fn report_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = Some(error);
        cx.notify();
    }

    pub(super) fn refresh_localized_placeholders(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for (input, key) in [
            (&self.name, "controllers.name"),
            (&self.address, "controllers.address"),
            (&self.secret, "controllers.secret"),
            (&self.ssid, "ssid.name"),
        ] {
            input.update(cx, |input, cx| {
                input.set_placeholder(zenclash_i18n::text(key), window, cx)
            });
        }
        if let Some(remote) = &self.remote {
            remote.pages.update(cx, |page, cx| {
                page.refresh_localized_placeholders(window, cx)
            });
            remote.proxies.update(cx, |_, cx| cx.notify());
        }
        cx.notify();
    }

    pub(super) fn shutdown(&mut self) {
        self.closed = true;
        self.remote = None;
        self.wifi.stop();
    }

    pub(super) fn show_manager(&mut self, cx: &mut Context<Self>) {
        self.manager = true;
        if let Some(remote) = &self.remote {
            remote.proxies.update(cx, |page, _| page.suspend());
            remote
                .pages
                .update(cx, |page, cx| page.set_presented(false, cx));
        }
        let task = self
            .runtime
            .spawn_blocking(|| ProfileStore::discover().and_then(|store| store.load()));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.closed {
                    return;
                }
                match result {
                    Ok(Ok(profiles)) => this.profiles = profiles,
                    Ok(Err(error)) => this.error = Some(error.to_string()),
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let task = self.runtime.spawn_blocking(|| {
            let store = ControllerStore::discover().map_err(store_error)?;
            let catalog = store.load().map_err(store_error)?;
            let rules = store.load_ssid().map_err(store_error);
            let profiles = ProfileStore::discover()
                .and_then(|store| store.load())
                .map_err(|error| error.to_string());
            Ok::<_, String>((store, catalog, rules, profiles))
        });
        let handle = self.window;
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = cx.update_window(handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    if this.closed {
                        return;
                    }
                    this.busy = false;
                    match result {
                        Ok((store, catalog, rules, profiles)) => {
                            this.store = Some(store);
                            let restore = catalog.active.clone();
                            this.catalog = catalog;
                            match rules {
                                Ok(rules) => {
                                    if rules.enabled {
                                        this.wifi.start(false);
                                    }
                                    this.ssid_rules = Some(rules);
                                }
                                Err(error) => this.error = Some(error),
                            }
                            match profiles {
                                Ok(profiles) => this.profiles = profiles,
                                Err(error) => this.error = Some(error),
                            }
                            if let Some(id) = restore {
                                this.connect(&id, window, cx);
                            }
                        }
                        Err(error) => this.error = Some(error),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn edit(
        &mut self,
        entry: Option<ControllerEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editing = entry.as_ref().map(|entry| entry.id.clone());
        let (name, address, secret) = entry.map_or_else(
            || (String::new(), String::new(), String::new()),
            |entry| (entry.name, entry.endpoint.controller, entry.endpoint.secret),
        );
        for (input, value) in [
            (&self.name, name),
            (&self.address, address),
            (&self.secret, secret),
        ] {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        self.manager = true;
        if let Some(remote) = &self.remote {
            remote
                .pages
                .update(cx, |page, cx| page.set_presented(false, cx));
            remote.proxies.update(cx, |page, _| page.suspend());
        }
        self.error = None;
        self.name.update(cx, |input, cx| {
            use gpui::Focusable;
            input.focus_handle(cx).focus(window);
        });
        cx.notify();
    }

    fn save_entry(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let result = ControllerEntry::new(
            self.name.read(cx).value().to_string(),
            MihomoEndpoint::new(
                self.address.read(cx).value().to_string(),
                self.secret.read(cx).value().to_string(),
            ),
        );
        let mut entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                self.error = Some(store_error(error));
                cx.notify();
                return;
            }
        };
        if let Some(id) = &self.editing {
            entry.id.clone_from(id);
        }
        self.editing = Some(entry.id.clone());
        let mut catalog = self.catalog.clone();
        if let Some(existing) = catalog
            .entries
            .iter_mut()
            .find(|existing| existing.id == entry.id)
        {
            *existing = entry.clone();
        } else {
            catalog.entries.push(entry.clone());
        }
        // The live target remains immutable. Editing it switches through the same probe/commit path.
        if self
            .remote
            .as_ref()
            .is_some_and(|remote| remote.entry.id == entry.id)
        {
            self.probe_and_commit(entry, catalog, cx);
        } else {
            self.persist(catalog, false, cx);
        }
    }

    fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let mut catalog = self.catalog.clone();
        catalog.entries.retain(|entry| entry.id != id);
        let local = catalog.active.as_deref() == Some(id);
        if local {
            catalog.active = None;
        }
        self.persist(catalog, local, cx);
    }

    pub(super) fn local(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.store.is_none() {
            cx.emit(TargetEvent::Local);
            return;
        }
        let mut catalog = self.catalog.clone();
        catalog.active = None;
        self.persist(catalog, true, cx);
    }

    fn persist(&mut self, catalog: ControllerCatalog, local: bool, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        self.busy = true;
        self.error = None;
        let task = self.runtime.spawn_blocking(move || {
            store.save(&catalog).map_err(store_error)?;
            Ok::<_, String>(catalog)
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if this.closed {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(catalog) => {
                        this.catalog = catalog;
                        this.notice = Some(zenclash_i18n::text("controllers.saved"));
                        if local {
                            this.remote = None;
                            this.generation = this.generation.wrapping_add(1);
                            cx.emit(TargetEvent::Local);
                        }
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn connect(&mut self, id: &str, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(entry) = self
            .catalog
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
        else {
            return;
        };
        let mut catalog = self.catalog.clone();
        catalog.active = Some(entry.id.clone());
        self.probe_and_commit(entry, catalog, cx);
    }

    fn probe_and_commit(
        &mut self,
        entry: ControllerEntry,
        catalog: ControllerCatalog,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.store.clone() else {
            return;
        };
        self.busy = true;
        self.error = None;
        self.notice = None;
        let runtime = self.runtime.clone();
        let task = runtime.spawn(prepare_controller(entry, catalog, store));
        let handle = self.window;
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = cx.update_window(handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    if this.closed {
                        return;
                    }
                    this.busy = false;
                    match result {
                        Ok((entry, catalog, client, mode)) => {
                            this.remote = Some(RemoteWorkspace::new(
                                entry,
                                client,
                                &mode,
                                &this.runtime,
                                this.controlled.clone(),
                                window,
                                cx,
                            ));
                            this.catalog = catalog;
                            this.manager = false;
                            this.generation = this.generation.wrapping_add(1);
                            cx.emit(TargetEvent::Remote);
                        }
                        Err(error) => this.error = Some(error),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
        cx.notify();
    }

    fn test(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(entry) = self.catalog.entries.iter().find(|entry| entry.id == id) else {
            return;
        };
        let endpoint = entry.endpoint.clone();
        self.busy = true;
        self.error = None;
        let task = self.runtime.spawn(async move {
            let client = MihomoClient::new(endpoint)
                .map_err(|_| zenclash_i18n::text("controllers.invalid_endpoint"))?;
            probe(&client).await
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if this.closed {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(_) => this.notice = Some(zenclash_i18n::text("controllers.reachable")),
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn start_updates(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        })
        .detach();
    }
}

async fn prepare_controller(
    entry: ControllerEntry,
    catalog: ControllerCatalog,
    store: ControllerStore,
) -> Result<(ControllerEntry, ControllerCatalog, MihomoClient, String), String> {
    let client = MihomoClient::new(entry.endpoint.clone())
        .map_err(|_| zenclash_i18n::text("controllers.invalid_endpoint"))?;
    let mode = probe(&client).await?;
    let saved = catalog.clone();
    tokio::task::spawn_blocking(move || store.save(&saved))
        .await
        .map_err(|error| error.to_string())?
        .map_err(store_error)?;
    Ok((entry, catalog, client, mode))
}

async fn probe(client: &MihomoClient) -> Result<String, String> {
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        client.version().await?;
        client.runtime_config().await.map(|config| config.mode)
    })
    .await
    .map_err(|_| zenclash_i18n::text("controllers.timeout"))?
    .map_err(|_| zenclash_i18n::text("controllers.probe_failed"))
}

fn store_error(error: zenclash_core::ContextStoreError) -> String {
    match error {
        zenclash_core::ContextStoreError::Invalid(key)
            if key.starts_with("controllers.") || key.starts_with("ssid.") =>
        {
            zenclash_i18n::text(&key)
        }
        other => {
            zenclash_i18n::text_with("controllers.store_failed", &[("error", other.to_string())])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn server(
        responses: Vec<(&'static str, &'static str)>,
    ) -> (MihomoEndpoint, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let task = thread::spawn(move || {
            responses.into_iter().map(|(status, body)| {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                let mut request = [0u8; 2048];
                let len = stream.read(&mut request).unwrap();
                write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                String::from_utf8_lossy(&request[..len]).lines().next().unwrap().to_owned()
            }).collect()
        });
        (MihomoEndpoint::new(format!("http://{address}"), ""), task)
    }

    #[tokio::test]
    async fn failed_probe_preserves_the_previous_saved_controller() {
        let (endpoint, server) = server(vec![(
            "401 Unauthorized",
            r#"{"message":"private server detail"}"#,
        )]);
        let old = ControllerEntry::new("old".into(), MihomoEndpoint::default()).unwrap();
        let new = ControllerEntry::new("new".into(), endpoint).unwrap();
        let root = std::env::temp_dir().join(format!("zenclash-switch-{}", old.id));
        let store = ControllerStore::new(root.clone());
        let original = ControllerCatalog {
            active: Some(old.id.clone()),
            entries: vec![old, new.clone()],
            ..Default::default()
        };
        store.save(&original).unwrap();
        let mut candidate = original.clone();
        candidate.active = Some(new.id.clone());
        let error = prepare_controller(new, candidate, store.clone())
            .await
            .err()
            .unwrap();
        assert!(!error.contains("private server detail"));
        assert_eq!(store.load().unwrap(), original);
        assert_eq!(server.join().unwrap(), ["GET /version HTTP/1.1"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn successful_switch_probes_version_and_config_before_committing_target() {
        let (endpoint, server) = server(vec![
            ("200 OK", r#"{"version":"test"}"#),
            ("200 OK", r#"{"mode":"global"}"#),
        ]);
        let entry = ControllerEntry::new("router".into(), endpoint).unwrap();
        let root = std::env::temp_dir().join(format!("zenclash-switch-{}", entry.id));
        let store = ControllerStore::new(root.clone());
        let catalog = ControllerCatalog {
            active: Some(entry.id.clone()),
            entries: vec![entry.clone()],
            ..Default::default()
        };
        let (_, saved, _, mode) = prepare_controller(entry, catalog.clone(), store.clone())
            .await
            .unwrap();
        assert_eq!((saved, mode), (catalog.clone(), "global".into()));
        assert_eq!(store.load().unwrap(), catalog);
        assert_eq!(
            server.join().unwrap(),
            ["GET /version HTTP/1.1", "GET /configs HTTP/1.1"]
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
