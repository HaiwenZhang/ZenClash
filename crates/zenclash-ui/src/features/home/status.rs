use super::*;

pub(super) fn system_proxy_status_text(capture: &CaptureStatus) -> String {
    match &capture.system_proxy {
        Observation::Loading => zenclash_i18n::text("home.controls.system_proxy_loading"),
        Observation::Failed { .. } => zenclash_i18n::text("home.controls.system_proxy_unavailable"),
        Observation::Fresh { value, .. } => describe_system_proxy(value),
        Observation::Stale {
            value,
            observed_at_ms,
            ..
        } => zenclash_i18n::text_with(
            "home.controls.system_proxy_stale",
            &[
                ("status", describe_system_proxy(value)),
                ("age", format_profile_age(observed_at_ms / 1_000)),
            ],
        ),
    }
}

pub(super) fn describe_system_proxy(
    snapshot: &zenclash_core::SystemProxySessionSnapshot,
) -> String {
    if snapshot.ownership == SystemProxyOwnershipState::Lost {
        return zenclash_i18n::text("home.controls.system_proxy_ownership_lost");
    }
    if snapshot.intent_enabled
        && snapshot.actual.active()
        && snapshot.ownership == SystemProxyOwnershipState::Unowned
    {
        return zenclash_i18n::text("home.controls.system_proxy_ownership_unknown");
    }
    match (snapshot.intent_enabled, snapshot.actual.active()) {
        (true, true) => zenclash_i18n::text("home.controls.system_proxy_on"),
        (true, false) => zenclash_i18n::text("home.controls.system_proxy_intent_only"),
        (false, true) => zenclash_i18n::text("home.controls.system_proxy_external"),
        (false, false) => zenclash_i18n::text("home.controls.system_proxy_off"),
    }
}

pub(super) fn capture_status_text(capture: &CaptureStatus) -> String {
    if capture.is_active() {
        return zenclash_i18n::text("home.controls.capture_active");
    }
    if capture
        .tun
        .value()
        .is_some_and(|tun| tun.configured && matches!(tun.observed, CapabilityState::Unknown))
    {
        return zenclash_i18n::text("home.controls.capture_unverified");
    }
    if matches!(
        &capture.system_proxy,
        Observation::Failed { .. } | Observation::Loading
    ) || matches!(
        &capture.tun,
        Observation::Failed { .. } | Observation::Loading
    ) {
        return zenclash_i18n::text("home.controls.capture_unknown");
    }
    zenclash_i18n::text("home.controls.capture_off")
}

pub(super) fn process_evidence(
    process: Option<&ProcessStatus>,
    theme: &gpui_kit::component::Theme,
) -> (String, gpui_kit::Hsla) {
    let (key, attempts) = process_evidence_copy(process);
    let text = attempts.map_or_else(
        || zenclash_i18n::text(key),
        |attempt| zenclash_i18n::text_with(key, &[("attempt", attempt.to_string())]),
    );
    let color = match process.map(|process| process.recovery) {
        Some(ProcessRecoveryStatus::Stable | ProcessRecoveryStatus::External)
            if process.is_some_and(|process| process.running) =>
        {
            crate::foundation::design::online_indicator(theme)
        }
        Some(
            ProcessRecoveryStatus::Recovering
            | ProcessRecoveryStatus::NetworkSuspended
            | ProcessRecoveryStatus::Unknown,
        ) => theme.warning,
        Some(
            ProcessRecoveryStatus::Stable
            | ProcessRecoveryStatus::Failed
            | ProcessRecoveryStatus::Stopped
            | ProcessRecoveryStatus::External,
        )
        | None => theme.danger,
    };
    (text, color)
}

pub(super) fn process_evidence_copy(
    process: Option<&ProcessStatus>,
) -> (&'static str, Option<u32>) {
    let Some(process) = process else {
        return ("home.evidence.core_unavailable", None);
    };
    match process.recovery {
        ProcessRecoveryStatus::Recovering => (
            "home.evidence.core_recovering",
            Some(process.recovery_attempts),
        ),
        ProcessRecoveryStatus::Failed => (
            "home.evidence.core_recovery_failed",
            Some(process.recovery_attempts),
        ),
        ProcessRecoveryStatus::Stopped => ("home.evidence.core_stopped", None),
        ProcessRecoveryStatus::Unknown => ("core_page.status.unreadable", None),
        ProcessRecoveryStatus::NetworkSuspended => ("automatic.network_stopped", None),
        ProcessRecoveryStatus::Stable if process.running && process.recovery_attempts > 0 => (
            "home.evidence.core_recovered",
            Some(process.recovery_attempts),
        ),
        ProcessRecoveryStatus::Stable | ProcessRecoveryStatus::External if process.running => {
            ("home.evidence.core_running", None)
        }
        ProcessRecoveryStatus::Stable | ProcessRecoveryStatus::External => {
            ("home.evidence.core_unavailable", None)
        }
    }
}

pub(super) fn stream_status_text(
    observation: &Observation<StreamStatus>,
    theme: &gpui_kit::component::Theme,
) -> (String, gpui_kit::Hsla) {
    match observation {
        Observation::Fresh { .. } => (zenclash_i18n::text("home.traffic.live"), theme.success),
        Observation::Stale { observed_at_ms, .. } => (
            zenclash_i18n::text_with(
                "home.traffic.stale",
                &[("age", format_profile_age(observed_at_ms / 1_000))],
            ),
            theme.warning,
        ),
        Observation::Failed { .. } => (
            zenclash_i18n::text("home.traffic.unavailable"),
            theme.danger,
        ),
        Observation::Loading => (
            zenclash_i18n::text("home.traffic.reconnecting"),
            theme.warning,
        ),
    }
}
