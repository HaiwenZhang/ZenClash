//! Application submission and observation of the shared outbound-mode capability.
use super::{Context, MihomoLogLevel, ZenClashApp};
use crate::outbound_mode::OutboundMode;
use crate::outbound_mode::OutboundModeCoordinator;
use std::sync::Arc;
use std::time::Duration;
use zenclash_core::{CoreSession, CoreSessionError, LogMonitor};

async fn synchronize_runtime_mode(
    session: CoreSession,
    mode: OutboundModeCoordinator,
    logs: Arc<LogMonitor>,
    intent_generation: u64,
) -> Result<(), CoreSessionError> {
    let observation = session.begin_observation().await?;
    let config = observation.client().runtime_config().await?;
    observation.identity().publish_if_current(|| {
        mode.synchronize(OutboundMode::from_api(&config.mode), intent_generation);
        if let Some(level) = MihomoLogLevel::from_api(&config.log_level) {
            logs.set_level(level);
        }
    });
    Ok(())
}

impl ZenClashApp {
    pub(in crate::app) fn request_outbound_mode(
        &mut self,
        mode: OutboundMode,
        cx: &mut Context<Self>,
    ) {
        self.proxies_page
            .update(cx, |page, cx| page.set_outbound_mode(mode.api_value(), cx));
        if self.outbound_mode.request(
            mode,
            &self.core_session,
            &self.controlled_config_store,
            &self.runtime,
        ) {
            let pending = self.outbound_mode.is_pending();
            self.runtime_page.update(cx, |page, cx| {
                page.begin_home_mode_transition(mode, pending, cx);
            });
            cx.notify();
        }
    }
    pub(super) fn start_mode_sync(&mut self, cx: &mut Context<Self>) {
        let session = self.core_session.clone();
        let runtime = self.runtime.clone();
        let mode = self.outbound_mode.clone();
        let logs = self.log_monitor.clone();
        cx.spawn(async move |this, _cx| {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let intent_generation = mode.intent_generation();
                let task = runtime.spawn(synchronize_runtime_mode(
                    session.clone(),
                    mode.clone(),
                    logs.clone(),
                    intent_generation,
                ));
                match task.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::debug!(%error, "failed to synchronize Mihomo outbound mode");
                    }
                    Err(error) => {
                        tracing::warn!(%error, "outbound mode synchronization task failed")
                    }
                }
                if this.upgrade().is_none() {
                    break;
                }
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use zenclash_core::{ControlledConfigStore, CoreKind, MihomoClient, MihomoEndpoint};

    #[tokio::test]
    async fn mode_sync_publishes_fresh_facts_but_discards_changed_configuration_or_runtime() {
        for (change_config, replace_runtime) in [(false, false), (true, false), (false, true)] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let client = MihomoClient::new(MihomoEndpoint::new(
                format!("http://{}", listener.local_addr().unwrap()),
                "",
            ))
            .unwrap();
            let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
            let mode = OutboundModeCoordinator::new_unsynchronized(OutboundMode::Rule);
            let logs = LogMonitor::start(
                &tokio::runtime::Handle::current(),
                MihomoEndpoint::new("http://127.0.0.1:1", ""),
                MihomoLogLevel::Info,
            );
            let (entered, started) = tokio::sync::oneshot::channel();
            let (release, released) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut old, _) = listener.accept().await.unwrap();
                zenclash_core::test_support::read_controller_request(&mut old)
                    .await
                    .unwrap();
                entered.send(()).unwrap();
                let old_reply = tokio::spawn(async move {
                    released.await.unwrap();
                    let body = r#"{"mode":"direct","log-level":"warning"}"#;
                    old.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                });
                if change_config {
                    for method in ["PATCH", "GET"] {
                        let (mut stream, _) = listener.accept().await.unwrap();
                        let (headers, _) =
                            zenclash_core::test_support::read_controller_request(&mut stream)
                                .await
                                .unwrap()
                                .into_parts();
                        assert!(headers.starts_with(method));
                        let response = if method == "PATCH" {
                            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned()
                        } else {
                            let body = r#"{"mode":"global","log-level":"debug"}"#;
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        };
                        stream.write_all(response.as_bytes()).await.unwrap();
                    }
                }
                old_reply.await.unwrap();
            });
            let sync = tokio::spawn(synchronize_runtime_mode(
                session.clone(),
                mode.clone(),
                logs.clone(),
                mode.intent_generation(),
            ));
            tokio::time::timeout(Duration::from_secs(2), started)
                .await
                .unwrap()
                .unwrap();
            let root = std::env::temp_dir().join(format!(
                "zenclash-mode-sync-{}-{}",
                std::process::id(),
                listener_port_marker(&session)
            ));
            if change_config {
                session
                    .set_mode(&ControlledConfigStore::new(&root), "global")
                    .await
                    .unwrap();
            }
            if replace_runtime {
                session
                    .switch_to_direct(MihomoEndpoint::new("http://127.0.0.1:1", ""))
                    .await
                    .unwrap();
            }
            if change_config || replace_runtime {
                mode.synchronize(OutboundMode::Global, mode.intent_generation());
                logs.set_level(MihomoLogLevel::Debug);
            }
            release.send(()).unwrap();
            let result = tokio::time::timeout(Duration::from_secs(2), sync)
                .await
                .unwrap()
                .unwrap();
            if !replace_runtime {
                result.unwrap();
            }
            assert_eq!(
                mode.displayed(),
                if change_config || replace_runtime {
                    OutboundMode::Global
                } else {
                    OutboundMode::Direct
                }
            );
            assert_eq!(
                logs.level(),
                if change_config || replace_runtime {
                    MihomoLogLevel::Debug
                } else {
                    MihomoLogLevel::Warning
                }
            );
            server.await.unwrap();
            let _ = std::fs::remove_dir_all(root);
        }
    }

    fn listener_port_marker(session: &CoreSession) -> String {
        session
            .client()
            .endpoint()
            .unwrap()
            .controller
            .replace(':', "-")
    }
}
