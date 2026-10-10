//! A temporary macOS resolver owned by the privileged core's lifetime.
//!
//! # CRITICAL REGRESSION: macOS LAN DNS bypasses TUN (2026-10-10)
//!
//! 重要回归警示：这个容易忽视的 DNS 问题经过长时间排查才定位，请勿省略 DNS 接管与恢复。
//! System proxy worked, but Google and ChatGPT model calls failed under TUN,
//! despite a running core, a working utun route and `dns-hijack: [any:53]`.
//! macOS can still send system DNS queries directly to the LAN router's IPv4
//! or IPv6 resolver, outside Mihomo's TUN DNS hijack. Polluted answers then
//! caused TLS failures and connections with no domain metadata. A healthy
//! TUN interface or valid Mihomo DNS configuration alone cannot detect this.
//!
//! Sending DNS to 1.1.1.1 through TUN returned Fake-IP addresses; using those
//! addresses made the same failing HTTPS requests succeed. This resolver
//! lease makes normal system lookups follow that working path. The public
//! address MUST be covered by the active TUN routes and UDP DNS hijack.
//!
//! Preserve acquisition on start, config readback and watchdog recovery, and
//! release on disable, stop and unexpected exit. Keep the lease temporary
//! and service-owned; do not replace it with permanent DNS preference edits.
//! Validate normal browser/model requests with system proxy off, then verify
//! original DNS restoration on disable. `dig @1.1.1.1` or `curl --resolve`
//! alone bypasses the failing system resolver and can hide this regression.
//! Evidence and acceptance: docs/research/clash-verge-tun-implementation.md, §11.

#[cfg(all(target_os = "macos", not(feature = "test")))]
use once_cell::sync::Lazy;
#[cfg(any(all(target_os = "macos", not(feature = "test")), test))]
use std::sync::mpsc;

#[cfg(any(all(target_os = "macos", not(feature = "test")), test))]
struct DnsController {
    requests: mpsc::Sender<(bool, tokio::sync::oneshot::Sender<anyhow::Result<()>>)>,
}

#[cfg(any(all(target_os = "macos", not(feature = "test")), test))]
impl DnsController {
    fn new<B: 'static>(
        create: impl FnOnce() -> B + Send + 'static,
        mut apply: impl FnMut(&mut B, bool) -> anyhow::Result<()> + Send + 'static,
    ) -> std::io::Result<Self> {
        let (requests, receiver) = mpsc::channel::<(bool, tokio::sync::oneshot::Sender<anyhow::Result<()>>)>();
        // The native store is created, used and released on this single thread.
        // Enqueue before awaiting so cancellation cannot reorder enable after stop.
        std::thread::Builder::new()
            .name("zenclash-tun-dns".into())
            .spawn(move || {
                let mut backend = create();
                for (enabled, reply) in receiver {
                    let _ = reply.send(apply(&mut backend, enabled));
                }
            })?;
        Ok(Self { requests })
    }

    fn enqueue(&self, enabled: bool) -> anyhow::Result<tokio::sync::oneshot::Receiver<anyhow::Result<()>>> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.requests
            .send((enabled, reply))
            .map_err(|_| anyhow::anyhow!("TUN DNS worker stopped"))?;
        Ok(result)
    }
}

#[cfg(all(target_os = "macos", not(feature = "test")))]
static CONTROLLER: Lazy<std::io::Result<DnsController>> =
    Lazy::new(|| DnsController::new(native::DnsLease::default, native::DnsLease::apply));

#[cfg(feature = "test")]
static SIMULATED_DNS_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Observes the resolver lease in isolated IPC fixtures; it never writes host DNS.
#[cfg(feature = "test")]
pub fn test_tun_dns_enabled() -> bool {
    SIMULATED_DNS_ENABLED.load(std::sync::atomic::Ordering::Acquire)
}

pub(super) async fn apply(enabled: bool) -> anyhow::Result<()> {
    #[cfg(feature = "test")]
    SIMULATED_DNS_ENABLED.store(enabled, std::sync::atomic::Ordering::Release);
    #[cfg(all(target_os = "macos", not(feature = "test")))]
    {
        let controller = CONTROLLER
            .as_ref()
            .map_err(|error| anyhow::anyhow!("cannot start TUN DNS worker: {error}"))?;
        controller
            .enqueue(enabled)?
            .await
            .map_err(|_| anyhow::anyhow!("TUN DNS worker lost its response"))??;
    }
    #[cfg(not(all(target_os = "macos", not(feature = "test"))))]
    let _ = enabled; // Simulated service fixtures must never change the host resolver.
    Ok(())
}

/// Starting and watchdog recovery have no GUI readback to trigger DNS setup.
pub(super) async fn apply_for_config(path: &str) -> anyhow::Result<()> {
    #[cfg(any(target_os = "macos", feature = "test"))]
    {
        use anyhow::{Context as _, ensure};
        use tokio::io::AsyncReadExt as _;
        let file = tokio::fs::File::open(path)
            .await
            .context("cannot read owned TUN configuration")?;
        let mut bytes = Vec::new();
        file.take(24 * 1024 * 1024 + 1).read_to_end(&mut bytes).await?;
        ensure!(bytes.len() <= 24 * 1024 * 1024, "TUN configuration exceeds size limit");
        let mut config: serde_yaml::Value = serde_yaml::from_slice(&bytes)?;
        config.apply_merge()?;
        let tun = &config["tun"];
        let enabled = tun["enable"].as_bool() == Some(true)
            && tun["auto-route"].as_bool() != Some(false)
            && tun["dns-hijack"].as_sequence().is_none_or(|entries| {
                entries.iter().any(|entry| {
                    matches!(
                        entry.as_str(),
                        Some(
                            "any:53"
                                | "udp://any:53"
                                | "0.0.0.0:53"
                                | "udp://0.0.0.0:53"
                                | "1.1.1.1:53"
                                | "udp://1.1.1.1:53"
                        )
                    )
                })
            });
        apply(enabled).await?;
    }
    #[cfg(not(any(target_os = "macos", feature = "test")))]
    let _ = path;
    Ok(())
}

/// Cancellation or a watchdog panic must also retire the temporary resolver.
pub(super) struct RetirementGuard;

impl Drop for RetirementGuard {
    fn drop(&mut self) {
        #[cfg(all(target_os = "macos", not(feature = "test")))]
        if let Some(Ok(controller)) = Lazy::get(&CONTROLLER) {
            let _ = controller.enqueue(false);
        }
    }
}

#[cfg(all(target_os = "macos", not(feature = "test")))]
mod native {
    use anyhow::{Context as _, ensure};
    use system_configuration::{
        core_foundation::{
            array::CFArray,
            base::{CFType, TCFType},
            dictionary::CFDictionary,
            number::CFNumber,
            string::CFString,
        },
        dynamic_store::{SCDynamicStore, SCDynamicStoreBuilder},
        sys::dynamic_store::SCDynamicStoreAddTemporaryValue,
    };

    #[derive(Default)]
    pub(super) struct DnsLease {
        store: Option<(SCDynamicStore, CFDictionary<CFString, CFType>)>,
    }

    fn key() -> CFString {
        CFString::from_static_string(if cfg!(feature = "development-channel") {
            "State:/Network/Service/org.zenclash.app.dev.tun-dns/DNS"
        } else {
            "State:/Network/Service/org.zenclash.app.tun-dns/DNS"
        })
    }

    impl DnsLease {
        pub(super) fn apply(&mut self, enabled: bool) -> anyhow::Result<()> {
            if !enabled {
                if let Some((store, expected)) = self.store.as_ref() {
                    // A configd restart may already have discarded session keys.
                    if let Some(current) = store.get(key())
                        && current.as_CFType() != expected.as_CFType()
                    {
                        self.store.take();
                        anyhow::bail!("macOS TUN DNS resolver was replaced by another session");
                    }
                    ensure!(
                        store.get(key()).is_none() || store.remove(key()),
                        "cannot remove owned macOS TUN DNS resolver"
                    );
                    self.store.take();
                }
                return Ok(());
            }
            if let Some((store, expected)) = self.store.as_ref()
                && let Some(current) = store.get(key())
            {
                ensure!(
                    current.as_CFType() == expected.as_CFType(),
                    "macOS TUN DNS resolver was replaced by another session"
                );
                return Ok(());
            }
            self.store.take();
            let store = SCDynamicStoreBuilder::new("ZenClash TUN DNS")
                .build()
                .context("cannot open macOS dynamic store")?;
            let servers = CFArray::from_CFTypes(&[CFString::from_static_string("1.1.1.1")]);
            let domains = CFArray::from_CFTypes(&[CFString::from_static_string("")]);
            let orders = CFArray::from_CFTypes(&[CFNumber::from(1_i32)]);
            let resolver = CFDictionary::from_CFType_pairs(&[
                (CFString::from_static_string("ServerAddresses"), servers.as_CFType()),
                (
                    CFString::from_static_string("SupplementalMatchDomains"),
                    domains.as_CFType(),
                ),
                (
                    CFString::from_static_string("SupplementalMatchOrders"),
                    orders.as_CFType(),
                ),
                (
                    CFString::from_static_string("SupplementalMatchDomainsNoSearch"),
                    CFNumber::from(1_i32).as_CFType(),
                ),
                (
                    CFString::from_static_string("InterfaceName"),
                    CFString::from_static_string("*").as_CFType(),
                ),
            ]);
            // SAFETY: all references remain valid for this synchronous call.
            // Add (rather than Set) refuses an existing resolver owned by another
            // session; configd removes our key even if the helper is SIGKILLed.
            let added = unsafe {
                SCDynamicStoreAddTemporaryValue(
                    store.as_concrete_TypeRef(),
                    key().as_concrete_TypeRef(),
                    resolver.as_CFTypeRef(),
                )
            };
            ensure!(added != 0, "cannot acquire temporary macOS TUN DNS resolver");
            self.store = Some((store, resolver));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DnsController;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn cancelled_enable_is_retired_before_stop_acknowledgement() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        let controller = DnsController::new(
            || false,
            move |active, enabled| {
                if *active != enabled {
                    observed.lock().unwrap().push(enabled);
                    *active = enabled;
                }
                Ok(())
            },
        )
        .unwrap();
        drop(controller.enqueue(true).unwrap()); // Lost handler response.
        controller.enqueue(false).unwrap().await.unwrap().unwrap();
        assert_eq!(*events.lock().unwrap(), [true, false]);
    }

    #[tokio::test]
    async fn failed_acquisition_can_be_retried_and_released() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        let controller = DnsController::new(
            || (false, true),
            move |(active, fail), enabled| {
                if enabled && *fail {
                    *fail = false;
                    anyhow::bail!("write refused");
                }
                if *active != enabled {
                    observed.lock().unwrap().push(enabled);
                    *active = enabled;
                }
                Ok(())
            },
        )
        .unwrap();
        assert!(controller.enqueue(true).unwrap().await.unwrap().is_err());
        controller.enqueue(true).unwrap().await.unwrap().unwrap();
        controller.enqueue(true).unwrap().await.unwrap().unwrap();
        controller.enqueue(false).unwrap().await.unwrap().unwrap();
        assert_eq!(*events.lock().unwrap(), [true, false]);
    }
}
