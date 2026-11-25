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
    pub(crate) fn from_mihomo(kind: &str, fixed: bool) -> Self {
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

/// Controller evidence for one group's actual member and selection behavior.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyGroupSelectionObservation {
    member: String,
    behavior: Option<ProxyGroupBehavior>,
}

impl ProxyGroupSelectionObservation {
    pub(crate) fn new(member: String, behavior: Option<ProxyGroupBehavior>) -> Self {
        Self { member, behavior }
    }

    /// Returns the member reported by the controller, empty when none was reported.
    #[must_use]
    pub fn member(&self) -> &str {
        &self.member
    }

    /// Returns behavior only when the response included the group's implementation type.
    #[must_use]
    pub fn behavior(&self) -> Option<&ProxyGroupBehavior> {
        self.behavior.as_ref()
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
#[non_exhaustive]
pub struct ProxyGroup {
    controller_name: String,
    /// User-facing label; controller commands use [`Self::controller_name`].
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

impl ProxyGroup {
    /// Creates a group whose display label initially matches its controller map key.
    #[must_use]
    pub fn new(controller_name: impl Into<String>) -> Self {
        let controller_name = controller_name.into();
        Self {
            name: controller_name.clone(),
            controller_name,
            ..Self::default()
        }
    }
    /// Returns the stable key accepted by controller group endpoints.
    #[must_use]
    pub fn controller_name(&self) -> &str {
        if self.controller_name.is_empty() {
            &self.name
        } else {
            &self.controller_name
        }
    }
    /// Sets the display label without changing controller identity.
    #[must_use]
    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
    /// Sets the raw controller implementation type.
    #[must_use]
    pub fn with_kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = kind.into();
        self
    }
    /// Sets the observed selection behavior.
    #[must_use]
    pub fn with_behavior(mut self, behavior: ProxyGroupBehavior) -> Self {
        self.behavior = behavior;
        self
    }
    /// Sets a member observed in the initial snapshot.
    #[must_use]
    pub fn with_current_member(mut self, member: impl Into<String>) -> Self {
        self.now = member.into();
        self
    }
    /// Sets canonical member references for catalog construction.
    #[must_use]
    pub fn with_members(mut self, members: Arc<[ProxyNodeId]>) -> Self {
        self.all = members;
        self
    }
    /// Sets the group health-check URL.
    #[must_use]
    pub fn with_test_url(mut self, test_url: Option<String>) -> Self {
        self.test_url = test_url;
        self
    }
    /// Sets whether normal catalog consumers hide this group.
    #[must_use]
    pub fn with_hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }
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
    /// This constructor uses each node name as its controller key.
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
        let Some(group) = self
            .groups
            .iter_mut()
            .find(|item| item.controller_name() == group)
        else {
            return false;
        };
        group.now = member;
        if matches!(group.behavior, ProxyGroupBehavior::Automatic { .. }) {
            group.behavior = ProxyGroupBehavior::Automatic { fixed: true };
        }
        true
    }

    /// Applies only actual member and behavior present in a controller observation.
    /// Returns whether the group exists, preserving membership identity.
    pub fn apply_group_observation(
        &mut self,
        group: &str,
        observation: &ProxyGroupSelectionObservation,
    ) -> bool {
        let Some(group) = self
            .groups
            .iter_mut()
            .find(|item| item.controller_name() == group)
        else {
            return false;
        };
        group.now = observation.member().to_owned();
        if let Some(behavior) = observation.behavior() {
            group.behavior = behavior.clone();
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
            .filter(move |group| group_visible_in_mode(group.controller_name(), mode))
    }

    /// Consumes the catalog and yields the strategy groups for one mode.
    pub fn into_groups_for_mode(self, mode: &str) -> impl Iterator<Item = ProxyGroup> + '_ {
        self.groups
            .into_iter()
            .filter(move |group| group_visible_in_mode(group.controller_name(), mode))
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
    #[serde(
        default,
        deserialize_with = "crate::mihomo::models::deserialize_null_default"
    )]
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
    #[serde(
        default,
        deserialize_with = "crate::mihomo::models::deserialize_null_default"
    )]
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

pub(crate) fn deserialize_fixed<'de, D>(deserializer: D) -> Result<bool, D::Error>
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
                    controller_name: key.clone(),
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
mod tests;
