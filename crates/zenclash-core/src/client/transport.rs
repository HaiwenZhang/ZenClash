use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use futures_util::StreamExt;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;
use zenclash_service_integration::{NativeSocket, ServiceSession};

use super::{MihomoError, MihomoResult};
use crate::{
    MihomoEndpoint, MihomoProcess,
    owned_core::OwnedCore,
    websocket::{MihomoSocket, connect_stream},
};

#[derive(Clone)]
pub(crate) enum ControllerBackend {
    Direct(MihomoEndpoint),
    Local(Arc<MihomoProcess>),
    Service {
        runtime: Arc<crate::service_runtime_session::ServiceRuntimeSession>,
    },
}

#[derive(Clone)]
pub(crate) struct BindingSnapshot {
    pub generation: u64,
    pub kind: crate::CoreKind,
    pub backend: ControllerBackend,
}

impl BindingSnapshot {
    pub(crate) fn is_service(&self) -> bool {
        matches!(self.backend, ControllerBackend::Service { .. })
    }

    pub(crate) fn owned_core(&self) -> Option<OwnedCore> {
        match &self.backend {
            ControllerBackend::Direct(_) => None,
            ControllerBackend::Local(process) => Some(OwnedCore::Local(process.clone())),
            ControllerBackend::Service { runtime } => Some(OwnedCore::Service(runtime.clone())),
        }
    }

    pub(crate) fn endpoint(&self) -> Option<MihomoEndpoint> {
        match &self.backend {
            ControllerBackend::Direct(endpoint) => Some(endpoint.clone()),
            ControllerBackend::Local(process) => Some(process.endpoint().clone()),
            ControllerBackend::Service { .. } => None,
        }
    }
}

pub(crate) struct ControllerBinding {
    updates: watch::Sender<BindingSnapshot>,
    closing: AtomicBool,
    used: AtomicBool,
}

impl ControllerBinding {
    pub(crate) fn close_admission(&self) {
        self.closing.store(true, Ordering::Release);
    }
    pub(crate) fn ensure_open(&self) -> MihomoResult<()> {
        if self.closing.load(Ordering::Acquire) {
            Err(MihomoError::Process(zenclash_i18n::text(
                "automatic.core_paused",
            )))
        } else {
            Ok(())
        }
    }

    pub(crate) fn descriptor(&self) -> crate::owned_core::CoreRuntimeDescriptor {
        use crate::owned_core::{CoreRuntimeBackend, CoreRuntimeDescriptor};
        // Borrowing avoids giving a renderer the last Arc of a retired process.
        let binding = self.updates.borrow();
        let (backend, kind, binary, config_file, home_dir) = match &binding.backend {
            ControllerBackend::Local(process) => {
                let launch = process.launch_config();
                (
                    CoreRuntimeBackend::Local,
                    launch.kind,
                    Some(launch.binary.clone()),
                    Some(launch.config_file.clone()),
                    Some(launch.home_dir.clone()),
                )
            }
            ControllerBackend::Service { .. } => (
                CoreRuntimeBackend::Service,
                crate::CoreKind::Mihomo,
                None,
                None,
                None,
            ),
            ControllerBackend::Direct(_) => {
                (CoreRuntimeBackend::Direct, binding.kind, None, None, None)
            }
        };
        CoreRuntimeDescriptor {
            binding_generation: binding.generation,
            backend,
            kind,
            binary,
            config_file,
            home_dir,
        }
    }

    pub(crate) fn local_recovery_launch(&self) -> Option<crate::MihomoLaunchConfig> {
        let binding = self.updates.borrow();
        match &binding.backend {
            ControllerBackend::Local(process) => Some(process.launch_config().clone()),
            ControllerBackend::Service { runtime } => runtime.local_launch().cloned(),
            ControllerBackend::Direct(_) => None,
        }
    }
    pub(crate) fn direct(endpoint: MihomoEndpoint) -> Arc<Self> {
        let (updates, _) = watch::channel(BindingSnapshot {
            generation: 0,
            kind: crate::CoreKind::Mihomo,
            backend: ControllerBackend::Direct(endpoint),
        });
        Arc::new(Self {
            updates,
            closing: AtomicBool::new(false),
            used: AtomicBool::new(false),
        })
    }
    pub(crate) fn service_binding(
        client: Arc<ServiceSession>,
        source_home: std::path::PathBuf,
    ) -> Arc<Self> {
        Self::service_binding_with_core(client, source_home, None)
    }
    pub(crate) fn service_binding_with_core(
        client: Arc<ServiceSession>,
        source_home: std::path::PathBuf,
        core_source: Option<std::path::PathBuf>,
    ) -> Arc<Self> {
        let (updates, _) = watch::channel(BindingSnapshot {
            generation: 0,
            kind: crate::CoreKind::Mihomo,
            backend: ControllerBackend::Service {
                runtime: crate::service_runtime_session::ServiceRuntimeSession::with_core_source(
                    client,
                    source_home,
                    core_source,
                ),
            },
        });
        Arc::new(Self {
            updates,
            closing: AtomicBool::new(false),
            used: AtomicBool::new(false),
        })
    }

    pub(crate) fn process_binding(process: Arc<MihomoProcess>) -> Arc<Self> {
        let (updates, _) = watch::channel(BindingSnapshot {
            generation: 0,
            kind: process.kind(),
            backend: ControllerBackend::Local(process),
        });
        Arc::new(Self {
            updates,
            closing: AtomicBool::new(false),
            used: AtomicBool::new(false),
        })
    }

    pub(crate) fn snapshot(&self) -> BindingSnapshot {
        self.updates.borrow().clone()
    }
    pub(crate) fn mark_used(&self) {
        self.used.store(true, Ordering::Release);
    }
    pub(crate) fn initialize_kind(&mut self, kind: crate::CoreKind) -> MihomoResult<()> {
        let binding = self.updates.borrow();
        if binding.kind == kind {
            return Ok(());
        }
        if self.used.load(Ordering::Acquire)
            || binding.generation != 0
            || !matches!(binding.backend, ControllerBackend::Direct(_))
        {
            return Err(MihomoError::InvalidInput(zenclash_i18n::text(
                "core_page.errors.runtime_kind_locked",
            )));
        }
        drop(binding);
        self.updates.send_modify(|binding| binding.kind = kind);
        Ok(())
    }
    pub(crate) fn generation(&self) -> u64 {
        self.updates.borrow().generation
    }
    pub(crate) fn subscribe(&self) -> watch::Receiver<BindingSnapshot> {
        self.updates.subscribe()
    }
    pub(crate) fn is_current(&self, generation: u64) -> bool {
        self.updates.borrow().generation == generation
    }
    pub(crate) fn endpoint(&self) -> Option<MihomoEndpoint> {
        let binding = self.updates.borrow();
        match &binding.backend {
            ControllerBackend::Direct(endpoint) => Some(endpoint.clone()),
            ControllerBackend::Local(process) => Some(process.endpoint().clone()),
            ControllerBackend::Service { .. } => None,
        }
    }
    pub(crate) fn runtime(
        &self,
    ) -> Option<Arc<crate::service_runtime_session::ServiceRuntimeSession>> {
        match self.snapshot().owned_core() {
            Some(OwnedCore::Service(runtime)) => Some(runtime),
            Some(OwnedCore::Local(_)) | None => None,
        }
    }
    pub(crate) fn replace(&self, backend: ControllerBackend) -> MihomoResult<BindingSnapshot> {
        let mut retired = None;
        self.updates.send_if_modified(|binding| {
            let Some(generation) = binding.generation.checked_add(1) else {
                return false;
            };
            retired = Some(std::mem::replace(
                binding,
                BindingSnapshot {
                    generation,
                    kind: match &backend {
                        ControllerBackend::Local(process) => process.kind(),
                        ControllerBackend::Service { .. } => crate::CoreKind::Mihomo,
                        ControllerBackend::Direct(_) => binding.kind,
                    },
                    backend,
                },
            ));
            true
        });
        retired.ok_or_else(|| MihomoError::Process("Controller generation exhausted".into()))
    }

    pub(crate) async fn connect(
        self: &Arc<Self>,
        path: &str,
        query: &[(&str, &str)],
        timeout: &str,
    ) -> Result<ControllerStream, String> {
        let binding = self.snapshot();
        self.connect_pinned(&binding, path, query, timeout).await
    }

    pub(crate) async fn connect_pinned(
        self: &Arc<Self>,
        binding: &BindingSnapshot,
        path: &str,
        query: &[(&str, &str)],
        timeout: &str,
    ) -> Result<ControllerStream, String> {
        if !self.is_current(binding.generation) {
            return Err("Controller changed while opening stream".into());
        }
        let stream = match &binding.backend {
            ControllerBackend::Local(process) if process.endpoint().ipc_path().is_some() => {
                let controller = process
                    .native_controller()
                    .map_err(|error| error.to_string())?
                    .ok_or("Local binding has no IPC controller")?;
                StreamBackend::Service(Box::new(
                    controller
                        .websocket(&service_stream_path(path, query)?)
                        .await
                        .map_err(|error| error.to_string())?,
                ))
            }
            ControllerBackend::Direct(_) | ControllerBackend::Local(_) => {
                let endpoint = binding.endpoint().ok_or("Direct binding has no endpoint")?;
                StreamBackend::Direct(Box::new(
                    connect_stream(&endpoint, path, query, timeout).await?,
                ))
            }
            ControllerBackend::Service { runtime } => {
                let path = service_stream_path(path, query)?;
                StreamBackend::Service(Box::new(
                    runtime
                        .client
                        .controller_socket(&path, &runtime.controller_secret())
                        .await
                        .map_err(|error| error.to_string())?,
                ))
            }
        };
        if !self.is_current(binding.generation) {
            return Err("Controller changed while opening stream".into());
        }
        Ok(ControllerStream {
            stream,
            binding: self.clone(),
            generation: binding.generation,
        })
    }
}

fn service_stream_path(path: &str, query: &[(&str, &str)]) -> Result<String, String> {
    if !matches!(path, "/logs" | "/traffic" | "/connections" | "/memory") {
        return Err("Unsupported native controller stream".into());
    }
    let mut url = reqwest::Url::parse(&format!("http://localhost{path}"))
        .map_err(|error| error.to_string())?;
    if path == "/logs" {
        let mut level = None;
        let mut format = None;
        for &(key, value) in query {
            match key {
                "level"
                    if level.is_none()
                        && matches!(value, "silent" | "error" | "warning" | "info" | "debug") =>
                {
                    level = Some(value)
                }
                "format" if format.is_none() && matches!(value, "plain" | "structured") => {
                    format = Some(value)
                }
                _ => return Err("Invalid Mihomo log stream options".into()),
            }
        }
        url.query_pairs_mut().extend_pairs([
            ("level", level.unwrap_or("info")),
            ("format", format.unwrap_or("plain")),
        ]);
    } else if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query.iter().copied());
    }
    Ok(format!(
        "{}{}",
        url.path(),
        url.query()
            .map_or_else(String::new, |query| format!("?{query}"))
    ))
}

#[cfg(test)]
#[path = "service_logs_tests.rs"]
mod service_logs_tests;

enum StreamBackend {
    Direct(Box<MihomoSocket>),
    Service(Box<NativeSocket>),
}

pub(crate) struct ControllerStream {
    stream: StreamBackend,
    binding: Arc<ControllerBinding>,
    generation: u64,
}

impl ControllerStream {
    pub(crate) async fn next(&mut self) -> Option<Result<Message, String>> {
        if !self.binding.is_current(self.generation) {
            return None;
        }
        let message = match &mut self.stream {
            StreamBackend::Direct(stream) => stream
                .next()
                .await
                .map(|message| message.map_err(|error| error.to_string())),
            StreamBackend::Service(stream) => stream
                .next()
                .await
                .map(|message| message.map_err(|error| error.to_string())),
        };
        if !self.binding.is_current(self.generation) {
            return None;
        }
        message
    }
}
