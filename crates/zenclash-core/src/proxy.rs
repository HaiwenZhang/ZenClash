use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One latency sample returned inside Mihomo's proxy history.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct DelayHistory {
    /// Mihomo-provided sample timestamp.
    #[serde(default)]
    pub time: String,
    /// Latest measured delay in milliseconds.
    #[serde(default)]
    pub delay: u32,
    /// Mean measured delay in milliseconds.
    #[serde(default, rename = "meanDelay")]
    pub mean_delay: u32,
}

/// Delay response returned by Mihomo's proxy health-check API.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct DelayResult {
    /// Latest measured delay in milliseconds.
    #[serde(default)]
    pub delay: u32,
    /// Mean measured delay in milliseconds.
    #[serde(default, rename = "meanDelay")]
    pub mean_delay: u32,
}

/// A proxy or a nested proxy group as exposed by `/proxies`.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProxyNode {
    /// Mihomo proxy name.
    #[serde(default)]
    pub name: String,
    /// Mihomo proxy implementation type.
    #[serde(default, rename = "type")]
    pub kind: String,
    /// Latest health state, when Mihomo provides one.
    #[serde(default)]
    pub alive: Option<bool>,
    /// Whether the proxy supports UDP forwarding.
    #[serde(default)]
    pub udp: bool,
    /// Whether the proxy supports XUDP.
    #[serde(default)]
    pub xudp: bool,
    /// Whether TCP Fast Open is enabled.
    #[serde(default)]
    pub tfo: bool,
    /// Whether Multipath TCP is enabled.
    #[serde(default)]
    pub mptcp: bool,
    /// Whether protocol multiplexing is enabled.
    #[serde(default)]
    pub smux: bool,
    /// Delay history reported by Mihomo and local checks.
    #[serde(default)]
    pub history: Vec<DelayHistory>,
    /// Provider that supplied this proxy, when applicable.
    #[serde(default, rename = "provider-name")]
    pub provider_name: Option<String>,
}

impl ProxyNode {
    /// Returns the most recent delay sample.
    #[must_use]
    pub fn latest_delay(&self) -> Option<u32> {
        self.history.last().map(|sample| sample.delay)
    }

    /// Iterates over enabled transport capability labels.
    pub fn capabilities(&self) -> impl Iterator<Item = &'static str> {
        [
            (self.udp, "UDP"),
            (self.xudp, "XUDP"),
            (self.tfo, "TFO"),
            (self.mptcp, "MPTCP"),
            (self.smux, "SMUX"),
        ]
        .into_iter()
        .filter_map(|(enabled, label)| enabled.then_some(label))
    }
}

/// Selectable Mihomo proxy group with its resolved member nodes.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProxyGroup {
    /// Group name.
    pub name: String,
    /// Mihomo group implementation type.
    pub kind: String,
    /// Currently selected member name.
    pub now: String,
    /// Resolved member nodes in Mihomo order.
    pub all: Vec<ProxyNode>,
    /// Optional URL used for group health checks.
    pub test_url: Option<String>,
    /// Whether Mihomo marks this group as hidden.
    pub hidden: bool,
}

/// Resolved proxy groups and aggregate proxy count.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProxyCatalog {
    /// Selectable groups exposed by Mihomo.
    pub groups: Vec<ProxyGroup>,
    /// Total raw proxy entries, including group objects.
    pub proxy_count: usize,
}

impl ProxyCatalog {
    /// Iterates over the strategy groups relevant to one outbound mode.
    ///
    /// Rule mode exposes profile-defined groups but not Mihomo's synthetic
    /// `GLOBAL` selector. Global mode exposes only that selector, while direct
    /// mode has no selectable proxy group.
    pub fn groups_for_mode<'a>(
        &'a self,
        mode: &'a str,
    ) -> impl Iterator<Item = &'a ProxyGroup> + 'a {
        self.groups
            .iter()
            .filter(move |group| group_visible_in_mode(&group.name, mode))
    }

    /// Consumes the catalog and yields the strategy groups for one mode.
    pub fn into_groups_for_mode(self, mode: &str) -> impl Iterator<Item = ProxyGroup> + '_ {
        self.groups
            .into_iter()
            .filter(move |group| group_visible_in_mode(&group.name, mode))
    }
}

fn group_visible_in_mode(group: &str, mode: &str) -> bool {
    if mode.eq_ignore_ascii_case("direct") {
        false
    } else if mode.eq_ignore_ascii_case("global") {
        group.eq_ignore_ascii_case("GLOBAL")
    } else {
        !group.eq_ignore_ascii_case("GLOBAL")
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct RawProxyCatalog {
    #[serde(default)]
    proxies: BTreeMap<String, RawProxy>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, Deserialize)]
struct RawProxy {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    now: String,
    #[serde(default)]
    all: Vec<String>,
    #[serde(default)]
    alive: Option<bool>,
    #[serde(default)]
    udp: bool,
    #[serde(default)]
    xudp: bool,
    #[serde(default)]
    tfo: bool,
    #[serde(default)]
    mptcp: bool,
    #[serde(default)]
    smux: bool,
    #[serde(default)]
    history: Vec<DelayHistory>,
    #[serde(default, rename = "provider-name")]
    provider_name: Option<String>,
    #[serde(default, alias = "testUrl", rename = "test-url")]
    test_url: Option<String>,
    #[serde(default)]
    hidden: bool,
}

impl RawProxy {
    fn into_node(self, fallback_name: &str) -> ProxyNode {
        ProxyNode {
            name: if self.name.is_empty() {
                fallback_name.to_owned()
            } else {
                self.name
            },
            kind: self.kind,
            alive: self.alive,
            udp: self.udp,
            xudp: self.xudp,
            tfo: self.tfo,
            mptcp: self.mptcp,
            smux: self.smux,
            history: self.history,
            provider_name: self.provider_name,
        }
    }
}

impl From<RawProxyCatalog> for ProxyCatalog {
    fn from(raw: RawProxyCatalog) -> Self {
        let proxy_count = raw.proxies.len();
        let mut groups = Vec::new();

        for (key, proxy) in &raw.proxies {
            if proxy.all.is_empty() {
                continue;
            }

            let all = proxy
                .all
                .iter()
                .map(|name| {
                    raw.proxies
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| RawProxy {
                            name: name.clone(),
                            ..Default::default()
                        })
                        .into_node(name)
                })
                .collect();

            groups.push(ProxyGroup {
                name: if proxy.name.is_empty() {
                    key.clone()
                } else {
                    proxy.name.clone()
                },
                kind: proxy.kind.clone(),
                now: proxy.now.clone(),
                all,
                test_url: proxy
                    .test_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|url| !url.is_empty())
                    .map(str::to_owned),
                hidden: proxy.hidden,
            });
        }

        Self {
            groups,
            proxy_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_group_members_from_mihomo_catalog() {
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{
                "proxies": {
                    "DIRECT": {"name":"DIRECT","type":"Direct","alive":true,"udp":true,"history":[]},
                    "HK 01": {"name":"HK 01","type":"Shadowsocks","alive":true,"history":[{"time":"now","delay":42}]},
                    "Proxy": {"name":"Proxy","type":"Selector","now":"HK 01","all":["HK 01","DIRECT"],"test-url":"https://example.com"}
                }
            }"#,
        )
        .unwrap();

        let catalog = ProxyCatalog::from(raw);
        assert_eq!(catalog.proxy_count, 3);
        assert_eq!(catalog.groups.len(), 1);
        assert_eq!(catalog.groups[0].name, "Proxy");
        assert_eq!(catalog.groups[0].now, "HK 01");
        assert_eq!(catalog.groups[0].all[0].latest_delay(), Some(42));
        assert_eq!(
            catalog.groups[0].all[1].capabilities().collect::<Vec<_>>(),
            vec!["UDP"]
        );
    }

    #[test]
    fn empty_group_test_url_uses_the_client_default() {
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Proxy":{"name":"Proxy","type":"Selector","now":"DIRECT","all":["DIRECT"],"testUrl":""}}}"#,
        )
        .unwrap();

        let catalog = ProxyCatalog::from(raw);

        assert_eq!(catalog.groups[0].test_url, None);
    }

    #[test]
    fn rule_mode_excludes_the_synthetic_global_group() {
        let catalog = mode_catalog();

        let names = catalog
            .groups_for_mode("rule")
            .map(|group| group.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["Proxy", "Streaming"]);
    }

    #[test]
    fn global_mode_exposes_only_the_synthetic_global_group() {
        let catalog = mode_catalog();

        let names = catalog
            .groups_for_mode("GLOBAL")
            .map(|group| group.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["GLOBAL"]);
    }

    #[test]
    fn direct_mode_has_no_selectable_proxy_groups() {
        let catalog = mode_catalog();

        assert_eq!(catalog.groups_for_mode("direct").count(), 0);
    }

    fn mode_catalog() -> ProxyCatalog {
        ProxyCatalog {
            groups: ["GLOBAL", "Proxy", "Streaming"]
                .into_iter()
                .map(|name| ProxyGroup {
                    name: name.into(),
                    ..ProxyGroup::default()
                })
                .collect(),
            proxy_count: 3,
        }
    }
}
