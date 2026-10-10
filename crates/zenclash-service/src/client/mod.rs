// Modified for the ZenClash fork on 2026-10-05; see NOTICE.md. GPL-3.0-only.
use std::{path::Path, sync::Arc, time::Duration};

use anyhow::Result;
use kode_bridge::{ClientConfig, IpcHttpClient};
use log::{debug, warn};
use once_cell::sync::Lazy;
use tokio::sync::RwLock;

#[cfg(all(windows, any(not(feature = "test"), test)))]
mod windows_identity;

use crate::{
    AuthenticatedRequest, AuthenticatedSessionRequest, IPC_AUTH_EXPECT, IPC_PATH, IpcCommand,
    MIN_REQUIRED_SERVICE_REVISION, MacosProxyConfig, OwnerCredentials, OwnerSessionProof, ProtocolInfo,
    ProtocolVersion, ProxyApplyOutcome, RuntimeBundle, RuntimeFileOutcome, RuntimeFileRequest, ServiceStatusSnapshot,
    StageRuntimeOutcome, StartClashRequest, StartClashResult, WriterConfig,
    core::structure::{JsonConvert, Response},
};

static CLIENT_CONFIG: Lazy<Arc<RwLock<Option<IpcConfig>>>> = Lazy::new(|| Arc::new(RwLock::new(None)));

static IPC_AUTH_HEADER_KEY: &str = "X-IPC-Magic";
const LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(30);
// Large cache chunks need a longer timeout than control messages.
const RUNTIME_FILE_TIMEOUT: Duration = Duration::from_secs(15);

fn protected<'a>(request: kode_bridge::HttpRequestBuilder<'a>) -> kode_bridge::HttpRequestBuilder<'a> {
    request.header(
        crate::SERVICE_PROTOCOL_HEADER,
        ProtocolVersion::current().header_value(),
    )
}

#[derive(Clone, Copy)]
enum Verb {
    Get,
    Post,
    Put,
    Delete,
}

/// Sends a versioned, authenticated request using the route's required envelope.
/// `session` selects owner-only or active-session authentication.
async fn protected_call<P, R>(
    verb: Verb,
    command: IpcCommand,
    credentials: &OwnerCredentials,
    session: Option<&OwnerSessionProof>,
    payload: P,
    timeout: Option<Duration>,
) -> Result<Response<R>>
where
    P: serde::Serialize + for<'de> serde::Deserialize<'de>,
    R: for<'de> serde::Deserialize<'de>,
{
    let client = connect().await?;
    let body = match session {
        None => AuthenticatedRequest {
            credentials: credentials.clone(),
            payload,
        }
        .to_json_value()?,
        Some(session) => AuthenticatedSessionRequest {
            credentials: credentials.clone(),
            session: session.clone(),
            payload,
        }
        .to_json_value()?,
    };
    let path = command.as_ref();
    let request = protected(match verb {
        Verb::Get => client.get(path),
        Verb::Post => client.post(path),
        Verb::Put => client.put(path),
        Verb::Delete => client.delete(path),
    });
    let request = match timeout {
        Some(timeout) => request.timeout(timeout),
        None => request,
    };
    let response = request.json_body(&body).send().await?.json::<Response<R>>()?;
    Ok(response)
}

#[derive(Debug, Clone)]
pub struct IpcConfig {
    pub default_timeout: Duration,
    pub max_retries: usize,
    pub retry_delay: Duration,
}

impl Default for IpcConfig {
    fn default() -> Self {
        Self {
            default_timeout: Duration::from_millis(50),
            max_retries: 8,
            retry_delay: Duration::from_millis(150),
        }
    }
}

pub async fn set_config(config: Option<IpcConfig>) {
    let mut guard = CLIENT_CONFIG.write().await;
    *guard = config;
}

///
/// # Errors
/// Returns native connection, serialization, timeout or HTTP decoding errors. Service refusals remain in the returned response code.
pub async fn connect() -> Result<IpcHttpClient> {
    debug!("Connecting to IPC at {}", IPC_PATH);

    #[cfg(all(target_os = "macos", not(feature = "test")))]
    if !Path::new(IPC_PATH).exists() {
        crate::macos_activation::wake_service().await?;
    }

    #[cfg(all(unix, any(not(target_os = "macos"), feature = "test")))]
    {
        if let Err(err) = Path::metadata(IPC_PATH.as_ref()) {
            return Err(anyhow::anyhow!("IPC path unavailable: {err}"));
        }
    }

    let c = { CLIENT_CONFIG.read().await.clone() }.unwrap_or_default();
    debug!("Using config: {:?}", c);
    let client = kode_bridge::IpcHttpClient::with_config(
        IPC_PATH,
        ClientConfig {
            default_timeout: c.default_timeout,
            max_retries: c.max_retries,
            retry_delay: c.retry_delay,
            enable_pooling: true,
            require_windows_server_system: false,
            #[cfg(all(windows, not(feature = "test")))]
            windows_server_pid_verifier: Some(windows_identity::verify_registered_service_process_id),
            #[cfg(all(windows, feature = "test"))]
            windows_server_pid_verifier: None,
            ..Default::default()
        },
    )?;

    let greeting = client
        .get(IpcCommand::Magic.as_ref())
        .header(IPC_AUTH_HEADER_KEY, IPC_AUTH_EXPECT)
        .send()
        .await;
    #[cfg(all(target_os = "macos", not(feature = "test")))]
    let greeting = match greeting {
        Ok(response) => Ok(response),
        Err(_) => {
            // A stale socket can survive a crash. XPC asks launchd to start its registered
            // helper without administrator prompts; the helper repairs its own socket.
            crate::macos_activation::wake_service().await?;
            client
                .get(IpcCommand::Magic.as_ref())
                .header(IPC_AUTH_HEADER_KEY, IPC_AUTH_EXPECT)
                .send()
                .await
        }
    };
    let greeting = match greeting {
        Ok(response) => response,
        Err(e) => {
            warn!("Failed to connect to IPC server: {}", e);
            return Err(anyhow::anyhow!("Failed to connect to IPC server: {}", e));
        }
    };
    #[cfg(all(target_os = "macos", not(feature = "test")))]
    if greeting
        .headers()
        .get("x-zenclash-on-demand")
        .and_then(serde_json::Value::as_str)
        == Some("1")
    {
        // Retain one native connection for this GUI process. Its death is observable even
        // when the process crashes, and it protects preparation before a core owner exists.
        crate::macos_activation::wake_service().await?;
    }
    #[cfg(any(not(target_os = "macos"), feature = "test"))]
    let _ = greeting;

    Ok(client)
}

pub fn is_ipc_path_exists() -> bool {
    Path::new(IPC_PATH).exists()
}

///
/// # Errors
/// Returns native connection, serialization, timeout or HTTP decoding errors. Service refusals remain in the returned response code.
pub async fn get_version() -> Result<Response<ProtocolInfo>> {
    let client = connect().await?;
    let response = client
        .get(IpcCommand::GetVersion.as_ref())
        .header(IPC_AUTH_HEADER_KEY, IPC_AUTH_EXPECT)
        .send()
        .await?
        .json::<Response<ProtocolInfo>>()?;
    Ok(response)
}

///
/// # Errors
/// Returns native connection, serialization, timeout or HTTP decoding errors. Service refusals remain in the returned response code.
pub async fn get_status(credentials: &OwnerCredentials) -> Result<Response<ServiceStatusSnapshot>> {
    protected_call(Verb::Get, IpcCommand::Status, credentials, None, (), None).await
}

/// Renews only the proved owner generation; status reads never extend ownership.
///
/// # Errors
/// Returns native connection, timeout or decoding errors; refusals retain their response code.
pub async fn heartbeat(credentials: &OwnerCredentials, session: &OwnerSessionProof) -> Result<Response<()>> {
    protected_call(
        Verb::Post,
        IpcCommand::Heartbeat,
        credentials,
        Some(session),
        (),
        Some(crate::OWNER_SESSION_HEARTBEAT_INTERVAL),
    )
    .await
}

pub async fn is_reinstall_service_needed() -> bool {
    is_ipc_path_exists()
        && match get_version().await {
            Ok(resp) => resp
                .data
                .is_none_or(|info| !info.supports_client(ProtocolVersion::current(), MIN_REQUIRED_SERVICE_REVISION)),
            Err(_) => true,
        }
}

///
/// # Errors
/// Returns native connection, serialization, timeout or HTTP decoding errors. Service refusals remain in the returned response code.
pub async fn start_clash(
    credentials: &OwnerCredentials,
    body: &StartClashRequest,
) -> Result<Response<StartClashResult>> {
    protected_call(
        Verb::Post,
        IpcCommand::StartClash,
        credentials,
        None,
        body.clone(),
        Some(LIFECYCLE_TIMEOUT),
    )
    .await
}

///
/// # Errors
/// Returns native connection, serialization, timeout or HTTP decoding errors. Service refusals remain in the returned response code.
pub async fn get_clash_logs(credentials: &OwnerCredentials) -> Result<Response<Vec<String>>> {
    protected_call(Verb::Get, IpcCommand::GetClashLogs, credentials, None, (), None).await
}

///
/// # Errors
/// Returns native connection, serialization, timeout or HTTP decoding errors. Service refusals remain in the returned response code.
pub async fn get_clash_log_snapshot(credentials: &OwnerCredentials) -> Result<Response<String>> {
    protected_call(Verb::Get, IpcCommand::GetClashLogSnapshot, credentials, None, (), None).await
}

///
/// # Errors
/// Returns native connection, serialization, timeout or HTTP decoding errors. Service refusals remain in the returned response code.
pub async fn stop_clash(credentials: &OwnerCredentials, session: &OwnerSessionProof) -> Result<Response<()>> {
    protected_call(
        Verb::Delete,
        IpcCommand::StopClash,
        credentials,
        Some(session),
        (),
        Some(LIFECYCLE_TIMEOUT),
    )
    .await
}

/// Stages `body` into the running core's generation without restarting it.
/// Call only when [`ProtocolInfo::supports_runtime_staging`] is true; `RestartRequired` is a
/// successful response that asks the caller to fall back to stop and start.
///
/// # Errors
/// Returns native connection, serialization, timeout or HTTP decoding errors. Service refusals remain in the returned response code.
pub async fn stage_runtime(
    credentials: &OwnerCredentials,
    session: &OwnerSessionProof,
    body: &RuntimeBundle,
) -> Result<Response<StageRuntimeOutcome>> {
    protected_call(
        Verb::Put,
        IpcCommand::StageRuntime,
        credentials,
        Some(session),
        body.clone(),
        Some(LIFECYCLE_TIMEOUT),
    )
    .await
}

/// Requires [`ProtocolInfo::supports_runtime_file_read`]; advance `offset` until `len` is reached.
///
/// # Errors
/// Returns native connection, serialization, timeout or HTTP decoding errors. Service refusals remain in the returned response code.
pub async fn read_runtime_file(
    credentials: &OwnerCredentials,
    session: &OwnerSessionProof,
    body: &RuntimeFileRequest,
) -> Result<Response<RuntimeFileOutcome>> {
    protected_call(
        Verb::Get,
        IpcCommand::ReadRuntimeFile,
        credentials,
        Some(session),
        body.clone(),
        Some(RUNTIME_FILE_TIMEOUT),
    )
    .await
}

///
/// # Errors
/// Returns native connection, serialization, timeout or HTTP decoding errors. Service refusals remain in the returned response code.
pub async fn update_writer(
    credentials: &OwnerCredentials,
    session: &OwnerSessionProof,
    body: &WriterConfig,
) -> Result<Response<()>> {
    protected_call(
        Verb::Put,
        IpcCommand::UpdateWriter,
        credentials,
        Some(session),
        body.clone(),
        None,
    )
    .await
}

///
/// # Errors
/// Returns native connection, serialization, timeout or HTTP decoding errors. Service refusals remain in the returned response code.
pub async fn set_system_proxy(
    credentials: &OwnerCredentials,
    session: &OwnerSessionProof,
    body: &MacosProxyConfig,
) -> Result<Response<ProxyApplyOutcome>> {
    protected_call(
        Verb::Put,
        IpcCommand::SetSystemProxy,
        credentials,
        Some(session),
        body.clone(),
        None,
    )
    .await
}

/// Inspects approved cores and global occupancy without exposing another owner's session.
///
/// # Errors
/// Returns IPC transport, malformed-response or service-inspection errors. A missing core is reported as availability data.
pub async fn inspect_installation(requirements: &[crate::CoreRequirement]) -> Result<crate::InstallationStatus> {
    inspect_installation_with_digest(requirements, false).await
}

pub(crate) async fn inspect_installation_with_digest(
    requirements: &[crate::CoreRequirement],
    include_service_digest: bool,
) -> Result<crate::InstallationStatus> {
    let client = connect().await?;
    let response = protected(client.post(IpcCommand::InspectInstallation.as_ref()))
        .timeout(LIFECYCLE_TIMEOUT)
        .header(IPC_AUTH_HEADER_KEY, IPC_AUTH_EXPECT)
        .header(
            "X-Service-Digest",
            if include_service_digest { "true" } else { "false" },
        )
        .json_body(&serde_json::to_value(requirements)?)
        .send()
        .await?
        .json::<Response<crate::InstallationStatus>>()?;
    anyhow::ensure!(
        response.code == 0,
        "installation inspection rejected: {} ({})",
        response.message,
        response.code
    );
    response
        .data
        .ok_or_else(|| anyhow::anyhow!("service omitted installation status"))
}
