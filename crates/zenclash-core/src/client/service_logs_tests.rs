use super::{ServiceStreamRequest, subscribe_service_stream};
use zenclash_service::{LogStreamOptions, ServiceLogFormat, ServiceLogLevel, ServiceStream};

#[tokio::test]
async fn service_logs_dispatch_preserves_every_canonical_level_and_format() {
    for (value, level) in [
        ("silent", ServiceLogLevel::Silent),
        ("error", ServiceLogLevel::Error),
        ("warning", ServiceLogLevel::Warning),
        ("info", ServiceLogLevel::Info),
        ("debug", ServiceLogLevel::Debug),
    ] {
        for (value_format, format) in [
            ("plain", ServiceLogFormat::Plain),
            ("structured", ServiceLogFormat::Structured),
        ] {
            let query = [("level", value), ("format", value_format)];
            let request =
                subscribe_service_stream("/logs", &query, |request| async { Ok(request) })
                    .await
                    .unwrap();
            let ServiceStreamRequest::Logs(options) = request else {
                panic!("canonical log query lost its named options: {request:?}");
            };
            assert_eq!(options, LogStreamOptions { level, format });
        }
    }
}

#[tokio::test]
async fn service_logs_dispatch_defaults_only_missing_fields() {
    for (query, expected) in [
        (
            vec![],
            LogStreamOptions {
                level: ServiceLogLevel::Info,
                format: ServiceLogFormat::Plain,
            },
        ),
        (
            vec![("level", "warning")],
            LogStreamOptions {
                level: ServiceLogLevel::Warning,
                format: ServiceLogFormat::Plain,
            },
        ),
        (
            vec![("format", "structured")],
            LogStreamOptions {
                level: ServiceLogLevel::Info,
                format: ServiceLogFormat::Structured,
            },
        ),
    ] {
        let request = subscribe_service_stream("/logs", &query, |request| async { Ok(request) })
            .await
            .unwrap();
        let ServiceStreamRequest::Logs(options) = request else {
            panic!("missing query field lost its explicit defaults: {request:?}");
        };
        assert_eq!(options, expected);
    }
}

#[tokio::test]
async fn invalid_service_log_queries_never_invoke_the_subscription_operation() {
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
        let mut called = false;
        let result = subscribe_service_stream("/logs", &query, |_| {
            called = true;
            std::future::ready(Ok(()))
        })
        .await;
        assert!(result.is_err(), "accepted invalid log options");
        assert!(!called, "connected before rejecting invalid log options");
    }
}

#[tokio::test]
async fn service_log_rejection_is_returned_without_a_second_subscription() {
    let mut calls = 0;
    let result = subscribe_service_stream(
        "/logs",
        &[("level", "warning"), ("format", "structured")],
        |_| {
            calls += 1;
            std::future::ready(Err::<(), _>("service rejected operation".to_owned()))
        },
    )
    .await;
    assert_eq!(result.unwrap_err(), "service rejected operation");
    assert_eq!(calls, 1);
}

#[tokio::test]
async fn non_log_stream_queries_keep_the_existing_dispatch_policy() {
    for (path, expected) in [
        ("/traffic", ServiceStream::Traffic),
        ("/connections", ServiceStream::Connections),
        ("/memory", ServiceStream::Memory),
    ] {
        let request = subscribe_service_stream(path, &[("existing", "query")], |request| async {
            Ok(request)
        })
        .await
        .unwrap();
        let ServiceStreamRequest::Standard(actual) = request else {
            panic!("non-log stream changed policy: {request:?}");
        };
        assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
    }
}
