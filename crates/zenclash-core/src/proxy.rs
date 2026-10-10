use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

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

/// Selection behavior exposed by a Mihomo proxy group.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum ProxyGroupBehavior {
    /// A selector whose current member is controlled explicitly.
    Selector,
    /// A URL-test or fallback group that can be pinned to one member.
    Automatic {
        /// Whether Mihomo currently pins the group instead of selecting automatically.
        fixed: bool,
    },
    /// A load-balancing group without one user-selected current member.
    LoadBalance,
    /// A group type unknown to this ZenClash version.
    Unknown(String),
}

impl Default for ProxyGroupBehavior {
    fn default() -> Self {
        Self::Unknown(String::new())
    }
}

impl ProxyGroupBehavior {
    fn from_mihomo(kind: &str, fixed: bool) -> Self {
        if kind.eq_ignore_ascii_case("selector") {
            Self::Selector
        } else if kind.eq_ignore_ascii_case("urltest") || kind.eq_ignore_ascii_case("fallback") {
            Self::Automatic { fixed }
        } else if kind.eq_ignore_ascii_case("loadbalance") {
            Self::LoadBalance
        } else {
            Self::Unknown(kind.to_owned())
        }
    }
}

/// Controller identity shared by every reference to one provider node.
///
/// Identity uses the controller map key, rather than its potentially duplicated
/// display name. IDs remain comparable across catalog refreshes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProxyNodeId(Arc<NodeIdentity>);

#[derive(Debug, PartialEq, Eq, Hash)]
struct NodeIdentity {
    controller_name: String,
    provider: Option<String>,
}

impl ProxyNodeId {
    /// Builds an identity using a controller map key and its optional provider.
    #[must_use]
    pub fn new(controller_name: String, provider: Option<String>) -> Self {
        Self(Arc::new(NodeIdentity {
            controller_name,
            provider,
        }))
    }

    /// Returns the key accepted by non-provider controller endpoints.
    #[must_use]
    pub fn controller_name(&self) -> &str {
        &self.0.controller_name
    }

    /// Returns the provider identity, absent for controller-local nodes.
    #[must_use]
    pub fn provider(&self) -> Option<&str> {
        self.0.provider.as_deref()
    }
}

/// Selectable Mihomo proxy group with references into its owning catalog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyGroup {
    /// Group name.
    pub name: String,
    /// Mihomo group implementation type.
    pub kind: String,
    /// Domain behavior derived from the Mihomo group type and fixed state.
    pub behavior: ProxyGroupBehavior,
    /// Currently selected member name.
    pub now: String,
    /// Canonical member identities in Mihomo order.
    pub all: Arc<[ProxyNodeId]>,
    /// Optional URL used for group health checks.
    pub test_url: Option<String>,
    /// Whether Mihomo marks this group as hidden.
    pub hidden: bool,
}

/// Resolved proxy groups and aggregate proxy count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyCatalog {
    groups: Vec<ProxyGroup>,
    /// Total raw proxy entries, including group objects.
    pub proxy_count: usize,
    nodes: HashMap<ProxyNodeId, Arc<ProxyNode>>,
    references: Arc<HashMap<ProxyNodeId, Vec<usize>>>,
}

impl ProxyCatalog {
    /// Constructs a catalog from resolved groups, deduplicating controller name and provider.
    ///
    /// Use [`RawProxyCatalog`] for controller responses whose map key differs
    /// from the display name. This constructor uses each node name as its key.
    #[must_use]
    pub fn from_group_nodes(groups: Vec<(ProxyGroup, Vec<ProxyNode>)>, proxy_count: usize) -> Self {
        let mut nodes: HashMap<ProxyNodeId, Arc<ProxyNode>> = HashMap::new();
        let groups = groups
            .into_iter()
            .map(|(mut group, members)| {
                group.all = members
                    .into_iter()
                    .map(|node| {
                        let identity =
                            ProxyNodeId::new(node.name.clone(), node.provider_name.clone());
                        if let Some((id, _)) = nodes.get_key_value(&identity) {
                            return id.clone();
                        }
                        nodes.insert(identity.clone(), Arc::new(node));
                        identity
                    })
                    .collect();
                group
            })
            .collect();
        Self::with_nodes(groups, nodes, proxy_count)
    }

    fn with_nodes(
        groups: Vec<ProxyGroup>,
        nodes: HashMap<ProxyNodeId, Arc<ProxyNode>>,
        proxy_count: usize,
    ) -> Self {
        let references = prepare_references(&groups);
        Self {
            groups,
            nodes,
            references: Arc::new(references),
            proxy_count,
        }
    }

    /// Reads selectable groups whose member identities belong to this catalog.
    #[must_use]
    pub fn groups(&self) -> &[ProxyGroup] {
        &self.groups
    }

    /// Updates confirmed or optimistic selection without changing membership identity.
    /// Returns whether the group exists in this snapshot.
    pub fn set_group_selection(&mut self, group: &str, member: String) -> bool {
        let Some(group) = self.groups.iter_mut().find(|item| item.name == group) else {
            return false;
        };
        group.now = member;
        if matches!(group.behavior, ProxyGroupBehavior::Automatic { .. }) {
            group.behavior = ProxyGroupBehavior::Automatic { fixed: true };
        }
        true
    }

    /// Looks up a prepared node without constructing a string key.
    #[must_use]
    pub fn node(&self, id: &ProxyNodeId) -> Option<&ProxyNode> {
        self.nodes.get(id).map(Arc::as_ref)
    }

    /// Mutates only this snapshot's node, copying its payload only if another snapshot owns it.
    pub fn node_mut(&mut self, id: &ProxyNodeId) -> Option<&mut ProxyNode> {
        self.nodes.get_mut(id).map(Arc::make_mut)
    }

    /// Returns the prepared indices of groups that reference this identity.
    #[must_use]
    pub fn referencing_groups(&self, id: &ProxyNodeId) -> &[usize] {
        self.references.get(id).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn hide_hidden_groups(&mut self) {
        self.groups.retain(|group| !group.hidden);
        let references = prepare_references(&self.groups);
        self.references = Arc::new(references);
    }

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

fn prepare_references(groups: &[ProxyGroup]) -> HashMap<ProxyNodeId, Vec<usize>> {
    let mut references = HashMap::<ProxyNodeId, Vec<usize>>::new();
    for (index, group) in groups.iter().enumerate() {
        for id in group.all.iter() {
            let references = references.entry(id.clone()).or_default();
            if references.last() != Some(&index) {
                references.push(index);
            }
        }
    }
    references
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
#[derive(Debug, Default, Deserialize)]
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
    #[serde(default, deserialize_with = "deserialize_fixed")]
    fixed: bool,
}

fn deserialize_fixed<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;

    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Bool(fixed) => Ok(fixed),
        serde_json::Value::String(member) => Ok(!member.trim().is_empty()),
        serde_json::Value::Null => Ok(false),
        value => Err(D::Error::custom(format!(
            "fixed must be a boolean, member name, or null; got {value}"
        ))),
    }
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
            provider_name: self
                .provider_name
                .as_deref()
                .and_then(|name| (!name.trim().is_empty()).then(|| name.trim().to_owned())),
        }
    }
}

impl From<RawProxyCatalog> for ProxyCatalog {
    fn from(raw: RawProxyCatalog) -> Self {
        let proxy_count = raw.proxies.len();
        // `/proxies` is a JSON object, whose key order is not configuration order.
        // Mihomo builds its synthetic GLOBAL members from the original proxies
        // and proxy-groups declarations, before dependency-sorting the groups.
        // Use controller keys, not display names, and never promote selectors
        // or hidden groups ahead of the subscription's order.
        let mut configured_order = HashMap::new();
        if let Some(global) = raw.proxies.get("GLOBAL") {
            for (position, name) in global.all.iter().enumerate() {
                configured_order.entry(name.clone()).or_insert(position);
            }
        }
        let mut nodes = HashMap::with_capacity(proxy_count);
        let mut identities = HashMap::with_capacity(proxy_count);
        let mut pending_groups = Vec::new();
        for (key, mut proxy) in raw.proxies {
            if !proxy.all.is_empty() {
                let members = std::mem::take(&mut proxy.all);
                let group = ProxyGroup {
                    name: if proxy.name.is_empty() {
                        key.clone()
                    } else {
                        proxy.name.clone()
                    },
                    kind: proxy.kind.clone(),
                    behavior: ProxyGroupBehavior::from_mihomo(&proxy.kind, proxy.fixed),
                    now: std::mem::take(&mut proxy.now),
                    test_url: proxy
                        .test_url
                        .as_deref()
                        .map(str::trim)
                        .filter(|url| !url.is_empty())
                        .map(str::to_owned),
                    hidden: proxy.hidden,
                    ..Default::default()
                };
                let position = configured_order.get(&key).copied().unwrap_or(usize::MAX);
                pending_groups.push((position, group, members));
            }
            let node = proxy.into_node(&key);
            let id = ProxyNodeId::new(key.clone(), node.provider_name.clone());
            identities.insert(key, id.clone());
            nodes.insert(id, Arc::new(node));
        }
        // Stable fallback for controllers with no complete GLOBAL membership:
        // retain map-key order for unknown groups rather than guessing intent.
        pending_groups.sort_by_key(|(position, _, _)| *position);
        let mut groups = Vec::with_capacity(pending_groups.len());
        for (_, mut group, members) in pending_groups {
            group.all = members
                .into_iter()
                .map(|name| {
                    if let Some(id) = identities.get(&name) {
                        return id.clone();
                    }
                    let id = ProxyNodeId::new(name.clone(), None);
                    nodes.insert(
                        id.clone(),
                        Arc::new(ProxyNode {
                            name: name.clone(),
                            ..Default::default()
                        }),
                    );
                    identities.insert(name, id.clone());
                    id
                })
                .collect();
            groups.push(group);
        }
        Self::with_nodes(groups, nodes, proxy_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_group_order_is_preserved_instead_of_name_or_selector_priority() {
        // Mihomo's map keys are alphabetic; GLOBAL.all carries the configured
        // order, deliberately different from both alphabetic and selector order.
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{"proxies":{
                "GLOBAL":{"type":"Selector","all":["DIRECT","z-stream","a-backup","Proxy"]},
                "Proxy":{"type":"Selector","all":["DIRECT"]},
                "a-backup":{"type":"Selector","all":["DIRECT"]},
                "z-stream":{"type":"Selector","all":["DIRECT"]}
            }}"#,
        )
        .unwrap();
        let catalog = ProxyCatalog::from(raw);
        assert_eq!(
            catalog
                .groups_for_mode("rule")
                .map(|group| group.name.as_str())
                .collect::<Vec<_>>(),
            ["z-stream", "a-backup", "Proxy"]
        );
    }

    #[test]
    fn configured_order_uses_controller_keys_and_hidden_filter_keeps_relative_order() {
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{"proxies":{
                "GLOBAL":{"type":"Selector","all":["z-key","hidden","a-key"]},
                "a-key":{"name":"First alphabetically","type":"Selector","all":["DIRECT"]},
                "hidden":{"type":"Selector","hidden":true,"all":["DIRECT"]},
                "z-key":{"name":"Last alphabetically","type":"Selector","all":["DIRECT"]}
            }}"#,
        )
        .unwrap();
        let mut catalog = ProxyCatalog::from(raw);
        let names = |catalog: &ProxyCatalog| {
            catalog
                .groups_for_mode("rule")
                .map(|group| group.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(&catalog),
            ["Last alphabetically", "hidden", "First alphabetically"]
        );
        catalog.hide_hidden_groups();
        assert_eq!(
            names(&catalog),
            ["Last alphabetically", "First alphabetically"]
        );
        let id = &catalog.groups_for_mode("rule").next().unwrap().all[0];
        assert_eq!(catalog.referencing_groups(id), [0, 1]);
    }

    #[test]
    fn group_members_keep_declared_order_instead_of_controller_map_order() {
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{"proxies":{
                "Proxy":{"type":"Selector","all":["z-node","missing","a-node","z-node"]},
                "a-node":{"provider-name":"subscription"},
                "z-node":{"provider-name":"subscription"}
            }}"#,
        )
        .unwrap();
        let catalog = ProxyCatalog::from(raw);
        assert_eq!(
            catalog.groups()[0]
                .all
                .iter()
                .map(ProxyNodeId::controller_name)
                .collect::<Vec<_>>(),
            ["z-node", "missing", "a-node", "z-node"]
        );
    }

    #[test]
    fn repeated_group_members_resolve_to_one_canonical_node_allocation() {
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{"proxies":{"HK":{"name":"HK","type":"Shadowsocks","provider-name":"Airport A","history":[{"delay":100}]},"first":{"name":"first","type":"Selector","all":["HK"]},"second":{"name":"second","type":"Selector","all":["HK"]}}}"#,
        ).unwrap();
        let catalog = ProxyCatalog::from(raw);
        let first = catalog.node(&catalog.groups[0].all[0]).unwrap();
        let second = catalog.node(&catalog.groups[1].all[0]).unwrap();
        assert_eq!(first, second);
        assert!(
            std::ptr::eq(first, second),
            "one Mihomo node was allocated separately for each group"
        );
    }

    #[test]
    fn canonical_identity_uses_controller_key_and_provider_and_preserves_missing_members() {
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{"proxies":{"a":{"name":"HK","provider-name":"Airport A","history":[{"delay":42}]},"b":{"name":"HK","provider-name":"Airport B","history":[{"delay":77}]},"first":{"type":"Selector","all":["a","b","missing"]},"second":{"type":"Selector","all":["a","missing"]}}}"#,
        ).unwrap();
        let catalog = ProxyCatalog::from(raw);
        let first = &catalog.groups[0].all;
        let second = &catalog.groups[1].all;
        assert_ne!(first[0], first[1]);
        assert_eq!(first[0].controller_name(), "a");
        assert_eq!(first[0].provider(), Some("Airport A"));
        assert_eq!(catalog.node(&first[0]).unwrap().name, "HK");
        assert_eq!(catalog.node(&first[1]).unwrap().latest_delay(), Some(77));
        assert!(std::ptr::eq(
            catalog.node(&first[2]).unwrap(),
            catalog.node(&second[1]).unwrap()
        ));
        assert_eq!(catalog.node(&first[2]).unwrap().name, "missing");
        assert_eq!(catalog.referencing_groups(&first[0]), [0, 1]);
    }

    #[test]
    fn hiding_groups_rebases_the_prepared_reference_indices() {
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{"proxies":{"HK":{"history":[{"delay":42}]},"GLOBAL":{"type":"Selector","hidden":true,"all":["HK"]},"visible":{"type":"Selector","all":["HK"]}}}"#,
        ).unwrap();
        let mut catalog = ProxyCatalog::from(raw);
        let id = catalog.groups[0].all[0].clone();
        assert_eq!(catalog.referencing_groups(&id), [0, 1]);
        catalog.hide_hidden_groups();
        assert_eq!(catalog.groups.len(), 1);
        assert_eq!(catalog.groups[0].name, "visible");
        assert_eq!(catalog.referencing_groups(&id), [0]);
    }

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
        assert_eq!(
            catalog
                .node(&catalog.groups[0].all[0])
                .unwrap()
                .latest_delay(),
            Some(42)
        );
        assert_eq!(
            catalog
                .node(&catalog.groups[0].all[1])
                .unwrap()
                .capabilities()
                .collect::<Vec<_>>(),
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
    fn automatic_group_preserves_fixed_state() {
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Auto":{"name":"Auto","type":"URLTest","now":"DIRECT","all":["DIRECT"],"fixed":true}}}"#,
        )
        .unwrap();

        let catalog = ProxyCatalog::from(raw);

        assert_eq!(
            catalog.groups[0].behavior,
            ProxyGroupBehavior::Automatic { fixed: true }
        );
    }

    #[test]
    fn automatic_group_accepts_real_mihomo_fixed_member_encoding() {
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Auto":{"name":"Auto","type":"URLTest","now":"DIRECT","all":["DIRECT"],"fixed":"DIRECT"}}}"#,
        )
        .unwrap();

        let catalog = ProxyCatalog::from(raw);

        assert_eq!(
            catalog.groups[0].behavior,
            ProxyGroupBehavior::Automatic { fixed: true }
        );
    }

    #[test]
    fn selector_group_has_explicit_selection_behavior() {
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Proxy":{"name":"Proxy","type":"Selector","now":"DIRECT","all":["DIRECT"]}}}"#,
        )
        .unwrap();

        let catalog = ProxyCatalog::from(raw);

        assert_eq!(catalog.groups[0].behavior, ProxyGroupBehavior::Selector);
    }

    #[test]
    fn fallback_group_without_a_pin_remains_automatic() {
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Fallback":{"name":"Fallback","type":"Fallback","now":"DIRECT","all":["DIRECT"]}}}"#,
        )
        .unwrap();

        let catalog = ProxyCatalog::from(raw);

        assert_eq!(
            catalog.groups[0].behavior,
            ProxyGroupBehavior::Automatic { fixed: false }
        );
    }

    #[test]
    fn load_balance_group_has_non_selectable_behavior() {
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Balance":{"name":"Balance","type":"LoadBalance","now":"DIRECT","all":["DIRECT"]}}}"#,
        )
        .unwrap();

        let catalog = ProxyCatalog::from(raw);

        assert_eq!(catalog.groups[0].behavior, ProxyGroupBehavior::LoadBalance);
    }

    #[test]
    fn unknown_group_behavior_preserves_the_mihomo_type() {
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Future":{"name":"Future","type":"ConsistentHashV2","now":"DIRECT","all":["DIRECT"]}}}"#,
        )
        .unwrap();

        let catalog = ProxyCatalog::from(raw);

        assert_eq!(
            catalog.groups[0].behavior,
            ProxyGroupBehavior::Unknown("ConsistentHashV2".into())
        );
    }

    #[test]
    fn hidden_group_state_is_preserved_for_visibility_filtering() {
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Internal":{"name":"Internal","type":"Selector","now":"DIRECT","all":["DIRECT"],"hidden":true}}}"#,
        )
        .unwrap();

        let catalog = ProxyCatalog::from(raw);

        assert!(catalog.groups[0].hidden);
    }

    #[test]
    fn blank_provider_name_uses_the_regular_proxy_delay_endpoint() {
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct","provider-name":"  "},"Proxy":{"name":"Proxy","type":"Selector","now":"DIRECT","all":["DIRECT"]}}}"#,
        )
        .unwrap();

        let catalog = ProxyCatalog::from(raw);

        assert_eq!(
            catalog
                .node(&catalog.groups[0].all[0])
                .unwrap()
                .provider_name,
            None
        );
    }

    #[test]
    fn provider_name_is_trimmed_without_losing_provider_routing() {
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{"proxies":{"HK":{"name":"HK","type":"Shadowsocks","provider-name":" Airport A "},"Proxy":{"name":"Proxy","type":"Selector","now":"HK","all":["HK"]}}}"#,
        )
        .unwrap();

        let catalog = ProxyCatalog::from(raw);

        assert_eq!(
            catalog
                .node(&catalog.groups[0].all[0])
                .unwrap()
                .provider_name
                .as_deref(),
            Some("Airport A")
        );
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

    #[test]
    fn missing_configured_order_does_not_guess_a_primary_selector() {
        let raw: RawProxyCatalog = serde_json::from_str(
            r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"OneDrive":{"name":"OneDrive","type":"Selector","all":["DIRECT"]},"🔰 选择节点":{"name":"🔰 选择节点","type":"Selector","all":["DIRECT"]},"GLOBAL":{"name":"GLOBAL","type":"Selector","all":["DIRECT"]}}}"#,
        )
        .unwrap();
        let catalog = ProxyCatalog::from(raw);

        assert_eq!(
            catalog
                .groups_for_mode("rule")
                .next()
                .map(|group| group.name.as_str()),
            Some("OneDrive")
        );
    }

    fn mode_catalog() -> ProxyCatalog {
        ProxyCatalog::from_group_nodes(
            ["GLOBAL", "Proxy", "Streaming"]
                .into_iter()
                .map(|name| {
                    (
                        ProxyGroup {
                            name: name.into(),
                            ..ProxyGroup::default()
                        },
                        Vec::new(),
                    )
                })
                .collect(),
            3,
        )
    }
}
