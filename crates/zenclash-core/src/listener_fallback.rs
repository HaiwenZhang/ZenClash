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
        let duplicates_another_listener = claims.iter().any(|claim| {
            selected_claims
                .iter()
                .any(|selected| claims_overlap(selected, claim))
        });
        match listener_available(&claims) {
            Ok(true) if !duplicates_another_listener => {
                selected_claims.extend(claims);
                continue;
            }
            Ok(true) => {}
            Ok(false) => {}
            Err(error) => {
                return Err(format!(
                    "无法确认 {} 监听端口 {} 是否可用：{error}",
                    spec.key, selected
                ));
            }
        }

        let replacement = find_available_port(selected, &reserved, |candidate| {
            let claims = listener_claims(spec, &addresses, candidate);
            !claims.iter().any(|claim| {
                selected_claims
                    .iter()
                    .any(|selected| claims_overlap(selected, claim))
            }) && listener_available(&claims).unwrap_or(false)
        })
        .ok_or_else(|| format!("{} 没有可用的回退端口", spec.key))?;
        let original = session
            .get(spec.key)
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

pub(crate) fn apply_session_fallbacks(
    payload: &str,
    session: &BTreeMap<String, SessionListenerFallback>,
) -> Result<String, String> {
    if session.is_empty() {
        return Ok(payload.to_owned());
    }
    let mut document = serde_yaml::from_str::<Value>(payload)
        .map_err(|error| format!("运行配置 YAML 无效：{error}"))?;
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
    if !changed {
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
        .unwrap_or(true);
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

fn claims_overlap(left: &BindClaim, right: &BindClaim) -> bool {
    left.port == right.port
        && left.transport == right.transport
        && left.address.is_ipv4() == right.address.is_ipv4()
        && (left.address == right.address
            || left.address.is_unspecified()
            || right.address.is_unspecified())
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
    mut available: impl FnMut(u16) -> bool,
) -> Option<u16> {
    let mut candidate = next_candidate(current);
    for _ in MIN_FALLBACK_PORT..=u16::MAX {
        if candidate != current && !reserved.contains(&candidate) && available(candidate) {
            return Some(candidate);
        }
        candidate = next_candidate(candidate);
    }
    None
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
        SessionListenerFallback, apply_session_fallbacks, mapping_port, resolve_conflicts,
        validate_listener_change,
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
    fn changed_mixed_port_checks_udp_occupancy() {
        let occupied = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();
        let current = "mixed-port: 32002\nallow-lan: false\nipv6: false\n";
        let candidate = format!("mixed-port: {port}\nallow-lan: false\nipv6: false\n");

        let error = validate_listener_change(current, &candidate, true).unwrap_err();

        assert!(error.contains(&port.to_string()) && error.contains("UDP"));
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
}
