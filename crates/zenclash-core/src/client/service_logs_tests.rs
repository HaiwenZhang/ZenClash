use super::service_stream_path;

#[test]
fn native_logs_preserve_canonical_levels_formats_and_defaults() {
    for level in ["silent", "error", "warning", "info", "debug"] {
        for format in ["plain", "structured"] {
            assert_eq!(
                service_stream_path("/logs", &[("level", level), ("format", format)]).unwrap(),
                format!("/logs?level={level}&format={format}")
            );
        }
    }
    assert_eq!(
        service_stream_path("/logs", &[]).unwrap(),
        "/logs?level=info&format=plain"
    );
    assert_eq!(
        service_stream_path("/logs", &[("level", "warning")]).unwrap(),
        "/logs?level=warning&format=plain"
    );
    assert_eq!(
        service_stream_path("/logs", &[("format", "structured")]).unwrap(),
        "/logs?level=info&format=structured"
    );
}

#[test]
fn invalid_log_queries_are_rejected_before_opening_the_native_socket() {
    for query in [
        vec![("unknown", "debug")],
        vec![("level", "debug"), ("level", "debug")],
        vec![("format", "plain"), ("format", "structured")],
        vec![("level", "warn")],
        vec![("level", "DEBUG")],
        vec![("level", " debug")],
        vec![("level", "")],
        vec![("format", "json")],
        vec![("format", "STRUCTURED")],
        vec![("format", "")],
    ] {
        assert!(service_stream_path("/logs", &query).is_err());
    }
    assert!(service_stream_path("/unsupported", &[]).is_err());
}

#[test]
fn native_non_log_stream_queries_are_encoded_without_losing_values() {
    for path in ["/traffic", "/connections", "/memory"] {
        assert_eq!(
            service_stream_path(path, &[("existing", "space / &")]).unwrap(),
            format!("{path}?existing=space+%2F+%26")
        );
    }
}
