use super::*;

#[test]
fn reputation_requires_both_sources_before_passing() {
    let ip = "203.0.113.1";
    let risk = json!({"status":"ok", ip:{"risk":12}});
    let spam = json!({"success":1,"ip":{"appears":0}});
    assert_eq!(
        risk_result(ip, Some(&risk), Some(&spam)).status(),
        Status::Passed
    );
    assert_eq!(risk_result(ip, Some(&risk), None).status(), Status::Unknown);
    assert_eq!(risk_result(ip, None, Some(&spam)).status(), Status::Unknown);
    assert_eq!(
        risk_result(ip, Some(&json!({"status":"error"})), Some(&spam)).status(),
        Status::Unknown
    );
}

#[test]
fn reputation_thresholds_and_positive_abuse_survive_partial_failure() {
    let ip = "203.0.113.1";
    for (score, expected) in [
        (29, Status::Passed),
        (30, Status::Attention),
        (69, Status::Attention),
        (70, Status::Failed),
        (100, Status::Failed),
        (101, Status::Unknown),
    ] {
        assert_eq!(
            risk_result(
                ip,
                Some(&json!({"status":"ok", ip:{"risk":score}})),
                Some(&json!({"success":true,"ip":{"appears":false}}))
            )
            .status(),
            expected
        );
    }
    assert_eq!(
        risk_result(
            ip,
            None,
            Some(&json!({"success":true,"ip":{"appears":true}}))
        )
        .status(),
        Status::Attention
    );
}

#[test]
fn endpoint_classification_rejects_lookalikes_and_redacts_secrets() {
    let result = local::endpoint_result(
        Some("https://user:secret@api.anthropic.com/private-key?token=secret#secret"),
        true,
    );
    assert_eq!(result.status(), Status::Info);
    assert!(!format!("{result:?}").contains("secret"));
    for url in [
        "https://api.anthropic.com.evil.test",
        "http://api.anthropic.com",
        "https://api.anthropic.com:8443",
    ] {
        assert_eq!(
            local::endpoint_result(Some(url), true).status(),
            Status::Attention
        );
    }
    assert_eq!(
        local::endpoint_result(None, false).status(),
        Status::Unknown
    );
    assert_eq!(
        local::endpoint_result(Some("not a url"), true).status(),
        Status::Unknown
    );
}

#[test]
fn dns_observations_do_not_prove_absence_of_leaks() {
    assert_eq!(local::dns_result(&[], false).status(), Status::Unknown);
    assert_eq!(
        local::dns_result(&["192.168.1.1".into()], false).status(),
        Status::Info
    );
    assert_eq!(
        local::dns_result(&["223.5.5.5".into()], false).status(),
        Status::Attention
    );
    assert_eq!(
        local::dns_result(&["8.8.8.8".into()], true).status(),
        Status::Unknown
    );
}

#[test]
fn timezone_checks_system_and_cli_independently() {
    assert_eq!(
        local::timezone_result(
            Some("Asia/Shanghai"),
            Some("America/Los_Angeles"),
            Some("America/Los_Angeles")
        )
        .status(),
        Status::Attention
    );
    assert_eq!(
        local::timezone_result(
            None,
            Some("America/Los_Angeles"),
            Some("America/Los_Angeles")
        )
        .status(),
        Status::Unknown
    );
    assert_eq!(
        local::timezone_result(
            Some("Asia/Shanghai"),
            Some("Asia/Shanghai"),
            Some("Asia/Shanghai")
        )
        .status(),
        Status::Passed
    );
}
