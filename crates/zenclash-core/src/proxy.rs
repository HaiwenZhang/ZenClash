use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One latency sample returned inside Mihomo's proxy history.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct DelayHistory {
    #[serde(default)]
    pub time: String,
    #[serde(default)]
    pub delay: u32,
    #[serde(default, rename = "meanDelay")]
    pub mean_delay: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct DelayResult {
    #[serde(default)]
    pub delay: u32,
    #[serde(default, rename = "meanDelay")]
    pub mean_delay: u32,
}

/// A proxy or a nested proxy group as exposed by `/proxies`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProxyNode {
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub alive: Option<bool>,
    #[serde(default)]
    pub udp: bool,
    #[serde(default)]
    pub xudp: bool,
    #[serde(default)]
    pub tfo: bool,
    #[serde(default)]
    pub mptcp: bool,
    #[serde(default)]
    pub smux: bool,
    #[serde(default)]
    pub history: Vec<DelayHistory>,
    #[serde(default, rename = "provider-name")]
    pub provider_name: Option<String>,
}

impl ProxyNode {
    pub fn latest_delay(&self) -> Option<u32> {
        self.history.last().map(|sample| sample.delay)
    }

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

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProxyGroup {
    pub name: String,
    pub kind: String,
    pub now: String,
    pub all: Vec<ProxyNode>,
    pub test_url: Option<String>,
    pub hidden: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProxyCatalog {
    pub groups: Vec<ProxyGroup>,
    pub proxy_count: usize,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawProxyCatalog {
    #[serde(default)]
    proxies: BTreeMap<String, RawProxy>,
}

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
                test_url: proxy.test_url.clone(),
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
}
