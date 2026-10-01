use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use futures_util::StreamExt;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;
use zenclash_service::{ServiceClient, ServiceStream, ServiceSubscription};

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
        client: Arc<ServiceClient>,
        source_home: std::path::PathBuf,
    ) -> Arc<Self> {
        let (updates, _) = watch::channel(BindingSnapshot {
            generation: 0,
            kind: crate::CoreKind::Mihomo,
            backend: ControllerBackend::Service {
                runtime: crate::service_runtime_session::ServiceRuntimeSession::new(
                    client,
                    source_home,
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
    pub(crate) fn service(&self) -> Option<Arc<ServiceClient>> {
        self.runtime().map(|runtime| runtime.client.clone())
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
        let stream = match &binding.backend {
            ControllerBackend::Direct(_) | ControllerBackend::Local(_) => {
                let endpoint = binding.endpoint().ok_or("Direct binding has no endpoint")?;
                StreamBackend::Direct(Box::new(
                    connect_stream(&endpoint, path, query, timeout).await?,
                ))
            }
            ControllerBackend::Service { runtime } => {
                let kind = match path {
                    "/traffic" => ServiceStream::Traffic,
                    "/logs" => ServiceStream::Logs,
                    "/connections" => ServiceStream::Connections,
                    "/memory" => ServiceStream::Memory,
                    _ => return Err("Unsupported service stream".into()),
                };
                StreamBackend::Service(
                    runtime
                        .client
                        .subscribe(kind)
                        .await
                        .map_err(|error| error.to_string())?,
                )
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

enum StreamBackend {
    Direct(Box<MihomoSocket>),
    Service(ServiceSubscription),
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
            StreamBackend::Service(stream) => match stream.next().await {
                Ok(Some(value)) => Some(
                    serde_json::to_vec(&value)
                        .map(Message::Binary)
                        .map_err(|error| error.to_string()),
                ),
                Ok(None) => None,
                Err(error) => Some(Err(error.to_string())),
            },
        };
        if !self.binding.is_current(self.generation) {
            return None;
        }
        message
    }
}
