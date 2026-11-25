use std::{
    collections::{BTreeMap, HashSet},
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    str::FromStr,
};

use serde_yaml::{Mapping, Value};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};

const MIN_FALLBACK_PORT: u16 = 1025;
const LISTENERS: [ListenerSpec; 5] = [
    ListenerSpec::new("mixed-port", true),
    ListenerSpec::new("socks-port", true),
    ListenerSpec::new("port", false),
    ListenerSpec::new("redir-port", false),
    ListenerSpec::new("tproxy-port", true),
];

#[derive(Clone, Copy)]
struct ListenerSpec {
    key: &'static str,
    udp: bool,
}

impl ListenerSpec {
    const fn new(key: &'static str, udp: bool) -> Self {
        Self { key, udp }
    }
}

/// One listener port moved for the current managed-core session only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListenerPortFallback {
    /// Clash/Mihomo configuration key, such as `mixed-port`.
    pub listener: String,
    /// Port requested by the effective persistent configuration.
    pub original: u16,
    /// Available port selected for this process session.
    pub current: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionListenerFallback {
    pub(crate) original: u16,
    pub(crate) current: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListenerTransport {
    Tcp,
    Udp,
}

#[derive(Clone, Copy, Debug)]
struct BindClaim {
    listener: &'static str,
    address: IpAddr,
    port: u16,
    transport: ListenerTransport,
}

impl BindClaim {
    const fn new(
        listener: &'static str,
        address: IpAddr,
        port: u16,
        transport: ListenerTransport,
    ) -> Self {
        Self {
            listener,
            address,
            port,
            transport,
        }
    }

    const fn socket_addr(self) -> SocketAddr {
        SocketAddr::new(self.address, self.port)
    }
}

pub(crate) fn validate_listener_change(
    current_payload: &str,
    candidate_payload: &str,
    current_core_is_running: bool,
) -> Result<(), String> {
    let current = serde_yaml::from_str::<Value>(current_payload)
        .map_err(|error| format!("当前运行配置 YAML 无效：{error}"))?;
    let candidate = serde_yaml::from_str::<Value>(candidate_payload)
        .map_err(|error| format!("候选运行配置 YAML 无效：{error}"))?;
    let current = current
        .as_mapping()
        .ok_or_else(|| "当前运行配置顶层必须是 YAML 映射".to_owned())?;
    let candidate = candidate
        .as_mapping()
        .ok_or_else(|| "候选运行配置顶层必须是 YAML 映射".to_owned())?;
    let current_claims = if current_core_is_running {
        proxy_claims(current)?
    } else {
        Vec::new()
    };
    let mut candidate_claims = proxy_claims(candidate)?;

    if let Some(claim) = internal_conflict(&candidate_claims) {
        return Err(format!(
            "候选配置中的 {} 与另一个代理监听器重复占用 {} {}",
            claim.listener,
            claim.socket_addr(),
            transport_name(claim.transport)
        ));
    }

    candidate_claims.retain(|candidate| {
        !current_claims
            .iter()
            .any(|current| claims_overlap(current, candidate))
    });
    probe_claims(&candidate_claims)
}

pub(crate) fn resolve_conflicts(
    document: &mut Value,
    session: &mut BTreeMap<String, SessionListenerFallback>,
) -> Result<Vec<(String, SessionListenerFallback)>, String> {
    resolve_conflicts_against_claims(document, session, &[])
}

/// Applies active session fallbacks to a candidate runtime payload and resolves
/// any remaining external port conflicts or duplicate listener ports.
///
/// Claims held by the currently running core (`current_payload`) are treated as
/// available so healthy listeners are not displaced when switching profiles.
pub(crate) fn resolve_candidate_conflicts(
    current_payload: Option<&str>,
    candidate_payload: &str,
    current_core_is_running: bool,
    session: &mut BTreeMap<String, SessionListenerFallback>,
) -> Result<String, String> {
    let current_claims = if current_core_is_running {
        match current_payload {
            Some(current) => {
                let current_doc = serde_yaml::from_str::<Value>(current)
                    .map_err(|error| format!("当前运行配置 YAML 无效：{error}"))?;
                let root = current_doc
                    .as_mapping()
                    .ok_or_else(|| "当前运行配置顶层必须是 YAML 映射".to_owned())?;
                proxy_claims(root)?
            }
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };

    let mut candidate_doc = serde_yaml::from_str::<Value>(candidate_payload)
        .map_err(|error| format!("候选运行配置 YAML 无效：{error}"))?;
    let session_changed = apply_session_fallbacks_to_value(&mut candidate_doc, session)?;
    let resolved = resolve_conflicts_against_claims(&mut candidate_doc, session, &current_claims)?;
    if !session_changed && resolved.is_empty() {
        return Ok(candidate_payload.to_owned());
    }
    serde_yaml::to_string(&candidate_doc).map_err(|error| format!("无法生成端口回退配置：{error}"))
}

fn resolve_conflicts_against_claims(
    document: &mut Value,
    session: &mut BTreeMap<String, SessionListenerFallback>,
    current_claims: &[BindClaim],
) -> Result<Vec<(String, SessionListenerFallback)>, String> {
    let root = document
        .as_mapping_mut()
        .ok_or_else(|| "运行配置顶层必须是 YAML 映射".to_owned())?;
    let addresses = proxy_bind_addresses(root)?;
    let mut reserved = configured_ports(root)?;
    reserve_controller_port(root, &mut reserved);
    let mut resolved = Vec::new();
    let mut selected_claims = Vec::new();

    for spec in LISTENERS {
        if !listener_supported_on_platform(spec.key) {
            continue;
        }
        let Some(selected) = mapping_port(root, spec.key)? else {
            continue;
        };
        let claims = listener_claims(spec, &addresses, selected);
        let duplicates_another_listener = claims
            .iter()
            .any(|claim| claim_matches_any(claim, &selected_claims));
        match listener_available_against_current(&claims, current_claims) {
            Ok(true) if !duplicates_another_listener => {
                selected_claims.extend(claims);
                continue;
            }
            Ok(true) | Ok(false) => {}
            Err(error) => {
                return Err(format!(
                    "无法确认 {} 监听端口 {} 是否可用：{error}",
                    spec.key, selected
                ));
            }
        }

        let replacement = find_available_port(selected, &reserved, |candidate| {
            let claims = listener_claims(spec, &addresses, candidate);
            if claims
                .iter()
                .any(|claim| claim_matches_any(claim, &selected_claims))
            {
                return Ok(false);
            }
            listener_available_against_current(&claims, current_claims)
        })
        .map_err(|error| format!("无法确认 {} 回退端口是否可用：{error}", spec.key))?
        .ok_or_else(|| format!("{} 没有可用的回退端口", spec.key))?;
        let original = session
            .get(spec.key)
            .filter(|fallback| fallback.current == selected)
            .map_or(selected, |fallback| fallback.original);
        let fallback = SessionListenerFallback {
            original,
            current: replacement,
        };
        set_mapping_port(root, spec.key, replacement);
        reserved.insert(replacement);
        selected_claims.extend(listener_claims(spec, &addresses, replacement));
        session.insert(spec.key.to_owned(), fallback.clone());
        resolved.push((spec.key.to_owned(), fallback));
    }
    Ok(resolved)
}

pub(crate) fn apply_session_fallbacks_to_value(
    document: &mut Value,
    session: &BTreeMap<String, SessionListenerFallback>,
) -> Result<bool, String> {
    if session.is_empty() {
        return Ok(false);
    }
    let root = document
        .as_mapping_mut()
        .ok_or_else(|| "运行配置顶层必须是 YAML 映射".to_owned())?;
    let mut changed = false;
    for (key, fallback) in session {
        if mapping_port(root, key)? == Some(fallback.original) {
            set_mapping_port(root, key, fallback.current);
            changed = true;
        }
    }
    Ok(changed)
}

pub(crate) fn apply_session_fallbacks(
    payload: &str,
    session: &BTreeMap<String, SessionListenerFallback>,
) -> Result<String, String> {
    if session.is_empty() {
        return Ok(payload.to_owned());
    }
    let mut document = serde_yaml::from_str::<Value>(payload)
        .map_err(|error| format!("运行配置 YAML 无效：{error}"))?;
    if !apply_session_fallbacks_to_value(&mut document, session)? {
        return Ok(payload.to_owned());
    }
    serde_yaml::to_string(&document).map_err(|error| format!("无法生成端口回退配置：{error}"))
}

fn configured_ports(root: &Mapping) -> Result<HashSet<u16>, String> {
    LISTENERS
        .iter()
        .filter(|spec| listener_supported_on_platform(spec.key))
        .filter_map(|spec| mapping_port(root, spec.key).transpose())
        .collect()
}

fn reserve_controller_port(root: &Mapping, reserved: &mut HashSet<u16>) {
    let Some(controller) = root
        .get(Value::String("external-controller".into()))
        .and_then(Value::as_str)
    else {
        return;
    };
    if let Ok(address) = SocketAddr::from_str(controller.trim()) {
        reserved.insert(address.port());
    }
}

fn mapping_port(root: &Mapping, key: &str) -> Result<Option<u16>, String> {
    let Some(value) = root.get(Value::String(key.into())) else {
        return Ok(None);
    };
    let port = match value {
        Value::Number(value) => value.as_u64().and_then(|value| u16::try_from(value).ok()),
        Value::String(value) => value.parse().ok(),
        _ => None,
    }
    .ok_or_else(|| format!("{key} 必须是 0 到 65535 的整数"))?;
    if port == 0 {
        return Ok(None);
    }
    Ok(Some(port))
}

fn set_mapping_port(root: &mut Mapping, key: &str, port: u16) {
    root.insert(
        Value::String(key.into()),
        Value::Number(serde_yaml::Number::from(port)),
    );
}

fn proxy_bind_addresses(root: &Mapping) -> Result<Vec<IpAddr>, String> {
    let allow_lan = root
        .get(Value::String("allow-lan".into()))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !allow_lan {
        return Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
    }
    let ipv6 = root
        .get(Value::String("ipv6".into()))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let bind_address = root
        .get(Value::String("bind-address".into()))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("*");
    if bind_address == "*" {
        let mut addresses = vec![IpAddr::V4(Ipv4Addr::UNSPECIFIED)];
        if ipv6 {
            addresses.push(IpAddr::V6(Ipv6Addr::UNSPECIFIED));
        }
        return Ok(addresses);
    }
    if bind_address.eq_ignore_ascii_case("localhost") {
        return Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
    }
    let normalized = bind_address
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(bind_address);
    IpAddr::from_str(normalized)
        .map(|address| vec![address])
        .map_err(|error| format!("bind-address {bind_address:?} 无效：{error}"))
}

fn proxy_claims(root: &Mapping) -> Result<Vec<BindClaim>, String> {
    let addresses = proxy_bind_addresses(root)?;
    let mut claims = Vec::new();
    for spec in LISTENERS {
        if !listener_supported_on_platform(spec.key) {
            continue;
        }
        let Some(port) = mapping_port(root, spec.key)? else {
            continue;
        };
        claims.extend(listener_claims(spec, &addresses, port));
    }
    Ok(claims)
}

fn listener_claims(spec: ListenerSpec, addresses: &[IpAddr], port: u16) -> Vec<BindClaim> {
    addresses
        .iter()
        .copied()
        .flat_map(|address| {
            let tcp = BindClaim::new(spec.key, address, port, ListenerTransport::Tcp);
            let udp = spec.udp.then_some(BindClaim::new(
                spec.key,
                address,
                port,
                ListenerTransport::Udp,
            ));
            std::iter::once(tcp).chain(udp)
        })
        .collect()
}

fn internal_conflict(claims: &[BindClaim]) -> Option<BindClaim> {
    claims.iter().enumerate().find_map(|(index, claim)| {
        claims[..index]
            .iter()
            .any(|existing| claims_overlap(existing, claim))
            .then_some(*claim)
    })
}

fn claim_matches_any(claim: &BindClaim, existing: &[BindClaim]) -> bool {
    existing.iter().any(|item| claims_overlap(item, claim))
}

fn claims_overlap(left: &BindClaim, right: &BindClaim) -> bool {
    left.port == right.port
        && left.transport == right.transport
        && left.address.is_ipv4() == right.address.is_ipv4()
        && (left.address == right.address
            || left.address.is_unspecified()
            || right.address.is_unspecified())
}

fn listener_available_against_current(
    claims: &[BindClaim],
    current_claims: &[BindClaim],
) -> io::Result<bool> {
    if current_claims.is_empty() {
        return listener_available(claims);
    }
    let remaining: Vec<BindClaim> = claims
        .iter()
        .filter(|claim| !claim_matches_any(claim, current_claims))
        .copied()
        .collect();
    if remaining.is_empty() {
        return Ok(true);
    }
    listener_available(&remaining)
}

fn listener_available(claims: &[BindClaim]) -> io::Result<bool> {
    let mut sockets = Vec::with_capacity(claims.len());
    for claim in claims {
        match bind_claim(*claim) {
            Ok(socket) => sockets.push(socket),
            Err(error) if is_bind_conflict(&error) => return Ok(false),
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

fn probe_claims(claims: &[BindClaim]) -> Result<(), String> {
    match listener_available(claims) {
        Ok(true) => Ok(()),
        Ok(false) => {
            let conflict = claims
                .iter()
                .find(|claim| bind_claim(**claim).is_err_and(|error| is_bind_conflict(&error)));
            let Some(conflict) = conflict else {
                return Err("候选代理监听端口在检查期间变为不可用".into());
            };
            Err(format!(
                "{} 无法监听 {} {}：端口已被其他进程占用",
                conflict.listener,
                conflict.socket_addr(),
                transport_name(conflict.transport)
            ))
        }
        Err(error) => Err(format!("无法确认候选代理监听端口是否可用：{error}")),
    }
}

fn bind_claim(claim: BindClaim) -> io::Result<Socket> {
    let domain = if claim.address.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let (socket_type, protocol) = match claim.transport {
        ListenerTransport::Tcp => (Type::STREAM, Protocol::TCP),
        ListenerTransport::Udp => (Type::DGRAM, Protocol::UDP),
    };
    let socket = Socket::new(domain, socket_type, Some(protocol))?;
    if claim.address.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.bind(&SockAddr::from(claim.socket_addr()))?;
    if claim.transport == ListenerTransport::Tcp {
        socket.listen(1)?;
    }
    Ok(socket)
}

const fn transport_name(transport: ListenerTransport) -> &'static str {
    match transport {
        ListenerTransport::Tcp => "TCP",
        ListenerTransport::Udp => "UDP",
    }
}

fn is_bind_conflict(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::AddrInUse {
        return true;
    }
    #[cfg(target_os = "windows")]
    {
        const WSA_EACCES: i32 = 10_013;
        error.raw_os_error() == Some(WSA_EACCES)
    }
    #[cfg(not(target_os = "windows"))]
    false
}

fn find_available_port(
    current: u16,
    reserved: &HashSet<u16>,
    mut available: impl FnMut(u16) -> io::Result<bool>,
) -> io::Result<Option<u16>> {
    let mut candidate = next_candidate(current);
    for _ in MIN_FALLBACK_PORT..=u16::MAX {
        if candidate != current && !reserved.contains(&candidate) && available(candidate)? {
            return Ok(Some(candidate));
        }
        candidate = next_candidate(candidate);
    }
    Ok(None)
}

const fn next_candidate(port: u16) -> u16 {
    if port < MIN_FALLBACK_PORT || port == u16::MAX {
        MIN_FALLBACK_PORT
    } else {
        port + 1
    }
}

fn listener_supported_on_platform(key: &str) -> bool {
    if cfg!(target_os = "windows") && matches!(key, "redir-port" | "tproxy-port") {
        return false;
    }
    if !cfg!(target_os = "linux") && key == "tproxy-port" {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{
        SessionListenerFallback, apply_session_fallbacks, mapping_port,
        resolve_candidate_conflicts, resolve_conflicts, validate_listener_change,
    };
    use serde_yaml::Value;
    use std::{
        collections::BTreeMap,
        net::{Ipv4Addr, TcpListener, UdpSocket},
    };

    #[test]
    fn occupied_mixed_port_gets_a_session_only_replacement() {
        let occupied = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();
        let mut document = serde_yaml::from_str::<Value>(&format!(
            "mixed-port: {port}\nallow-lan: false\nrules: [MATCH,DIRECT]\n"
        ))
        .unwrap();
        let mut session = BTreeMap::new();

        let resolved = resolve_conflicts(&mut document, &mut session).unwrap();

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0, "mixed-port");
        assert_eq!(resolved[0].1.original, port);
        assert_ne!(resolved[0].1.current, port);
    }

    #[test]
    fn fallback_only_replaces_the_original_value() {
        let session = BTreeMap::from([(
            "mixed-port".to_owned(),
            SessionListenerFallback {
                original: 7890,
                current: 7891,
            },
        )]);

        let applied = apply_session_fallbacks("mixed-port: 7890\nrules: []\n", &session).unwrap();
        let explicit = apply_session_fallbacks("mixed-port: 8888\nrules: []\n", &session).unwrap();

        assert!(applied.contains("mixed-port: 7891"));
        assert!(explicit.contains("mixed-port: 8888"));
    }

    #[test]
    fn zero_port_values_are_treated_as_disabled_listeners() {
        let mut document = serde_yaml::from_str::<Value>(
            "port: 0\nsocks-port: 0\nmixed-port: 0\nredir-port: 0\ntproxy-port: 0\nrules:\n  - MATCH,DIRECT\n",
        )
        .unwrap();
        let mut session = BTreeMap::new();

        let resolved = resolve_conflicts(&mut document, &mut session).unwrap();

        assert!(resolved.is_empty());
        assert!(session.is_empty());
    }

    #[test]
    fn unchanged_port_owned_by_the_running_core_is_not_rejected() {
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();
        let payload = format!("mixed-port: {port}\nallow-lan: false\nipv6: false\n");

        validate_listener_change(&payload, &payload, true).unwrap();
    }

    #[test]
    fn changed_port_owned_by_another_process_is_rejected() {
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();
        let current = "mixed-port: 32001\nallow-lan: false\nipv6: false\n";
        let candidate = format!("mixed-port: {port}\nallow-lan: false\nipv6: false\n");

        let error = validate_listener_change(current, &candidate, true).unwrap_err();

        assert!(error.contains(&port.to_string()) && error.contains("TCP"));
    }

    #[test]
    fn adding_udp_to_an_existing_tcp_listener_checks_udp_occupancy() {
        let occupied = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();
        // The running core already owns TCP; this change introduces only UDP.
        // An unrelated TCP listener on the randomly allocated UDP port must not
        // determine which transport this test reports.
        let current = format!("port: {port}\nallow-lan: false\nipv6: false\n");
        let candidate = format!("mixed-port: {port}\nallow-lan: false\nipv6: false\n");

        let error = validate_listener_change(&current, &candidate, true).unwrap_err();

        assert!(
            error.contains(&port.to_string()) && error.contains("UDP"),
            "expected UDP conflict on {port}, got: {error}"
        );
    }

    #[test]
    fn duplicate_candidate_listener_ports_are_rejected() {
        let current = "mixed-port: 32003\nallow-lan: false\nipv6: false\n";
        let candidate = "mixed-port: 32004\nport: 32004\nallow-lan: false\nipv6: false\n";

        let error = validate_listener_change(current, candidate, true).unwrap_err();

        assert!(error.contains("重复占用") && error.contains("32004"));
    }

    #[test]
    fn startup_fallback_moves_a_duplicate_listener_port() {
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut document = serde_yaml::from_str::<Value>(&format!(
            "mixed-port: {port}\nport: {port}\nallow-lan: false\nipv6: false\n"
        ))
        .unwrap();
        let mut session = BTreeMap::new();

        let resolved = resolve_conflicts(&mut document, &mut session).unwrap();

        assert_eq!(resolved.len(), 1);
        assert!(matches!(resolved[0].0.as_str(), "mixed-port" | "port"));
        assert_ne!(resolved[0].1.current, port);
        let root = document.as_mapping().unwrap();
        assert_ne!(
            mapping_port(root, "mixed-port").unwrap(),
            mapping_port(root, "port").unwrap()
        );
    }

    #[test]
    fn candidate_resolution_moves_duplicate_port_while_preserving_running_core_port() {
        let held_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = held_listener.local_addr().unwrap().port();
        let current = format!("mixed-port: {port}\nallow-lan: false\nipv6: false\n");
        let candidate = format!("mixed-port: {port}\nport: {port}\nallow-lan: true\nipv6: false\n");
        let mut session = BTreeMap::new();

        let resolved =
            resolve_candidate_conflicts(Some(&current), &candidate, true, &mut session).unwrap();

        let doc: Value = serde_yaml::from_str(&resolved).unwrap();
        let root = doc.as_mapping().unwrap();
        assert_eq!(mapping_port(root, "mixed-port").unwrap(), Some(port));
        let fallback_port = mapping_port(root, "port").unwrap().unwrap();
        assert_ne!(fallback_port, port);
        assert_eq!(
            session.get("port"),
            Some(&SessionListenerFallback {
                original: port,
                current: fallback_port,
            })
        );
    }

    #[test]
    fn candidate_resolution_updates_prior_session_fallback_when_profile_port_collides() {
        let _running_p0 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let _running_p1 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let p0 = _running_p0.local_addr().unwrap().port();
        let p1 = _running_p1.local_addr().unwrap().port();
        let current = format!("mixed-port: {p0}\nport: {p1}\nallow-lan: false\nipv6: false\n");
        let candidate = format!("socks-port: {p1}\nport: {p0}\nallow-lan: false\nipv6: false\n");
        let mut session = BTreeMap::from([(
            "port".to_owned(),
            SessionListenerFallback {
                original: p0,
                current: p1,
            },
        )]);

        let resolved =
            resolve_candidate_conflicts(Some(&current), &candidate, true, &mut session).unwrap();
        let doc: Value = serde_yaml::from_str(&resolved).unwrap();
        let root = doc.as_mapping().unwrap();
        assert_eq!(mapping_port(root, "socks-port").unwrap(), Some(p1));
        let moved_port = mapping_port(root, "port").unwrap().unwrap();
        assert_ne!(moved_port, p1);
        assert_eq!(
            session.get("port"),
            Some(&SessionListenerFallback {
                original: p0,
                current: moved_port,
            })
        );
    }

    #[test]
    fn allow_lan_defaults_ipv6_binding_to_false_matching_mihomo() {
        let doc: Value = serde_yaml::from_str("allow-lan: true\n").unwrap();
        let addresses = super::proxy_bind_addresses(doc.as_mapping().unwrap()).unwrap();
        assert_eq!(addresses, vec![std::net::IpAddr::V4(Ipv4Addr::UNSPECIFIED)]);
    }

    #[test]
    fn find_available_port_short_circuits_immediately_on_non_conflict_error() {
        let mut attempts = 0usize;
        let result = super::find_available_port(7890, &std::collections::HashSet::new(), |_| {
            attempts += 1;
            Err(std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                "cannot assign requested address",
            ))
        });
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::AddrNotAvailable
        );
        assert_eq!(
            attempts, 1,
            "non-conflict bind error must short-circuit instead of scanning 64,511 ports"
        );
    }
}
