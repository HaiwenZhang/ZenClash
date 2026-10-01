use std::collections::BTreeMap;

use serde_json::Value;

use crate::protocol::{ApiRequest, ServiceErrorCode};

const MAX_API_PATH_BYTES: usize = 16 * 1024;
const MAX_API_BODY_BYTES: usize = 64 * 1024;

pub(crate) fn validate_request(request: &ApiRequest) -> Result<(), ServiceErrorCode> {
    if request.path.len() > MAX_API_PATH_BYTES
        || !request.path.starts_with('/')
        || request.path.starts_with("//")
        || request.path.contains(['\\', '#'])
        || request.path.chars().any(char::is_control)
    {
        return Err(ServiceErrorCode::InvalidRequest);
    }
    let uri = url::Url::parse(&format!("http://localhost{}", request.path))
        .map_err(|_| ServiceErrorCode::InvalidRequest)?;
    let raw_path = request
        .path
        .split('?')
        .next()
        .ok_or(ServiceErrorCode::InvalidRequest)?;
    // URL parsing must not normalize a traversal into a different allowed API.
    if uri.host_str() != Some("localhost") || uri.path() != raw_path {
        return Err(ServiceErrorCode::InvalidRequest);
    }
    let query = collect_query(&uri)?;
    let parts: Vec<_> = raw_path
        .strip_prefix('/')
        .ok_or(ServiceErrorCode::InvalidRequest)?
        .split('/')
        .collect();
    if parts.iter().any(|part| !valid_segment(part)) {
        return Err(ServiceErrorCode::InvalidRequest);
    }
    if let Some(body) = &request.body
        && serde_json::to_vec(body)
            .map_err(|_| ServiceErrorCode::InvalidRequest)?
            .len()
            > MAX_API_BODY_BYTES
    {
        return Err(ServiceErrorCode::BudgetExceeded);
    }
    let allowed = match (request.method.as_str(), parts.as_slice()) {
        ("GET", ["version" | "configs" | "proxies" | "rules" | "connections"])
        | ("GET", ["proxies", _])
        | ("GET", ["providers", "proxies" | "rules"])
        | ("GET", ["providers", "proxies" | "rules", _]) => query.is_empty() && no_body(request),
        ("GET", ["proxies" | "group", _, "delay"])
        | ("GET", ["providers", "proxies", _, _, "healthcheck"]) => {
            no_body(request) && valid_delay_query(&query)
        }
        ("GET", ["providers", "proxies", _, "healthcheck"]) => no_body(request) && query.is_empty(),
        ("GET", ["dns", "query"]) => no_body(request) && valid_dns_query(&query),
        ("PUT", ["proxies", _]) => {
            query.is_empty() && request.body.as_ref().is_some_and(valid_selection)
        }
        ("PUT", ["providers", "proxies" | "rules", _])
        | ("DELETE", ["proxies", _])
        | ("DELETE", ["connections"])
        | ("DELETE", ["connections", _])
        | ("POST", ["cache", "dns" | "fakeip", "flush"])
        | ("POST", ["configs", "geo"]) => query.is_empty() && no_body(request),
        ("PATCH", ["rules", "disable"]) => {
            query.is_empty() && request.body.as_ref().is_some_and(valid_rule_patch)
        }
        // Full reloads must consume a service-generated staged runtime. A caller
        // can never ask the root kernel to load its own path or raw payload.
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(ServiceErrorCode::InvalidRequest)
    }
}

fn no_body(request: &ApiRequest) -> bool {
    request.body.is_none()
}

fn collect_query(uri: &url::Url) -> Result<BTreeMap<String, String>, ServiceErrorCode> {
    let mut query = BTreeMap::new();
    for (key, value) in uri.query_pairs() {
        if key.chars().any(char::is_control)
            || value.chars().any(char::is_control)
            || query.insert(key.into_owned(), value.into_owned()).is_some()
        {
            return Err(ServiceErrorCode::InvalidRequest);
        }
    }
    Ok(query)
}

fn valid_segment(segment: &str) -> bool {
    if segment.is_empty() || segment.len() > 4096 {
        return false;
    }
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let Some(high) = bytes
                .get(index + 1)
                .and_then(|byte| (*byte as char).to_digit(16))
            else {
                return false;
            };
            let Some(low) = bytes
                .get(index + 2)
                .and_then(|byte| (*byte as char).to_digit(16))
            else {
                return false;
            };
            decoded.push((high * 16 + low) as u8);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    let Ok(decoded) = std::str::from_utf8(&decoded) else {
        return false;
    };
    !decoded.contains('\\')
        && !decoded.chars().any(char::is_control)
        && decoded.split('/').all(|part| part != "." && part != "..")
}

fn valid_delay_query(query: &BTreeMap<String, String>) -> bool {
    if query.len() != 2 {
        return false;
    }
    let Some(timeout) = query
        .get("timeout")
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return false;
    };
    let Some(url) = query
        .get("url")
        .and_then(|value| url::Url::parse(value).ok())
    else {
        return false;
    };
    (1..=60_000).contains(&timeout)
        && matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

fn valid_dns_query(query: &BTreeMap<String, String>) -> bool {
    query.len() == 2
        && query
            .get("name")
            .is_some_and(|value| !value.is_empty() && value.len() <= 253)
        && query
            .get("type")
            .is_some_and(|value| matches!(value.as_str(), "A" | "AAAA"))
}

fn valid_selection(body: &Value) -> bool {
    body.as_object().is_some_and(|object| {
        object.len() == 1
            && object
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| {
                    !name.is_empty() && name.len() <= 4096 && !name.chars().any(char::is_control)
                })
    })
}

fn valid_rule_patch(body: &Value) -> bool {
    body.as_object().is_some_and(|object| {
        !object.is_empty()
            && object.len() <= 1024
            && object
                .iter()
                .all(|(index, value)| index.parse::<u32>().is_ok() && value.is_boolean())
    })
}

fn valid_runtime_patch(body: &Value) -> bool {
    body.as_object().is_some_and(|object| {
        !object.is_empty()
            && object.iter().all(|(key, value)| match key.as_str() {
                "mode" => matches!(value.as_str(), Some("rule" | "global" | "direct")),
                "log-level" => matches!(
                    value.as_str(),
                    Some("silent" | "error" | "warning" | "info" | "debug")
                ),
                "port" | "socks-port" | "mixed-port" | "redir-port" | "tproxy-port" => value
                    .as_u64()
                    .is_some_and(|port| port <= u64::from(u16::MAX)),
                "ipv6" | "allow-lan" | "tcp-concurrent" | "unified-delay" | "inbound-tfo"
                | "inbound-mptcp" | "disable-keep-alive" => value.is_boolean(),
                "bind-address" | "interface-name" => bounded_text(value, 256),
                "find-process-mode" => matches!(value.as_str(), Some("always" | "strict" | "off")),
                "keep-alive-interval" | "keep-alive-idle" => {
                    value.as_u64().is_some_and(|value| value <= 86_400)
                }
                "routing-mark" => value
                    .as_u64()
                    .is_some_and(|value| value <= u64::from(u32::MAX)),
                "tun" => valid_tun_patch(value),
                _ => false,
            })
    })
}

pub(crate) fn canonical_runtime_patch(body: &Value) -> Result<Value, ServiceErrorCode> {
    if serde_json::to_vec(body).map_err(|_| ServiceErrorCode::InvalidRequest)?.len() > MAX_API_BODY_BYTES {
        return Err(ServiceErrorCode::BudgetExceeded);
    }
    if !valid_runtime_patch(body) { return Err(ServiceErrorCode::InvalidConfiguration); }
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(object) => Value::Object(object.iter().map(|(key, value)| (key.clone(), sorted(value))).collect::<BTreeMap<_, _>>().into_iter().collect()),
            Value::Array(values) => Value::Array(values.iter().map(sorted).collect()),
            _ => value.clone(),
        }
    }
    Ok(sorted(body))
}

pub(crate) fn affected_values(actual: &Value, patch: &Value) -> Result<Value, ServiceErrorCode> {
    let object = patch.as_object().ok_or(ServiceErrorCode::InvalidConfiguration)?;
    let actual = actual.as_object().ok_or(ServiceErrorCode::KernelFailed)?;
    let mut selected = serde_json::Map::new();
    for (key, requested) in object {
        let value = actual.get(key).ok_or(ServiceErrorCode::KernelFailed)?;
        selected.insert(key.clone(), if requested.is_object() { affected_values(value, requested)? } else { value.clone() });
    }
    Ok(Value::Object(selected))
}

fn bounded_text(value: &Value, limit: usize) -> bool {
    value
        .as_str()
        .is_some_and(|text| text.len() <= limit && !text.chars().any(char::is_control))
}

fn valid_tun_patch(body: &Value) -> bool {
    body.as_object().is_some_and(|object| {
        !object.is_empty()
            && object.iter().all(|(key, value)| match key.as_str() {
                "enable"
                | "auto-route"
                | "auto-detect-interface"
                | "strict-route"
                | "auto-redirect"
                | "gso" => value.is_boolean(),
                "stack" => matches!(value.as_str(), Some("gvisor" | "system" | "mixed")),
                "device" => bounded_text(value, 256),
                "mtu" => value
                    .as_u64()
                    .is_some_and(|mtu| (576..=65_535).contains(&mtu)),
                "dns-hijack" => value.as_array().is_some_and(|values| {
                    values.len() <= 64 && values.iter().all(|value| bounded_text(value, 256))
                }),
                _ => false,
            })
    })
}

pub(crate) fn redact_response(_path: &str, mut body: Value) -> Value {
    redact_private_fields(&mut body);
    body
}

fn redact_private_fields(body: &mut Value) {
    match body {
        Value::Object(object) => {
            object.retain(|key, _| {
                !matches!(
                    key.as_str(),
                    "secret"
                        | "external-controller"
                        | "external-controller-unix"
                        | "external-controller-pipe"
                        | "external-controller-tls"
                        | "external-ui"
                        | "external-ui-url"
                        | "external-ui-name"
                        | "private-key"
                        | "private_key"
                )
            });
            for value in object.values_mut() {
                redact_private_fields(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_private_fields(value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn raw_runtime_patch_cannot_bypass_managed_revision_transactions() {
        assert_eq!(
            super::validate_request(&super::ApiRequest {
                method: "PATCH".into(),
                path: "/configs".into(),
                body: Some(serde_json::json!({"mode":"global"})),
            }),
            Err(super::ServiceErrorCode::InvalidRequest)
        );
    }

    use super::*;

    fn request(method: &str, path: &str, body: Option<Value>) -> ApiRequest {
        ApiRequest {
            method: method.into(),
            path: path.into(),
            body,
        }
    }

    #[test]
    fn business_reads_and_selector_updates_keep_encoded_names_in_their_route() {
        for path in [
            "/version",
            "/configs",
            "/proxies",
            "/proxies/%E9%A6%99%E6%B8%AF%2F01",
            "/providers/proxies/local",
            "/rules",
            "/connections",
        ] {
            assert!(
                validate_request(&request("GET", path, None)).is_ok(),
                "rejected {path}"
            );
        }
        assert!(
            validate_request(&request(
                "PUT",
                "/proxies/%E9%A6%99%E6%B8%AF",
                Some(serde_json::json!({"name":"香港/01"}))
            ))
            .is_ok()
        );
        assert!(
            validate_request(&request(
                "PATCH",
                "/configs",
                Some(serde_json::json!({"mode":"rule","tun":{"enable":false}}))
            ))
            .is_ok()
        );
    }

    #[test]
    fn privileged_execution_and_config_path_operations_are_not_proxyable() {
        for (method, path, body) in [
            (
                "PUT",
                "/configs",
                Some(serde_json::json!({"path":"/etc/shadow"})),
            ),
            (
                "PUT",
                "/configs",
                Some(serde_json::json!({"payload":"external-ui: /etc"})),
            ),
            ("POST", "/restart", None),
            ("POST", "/upgrade", None),
            ("POST", "/upgrade/ui", None),
            ("GET", "/storage", None),
            (
                "PATCH",
                "/configs",
                Some(serde_json::json!({"secret":"attacker"})),
            ),
            (
                "PATCH",
                "/configs",
                Some(serde_json::json!({"external-controller":"0.0.0.0:9000"})),
            ),
            (
                "PATCH",
                "/configs",
                Some(serde_json::json!({"tun":{"path":"/etc/shadow"}})),
            ),
        ] {
            assert!(
                validate_request(&request(method, path, body)).is_err(),
                "accepted {method} {path}"
            );
        }
    }

    #[test]
    fn query_parameter_smuggling_and_path_normalization_are_rejected() {
        for path in [
            "//evil/configs",
            "/proxies/../configs",
            "/proxies/%2e%2e/configs",
            "/proxies/a%2F..%2Fconfigs",
            "/proxies/a%00",
            "/configs?path=/etc/shadow",
            "/version#ignored",
            "/proxies/a/delay?url=https://example.com&timeout=1000&timeout=2000",
            "/proxies/a/delay?url=file:///etc/shadow&timeout=1000",
        ] {
            assert!(
                validate_request(&request("GET", path, None)).is_err(),
                "accepted {path}"
            );
        }
        assert!(
            validate_request(&request(
                "GET",
                "/proxies/a/delay?url=https%3A%2F%2Fexample.com&timeout=1000",
                None
            ))
            .is_ok()
        );
        assert!(
            validate_request(&request(
                "GET",
                "/dns/query?name=example.com&type=AAAA",
                None
            ))
            .is_ok()
        );
    }

    #[test]
    fn runtime_patch_schema_rejects_types_and_fields_the_kernel_might_ignore() {
        for body in [
            serde_json::json!({"mode":"script"}),
            serde_json::json!({"mixed-port":65536}),
            serde_json::json!({"tun":{"enable":"true"}}),
            serde_json::json!({"tun":{"certificate":"/etc/shadow"}}),
            serde_json::json!({"future-unknown-key":true}),
        ] {
            assert!(validate_request(&request("PATCH", "/configs", Some(body))).is_err());
        }
    }

    #[test]
    fn private_controller_credentials_and_paths_are_removed_from_response_trees() {
        let body = serde_json::json!({"mode":"rule", "secret":"private", "external-controller-pipe":"private-pipe", "tls":{"private-key":"private-key", "enabled":true}, "nested":[{"external-controller-unix":"private-socket"}]});
        assert_eq!(
            redact_response("/configs", body),
            serde_json::json!({"mode":"rule", "tls":{"enabled":true}, "nested":[{}]})
        );
    }
}
