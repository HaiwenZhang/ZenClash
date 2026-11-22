// Modified for the ZenClash fork on 2026-10-05; see NOTICE.md. GPL-3.0-only.
//! IPC HTTP router wiring for all privileged service commands.

use super::IPC_HANDLER_TIMEOUT;
use super::handlers::{
    OWNER_LIFECYCLE_LOCK, OwnerLifecycleGate, STOP_RESERVE, StartOwnerTransition, authenticate_request, bad_request,
    enter_owner_lifecycle, ok_empty, ok_json, read_log_snapshot, require_active_session, require_protocol_version,
    run_owner_transition, service_error, service_unavailable, sigkill_deadline,
};
use crate::core::auth::{ServiceError, hash_session_token, ipc_request_context_to_auth_context};
use crate::core::desired::{
    clear_active_owner, load_active_owner, persist_owner_core_stopped, persist_owner_writer_config,
};
use crate::core::logger::{LOG_RING, flush_writer, set_or_update_writer};
use crate::core::manager::CORE_MANAGER;
use crate::core::paths::service_paths;
use crate::core::runtime_generation::{prepare_runtime, read_runtime_file, stage_runtime};
use crate::core::state::set_core_lifecycle_state;
use crate::core::status::service_status_snapshot;
use crate::core::structure::ServiceLifecycleState;
use crate::{
    AuthenticatedRequest, AuthenticatedSessionRequest, IpcCommand, MacosProxyConfig, OwnerSessionHandle, ProtocolInfo,
    ProxyApplyOutcome, RuntimeBundle, RuntimeFileRequest, StartClashRequest, StartClashResult, WriterConfig,
};
use kode_bridge::{Result, Router, ipc_http_server::HttpResponse};
use std::{ops::ControlFlow, time::Instant};
use tracing::{info, trace};

pub(super) fn create_ipc_router() -> Result<Router> {
    let router = Router::new()
        .get(IpcCommand::Magic.as_ref(), |ctx| async move {
            trace!("Received Magic command");
            ipc_request_context_to_auth_context(&ctx)?;
            let response = HttpResponse::builder().text("Tunglies!");
            #[cfg(all(target_os = "macos", not(feature = "test")))]
            let response = response.header("X-ZenClash-On-Demand", "1");
            Ok(response.build())
        })
        .get(IpcCommand::GetVersion.as_ref(), |ctx| async move {
            ipc_request_context_to_auth_context(&ctx)?;
            ok_json(ProtocolInfo::current())
        })
        .post(IpcCommand::InspectInstallation.as_ref(), |ctx| async move {
            ipc_request_context_to_auth_context(&ctx)?;
            if let Err(error) = require_protocol_version(&ctx) {
                return service_error(error);
            }
            let requirements = match ctx.json::<Vec<crate::CoreRequirement>>() {
                Ok(requirements) => requirements,
                Err(error) => return bad_request(format!("Invalid core requirements: {error}")),
            };
            let _guard = OWNER_LIFECYCLE_LOCK.lock().await;
            // Omitting this header retains the original response with a service digest.
            let include_service_digest = ctx.headers.get("X-Service-Digest").is_none_or(|value| value != "false");
            match crate::core::installation::inspect(&requirements, include_service_digest).await {
                Ok(status) => ok_json(status),
                Err(error) => service_unavailable(format!("Failed to inspect installation: {error:#}")),
            }
        })
        .get(IpcCommand::Status.as_ref(), |ctx| async move {
            trace!("Received Status command");
            let (_request, owner) = match authenticate_request::<AuthenticatedRequest<()>>(&ctx) {
                ControlFlow::Continue(authenticated) => authenticated,
                ControlFlow::Break(response) => return response,
            };
            let _lifecycle_guard = match enter_owner_lifecycle(&owner, OwnerLifecycleGate::Unchecked).await {
                ControlFlow::Continue(guard) => guard,
                ControlFlow::Break(response) => return response,
            };
            match service_status_snapshot(&owner).await {
                Ok(status) => ok_json(status),
                Err(error) => service_unavailable(format!("Failed to collect service status: {error:#}")),
            }
        })
        .post(IpcCommand::StartClash.as_ref(), |ctx| async move {
            let handler_deadline = Instant::now() + IPC_HANDLER_TIMEOUT;
            trace!("Received StartClash command");
            let (request, owner) = match authenticate_request::<AuthenticatedRequest<StartClashRequest>>(&ctx) {
                ControlFlow::Continue(authenticated) => authenticated,
                ControlFlow::Break(response) => return response,
            };
            let start_request = request.payload;
            if hash_session_token(&start_request.proposed_session_token).is_err() {
                return bad_request("Invalid proposed owner session token");
            }
            if start_request.macos_proxy.is_some() {
                return service_error(ServiceError::invalid_proxy_config(
                    "system proxy is managed by zenclash-core, not zenclash-service",
                ));
            }
            let _lifecycle_guard = match enter_owner_lifecycle(&owner, OwnerLifecycleGate::Unchecked).await {
                ControlFlow::Continue(guard) => guard,
                ControlFlow::Break(response) => return response,
            };
            let previous_owner = match load_active_owner().await {
                Ok(owner) => owner,
                Err(error) => {
                    return service_unavailable(format!("Failed to load active owner: {error:#}"));
                }
            };
            let prepared_runtime = match prepare_runtime(&owner, &start_request.runtime).await {
                Ok(prepared) => prepared,
                Err(error) => return service_error(error),
            };
            let mut transition = StartOwnerTransition {
                handler_deadline,
                previous_owner,
                owner: &owner,
                prepared_runtime: Some(prepared_runtime),
                proposed_session_token: &start_request.proposed_session_token,
            };
            let active = match run_owner_transition(&mut transition).await {
                Ok(active) => active,
                Err(error) => return service_error(error),
            };
            info!("Core started successfully");
            ok_json(StartClashResult {
                session: OwnerSessionHandle {
                    generation: active.generation,
                },
                proxy_outcome: ProxyApplyOutcome::NotRequested,
            })
        })
        .post(IpcCommand::Heartbeat.as_ref(), |ctx| async move {
            let (request, owner) = match authenticate_request::<AuthenticatedSessionRequest<()>>(&ctx) {
                ControlFlow::Continue(authenticated) => authenticated,
                ControlFlow::Break(response) => return response,
            };
            let _guard = OWNER_LIFECYCLE_LOCK.lock().await;
            if let Err(error) = require_active_session(&owner, &request.session).await {
                return service_error(error);
            }
            if crate::core::owner_lease::is_expired(request.session.generation) {
                return service_error(ServiceError::stale_owner_session());
            }
            crate::core::owner_lease::renew(request.session.generation);
            ok_empty("Owner session renewed")
        })
        .get(IpcCommand::GetClashLogs.as_ref(), |ctx| async move {
            trace!("Received GetClashLogs command");
            let (_request, owner) = match authenticate_request::<AuthenticatedRequest<()>>(&ctx) {
                ControlFlow::Continue(authenticated) => authenticated,
                ControlFlow::Break(response) => return response,
            };
            let _lifecycle_guard = match enter_owner_lifecycle(&owner, OwnerLifecycleGate::ActiveOwner).await {
                ControlFlow::Continue(guard) => guard,
                ControlFlow::Break(response) => return response,
            };
            ok_json(LOG_RING.get_logs())
        })
        .get(IpcCommand::GetClashLogSnapshot.as_ref(), |ctx| async move {
            trace!("Received GetClashLogSnapshot command");
            let (_request, owner) = match authenticate_request::<AuthenticatedRequest<()>>(&ctx) {
                ControlFlow::Continue(authenticated) => authenticated,
                ControlFlow::Break(response) => return response,
            };
            let _lifecycle_guard = match enter_owner_lifecycle(&owner, OwnerLifecycleGate::ActiveOwner).await {
                ControlFlow::Continue(guard) => guard,
                ControlFlow::Break(response) => return response,
            };
            let paths = match service_paths() {
                Ok(paths) => paths,
                Err(error) => return service_unavailable(format!("Failed to locate service data: {error}")),
            };
            flush_writer();
            let path = paths.for_owner(&owner.identity).logs_dir().join("service_latest.log");
            match read_log_snapshot(&path).await {
                Ok(snapshot) => ok_json(snapshot),
                Err(error) => service_unavailable(format!("Failed to read core log snapshot: {error}")),
            }
        })
        .delete(IpcCommand::StopClash.as_ref(), |ctx| async move {
            let handler_deadline = Instant::now() + IPC_HANDLER_TIMEOUT;
            trace!("Received StopClash command");
            let (request, owner) = match authenticate_request::<AuthenticatedSessionRequest<()>>(&ctx) {
                ControlFlow::Continue(authenticated) => authenticated,
                ControlFlow::Break(response) => return response,
            };
            let _lifecycle_guard =
                match enter_owner_lifecycle(&owner, OwnerLifecycleGate::ActiveSession(&request.session)).await {
                    ControlFlow::Continue(guard) => guard,
                    ControlFlow::Break(response) => return response,
                };
            let kill_by = sigkill_deadline(handler_deadline, STOP_RESERVE);
            match CORE_MANAGER.lock().await.stop_core_by(kill_by).await {
                Ok(_) => info!("Core stopped successfully"),
                Err(e) => {
                    return service_unavailable(format!("Failed to stop core: {}", e));
                }
            }
            if let Err(e) = persist_owner_core_stopped(&owner).await {
                set_core_lifecycle_state(ServiceLifecycleState::Fatal);
                return service_unavailable(format!("Failed to persist desired state: {}", e));
            }
            if let Err(e) = clear_active_owner().await {
                set_core_lifecycle_state(ServiceLifecycleState::Fatal);
                return service_unavailable(format!("Failed to clear active owner: {}", e));
            }
            crate::core::owner_lease::reset(None);
            ok_empty("Core stopped successfully")
        })
        .put(IpcCommand::StageRuntime.as_ref(), |ctx| async move {
            trace!("Received StageRuntime command");
            let (request, owner) = match authenticate_request::<AuthenticatedSessionRequest<RuntimeBundle>>(&ctx) {
                ControlFlow::Continue(authenticated) => authenticated,
                ControlFlow::Break(response) => return response,
            };
            // Staging rewrites the live generation, so hold the lifecycle lock and require its
            // current session for the whole operation.
            let _lifecycle_guard =
                match enter_owner_lifecycle(&owner, OwnerLifecycleGate::ActiveSession(&request.session)).await {
                    ControlFlow::Continue(guard) => guard,
                    ControlFlow::Break(response) => return response,
                };
            match stage_runtime(&owner, &request.payload).await {
                Ok(outcome) => ok_json(outcome),
                Err(error) => service_error(error),
            }
        })
        .get(IpcCommand::ReadRuntimeFile.as_ref(), |ctx| async move {
            trace!("Received ReadRuntimeFile command");
            let (request, owner) = match authenticate_request::<AuthenticatedSessionRequest<RuntimeFileRequest>>(&ctx) {
                ControlFlow::Continue(authenticated) => authenticated,
                ControlFlow::Break(response) => return response,
            };
            // Reads require the session that owns the running core.
            let _lifecycle_guard =
                match enter_owner_lifecycle(&owner, OwnerLifecycleGate::ActiveSession(&request.session)).await {
                    ControlFlow::Continue(guard) => guard,
                    ControlFlow::Break(response) => return response,
                };
            match read_runtime_file(&owner, &request.payload.destination, request.payload.offset).await {
                Ok(outcome) => ok_json(outcome),
                Err(error) => service_error(error),
            }
        })
        .put(IpcCommand::UpdateWriter.as_ref(), |ctx| async move {
            trace!("Received UpdateWriter command");
            let (request, owner) = match authenticate_request::<AuthenticatedSessionRequest<WriterConfig>>(&ctx) {
                ControlFlow::Continue(authenticated) => authenticated,
                ControlFlow::Break(response) => return response,
            };
            let mut writer_config = request.payload;
            let _lifecycle_guard =
                match enter_owner_lifecycle(&owner, OwnerLifecycleGate::ActiveSession(&request.session)).await {
                    ControlFlow::Continue(guard) => guard,
                    ControlFlow::Break(response) => return response,
                };
            // Never let the client choose a service-owned log destination.
            let paths = match service_paths() {
                Ok(paths) => paths,
                Err(error) => return service_unavailable(format!("Failed to locate service data: {error}")),
            };
            writer_config.directory = paths
                .for_owner(&owner.identity)
                .logs_dir()
                .to_string_lossy()
                .into_owned();
            match set_or_update_writer(&writer_config) {
                Ok(_) => info!("Update writer successfully"),
                Err(e) => {
                    return service_unavailable(format!("Failed to update writer: {}", e));
                }
            };
            if let Err(e) = persist_owner_writer_config(&owner, &writer_config).await {
                return service_unavailable(format!("Failed to persist writer config: {}", e));
            }
            ok_empty("Update Writer successfully")
        })
        .put(IpcCommand::SetTunDns.as_ref(), |ctx| async move {
            let (request, owner) = match authenticate_request::<AuthenticatedSessionRequest<bool>>(&ctx) {
                ControlFlow::Continue(authenticated) => authenticated,
                ControlFlow::Break(response) => return response,
            };
            let _lifecycle_guard =
                match enter_owner_lifecycle(&owner, OwnerLifecycleGate::ActiveSession(&request.session)).await {
                    ControlFlow::Continue(guard) => guard,
                    ControlFlow::Break(response) => return response,
                };
            if request.payload && CORE_MANAGER.lock().await.running_core_config().await.is_none() {
                return service_unavailable("TUN DNS requires a running owned core");
            }
            match crate::core::tun_dns::apply(request.payload).await {
                Ok(()) => ok_empty("TUN DNS updated"),
                Err(error) => service_unavailable(format!("Failed to update TUN DNS: {error:#}")),
            }
        })
        .put(IpcCommand::SetSystemProxy.as_ref(), |ctx| async move {
            trace!("Received SetSystemProxy command");
            let (request, owner) = match authenticate_request::<AuthenticatedSessionRequest<MacosProxyConfig>>(&ctx) {
                ControlFlow::Continue(authenticated) => authenticated,
                ControlFlow::Break(response) => return response,
            };
            // Reject a stale session before reporting removal of service-side proxy control.
            let _lifecycle_guard =
                match enter_owner_lifecycle(&owner, OwnerLifecycleGate::ActiveSession(&request.session)).await {
                    ControlFlow::Continue(guard) => guard,
                    ControlFlow::Break(response) => return response,
                };
            service_error(ServiceError::invalid_proxy_config(
                "system proxy is managed by zenclash-core, not zenclash-service",
            ))
        });
    Ok(router)
}
