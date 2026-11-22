//! Shared outbound routing values and intent coordination used by application entry points.

use std::sync::Arc;

use zenclash_core::{CoreSession, CoreSessionError, LogMonitor, MihomoLogLevel};

mod coordinator;
pub use coordinator::OutboundModeCoordinator;

/// Mutually exclusive outbound routing modes accepted by the connected core.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OutboundMode {
    /// Route connections through Mihomo rules.
    #[default]
    Rule,
    /// Route connections through the selected global proxy.
    Global,
    /// Bypass proxies for every connection.
    Direct,
}

impl OutboundMode {
    /// Returns the localized user-facing label.
    #[must_use]
    pub fn label(self) -> String {
        zenclash_i18n::text(match self {
            Self::Rule => "outbound_mode.rule",
            Self::Global => "outbound_mode.global",
            Self::Direct => "outbound_mode.direct",
        })
    }

    /// Returns the compact uppercase UI code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Rule => "RULE",
            Self::Global => "GLOBAL",
            Self::Direct => "DIRECT",
        }
    }

    /// Returns the lowercase value accepted by Mihomo's `/configs` API.
    #[must_use]
    pub const fn api_value(self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::Global => "global",
            Self::Direct => "direct",
        }
    }

    /// Parses a Mihomo API value, defaulting unknown values to rule mode.
    #[must_use]
    pub fn from_api(mode: &str) -> Self {
        match mode.to_ascii_lowercase().as_str() {
            "global" => Self::Global,
            "direct" => Self::Direct,
            _ => Self::Rule,
        }
    }
}

/// Synchronizes outbound mode and log level from the current controller runtime,
/// discarding stale observations if the runtime or effective configuration changed.
///
/// # Errors
///
/// Returns a [`CoreSessionError`] when the controller observation cannot begin
/// or `/configs` cannot be read.
pub async fn synchronize_runtime_mode(
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::AsyncWriteExt;
    use zenclash_core::{ControlledConfigStore, CoreKind, MihomoClient, MihomoEndpoint};

    use super::*;

    #[test]
    fn outbound_mode_parses_mihomo_values_case_insensitively() {
        assert_eq!(OutboundMode::from_api("GLOBAL"), OutboundMode::Global);
    }

    #[test]
    fn outbound_mode_defaults_unknown_values_to_rule() {
        assert_eq!(OutboundMode::from_api("unexpected"), OutboundMode::Rule);
    }

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
                    old.write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
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
