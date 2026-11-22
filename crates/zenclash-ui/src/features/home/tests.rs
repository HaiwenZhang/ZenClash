use zenclash_core::{
    DelayHistory, ProxyGroup, ProxyGroupBehavior, ProxyNode, SystemProxySessionSnapshot,
    SystemProxyStatus, TunCaptureStatus, TunRuntimeObservation,
};

use super::controls::home_service_feedback;
use super::status::{describe_system_proxy, process_evidence_copy};
use super::transitions::{
    CapturePresentation, CaptureTransition, ModeTransition, capture_presentation, mode_presentation,
};
use super::*;
use crate::components::page_primitives::describe_tun;

fn traffic_chart_points(samples: &VecDeque<TrafficSample>) -> Vec<TrafficChartPoint> {
    let last_ix = samples.len().saturating_sub(1);
    let ceiling = traffic_chart_ceiling(samples);
    samples
        .iter()
        .enumerate()
        .map(|(ix, sample)| {
            let remaining_seconds = last_ix.saturating_sub(ix);
            let label = if remaining_seconds == 0 {
                zenclash_i18n::text("home.traffic.now").into()
            } else {
                format!("−{remaining_seconds}s").into()
            };
            TrafficChartPoint {
                label,
                upload: chart_value(sample.upload),
                download: chart_value(sample.download),
                ceiling,
            }
        })
        .collect()
}

#[test]
fn core_memory_uses_mb_minimum_and_requires_current_fresh_data() {
    assert_eq!(format_core_memory(1024), "0.0 MB");
    assert_eq!(format_core_memory(1_500_000), "1.5 MB");
    assert_eq!(format_core_memory(1_500_000_000), "1.5 GB");
    let mut streams = StreamStatuses::default();
    assert_eq!(core_memory_bytes(&streams, 2), None);
    streams.memory = Observation::Fresh {
        value: StreamStatus {
            generation: 2,
            memory: 47_000_000,
            ..Default::default()
        },
        observed_at_ms: 1,
    };
    assert_eq!(core_memory_bytes(&streams, 2), Some(47_000_000));
    assert_eq!(core_memory_bytes(&streams, 3), None);
    streams.memory = Observation::Stale {
        value: *streams.memory.value().unwrap(),
        observed_at_ms: 1,
        failure: zenclash_core::OperationalFailure {
            message: "read failed".into(),
            occurred_at_ms: 2,
        },
    };
    assert_eq!(core_memory_bytes(&streams, 2), None);
    streams.memory = Observation::Loading;
    assert_eq!(core_memory_bytes(&streams, 2), None);
    streams.memory = Observation::Fresh {
        value: StreamStatus {
            generation: 2,
            memory: 0,
            ..Default::default()
        },
        observed_at_ms: 1,
    };
    assert_eq!(core_memory_bytes(&streams, 2), None);
}

struct ServiceFeedbackView {
    phase: Option<zenclash_core::ServicePhase>,
    warning: Option<String>,
}

impl gpui_kit::Render for ServiceFeedbackView {
    fn render(&mut self, _: &mut gpui_kit::Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::ActiveTheme;
        home_service_feedback(self.phase, false, self.warning.clone(), cx.theme())
            .unwrap_or_else(div)
    }
}

#[gpui_kit::test]
fn home_service_feedback_native_uncertainty_and_independent_warning_remain_visible(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::AppContext;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    cx.update(gpui_kit::init);
    let mut view = None;
    let window = cx.open_window(gpui_kit::size(px(900.), px(500.)), |window, cx| {
        let entity = cx.new(|_| ServiceFeedbackView {
            phase: Some(zenclash_core::ServicePhase::Unconfirmed),
            warning: None,
        });
        view = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    let view = view.unwrap();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("home-service-pending").label(),
            Some(zenclash_i18n::text("core_page.service.pending").as_str())
        );
        let warning = zenclash_i18n::text("core_page.service.failed");
        view.update(cx, |view, cx| {
            view.phase = Some(zenclash_core::ServicePhase::Completed);
            view.warning = Some(warning.clone());
            cx.notify();
        });
        window.render_frame(cx);
        assert!(window.try_find("home-service-pending").is_none());
        assert_eq!(
            window.find("home-service-warning").label(),
            Some(warning.as_str())
        );
        window.render_frame(cx);
        assert_eq!(
            window.find("home-service-warning").label(),
            Some(warning.as_str())
        );
        window.remove_window();
    })
    .unwrap();
}

#[test]
fn process_recovery_attempts_have_a_visible_non_color_label() {
    let process = ProcessStatus {
        kind: zenclash_core::CoreKind::Mihomo,
        managed: true,
        pid: None,
        started_at_secs: None,
        running: false,
        generation: 3,
        exit_reason: Some("exit status: 23".into()),
        recovery_attempts: 2,
        recovery: ProcessRecoveryStatus::Recovering,
    };

    assert_eq!(
        process_evidence_copy(Some(&process)),
        ("home.evidence.core_recovering", Some(2))
    );
    assert_eq!(
        process_evidence_copy(Some(&ProcessStatus {
            recovery: ProcessRecoveryStatus::Failed,
            ..process.clone()
        })),
        ("home.evidence.core_recovery_failed", Some(2))
    );
    assert_eq!(
        process_evidence_copy(Some(&ProcessStatus {
            running: true,
            recovery: ProcessRecoveryStatus::Stable,
            ..process
        })),
        ("home.evidence.core_recovered", Some(2))
    );
}

#[test]
fn direct_mode_summary_does_not_require_a_proxy_group() {
    let summary = current_proxy_summary(
        &RuntimeConfig {
            mode: "direct".into(),
            ..RuntimeConfig::default()
        },
        &ProxyCatalog::default(),
    );

    assert_eq!(summary.node, "DIRECT");
}

#[test]
fn rule_mode_summary_uses_the_primary_group_current_node() {
    let catalog = ProxyCatalog::from_group_nodes(
        vec![(
            ProxyGroup::new("Proxy").with_current_member("HK 01"),
            vec![ProxyNode {
                name: "HK 01".into(),
                kind: "Hysteria2".into(),
                history: vec![DelayHistory {
                    delay: 42,
                    ..DelayHistory::default()
                }],
                ..ProxyNode::default()
            }],
        )],
        1,
    );

    let summary = current_proxy_summary(
        &RuntimeConfig {
            mode: "rule".into(),
            ..RuntimeConfig::default()
        },
        &catalog,
    );
    assert_eq!(summary.group, "Proxy");
    assert_eq!(summary.node, "HK 01");
}

#[test]
fn load_balance_summary_does_not_offer_a_fake_manual_selection() {
    let group = ProxyGroup::new("Balance")
        .with_behavior(ProxyGroupBehavior::LoadBalance)
        .with_current_member("HK 01");
    let catalog = ProxyCatalog::from_group_nodes(
        vec![(
            group.clone(),
            vec![ProxyNode {
                name: "HK 01".into(),
                ..Default::default()
            }],
        )],
        2,
    );

    let summary = current_proxy_summary(
        &RuntimeConfig {
            mode: "rule".into(),
            ..RuntimeConfig::default()
        },
        &catalog,
    );

    assert!(!home_group_can_switch(&group));
    assert_eq!(summary.node, zenclash_i18n::text("home.proxy.load_balance"));
}

#[test]
fn system_proxy_intent_actual_and_lost_ownership_have_distinct_copy() {
    let intent_only = SystemProxySessionSnapshot {
        intent_enabled: true,
        actual: SystemProxyStatus::default(),
        ownership: SystemProxyOwnershipState::Unowned,
    };
    let external = SystemProxySessionSnapshot {
        intent_enabled: false,
        actual: SystemProxyStatus {
            enabled: true,
            ..SystemProxyStatus::default()
        },
        ownership: SystemProxyOwnershipState::Unowned,
    };
    let lost = SystemProxySessionSnapshot {
        intent_enabled: true,
        actual: external.actual.clone(),
        ownership: SystemProxyOwnershipState::Lost,
    };

    assert_eq!(
        describe_system_proxy(&intent_only),
        zenclash_i18n::text("home.controls.system_proxy_intent_only")
    );
    assert_eq!(
        describe_system_proxy(&external),
        zenclash_i18n::text("home.controls.system_proxy_external")
    );
    assert_eq!(
        describe_system_proxy(&lost),
        zenclash_i18n::text("home.controls.system_proxy_ownership_lost")
    );
}

#[test]
fn tun_status_copy_distinguishes_off_unverified_active_and_unsupported() {
    let mut status = TunCaptureStatus {
        requested: false,
        configured: false,
        permission: CapabilityState::Inactive,
        runtime: TunRuntimeObservation {
            device_name: None,
            device: CapabilityState::Inactive,
            route: CapabilityState::Inactive,
            detail: String::new(),
        },
        observed: CapabilityState::Inactive,
    };

    assert_eq!(
        describe_tun(&status),
        zenclash_i18n::text("home.controls.tun_off")
    );

    status.requested = true;
    status.configured = true;
    status.observed = CapabilityState::Unknown;
    assert_eq!(
        describe_tun(&status),
        zenclash_i18n::text("home.controls.tun_unverified")
    );

    status.observed = CapabilityState::Active;
    assert_eq!(
        describe_tun(&status),
        zenclash_i18n::text("home.controls.tun_on")
    );

    status.observed = CapabilityState::Unsupported;
    assert_eq!(
        describe_tun(&status),
        zenclash_i18n::text("home.controls.tun_unsupported")
    );
}

#[gpui_kit::test]
fn disabled_home_tun_keeps_checked_state_and_explains_why(cx: &mut gpui_kit::TestAppContext) {
    use crate::test_support::{RuntimePageFixture as Fixture, open_runtime_page_window as open};
    use gpui_kit::AppContext;
    use gpui_kit::test::TestWindowExt;

    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Home);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        let token = page.update(cx, |page, _| {
            let mut snapshot = page.monitors.operational_status.snapshot();
            snapshot.capture.tun = Observation::Fresh {
                value: TunCaptureStatus {
                    requested: true,
                    configured: true,
                    permission: CapabilityState::Active,
                    runtime: TunRuntimeObservation {
                        device_name: Some("utun9".into()),
                        device: CapabilityState::Active,
                        route: CapabilityState::Active,
                        detail: String::new(),
                    },
                    observed: CapabilityState::Active,
                },
                observed_at_ms: 1,
            };
            page.home.design_validation = Some((snapshot, Default::default()));
            page.begin_mutation(Page::Home).unwrap()
        });
        window.render_frame(cx);
        assert_eq!(window.find("home-tun").checked(), Some(true));
        assert_eq!(
            window.find("home-tun-status").label(),
            Some(zenclash_i18n::text("home.controls.tun_on").as_str())
        );
        assert_eq!(
            window.find("home-tun-disabled-reason").label(),
            Some(zenclash_i18n::text("tun.control.core_busy").as_str())
        );
        window.click("home-tun", cx);
        assert!(page.read(cx).home.capture_transition.is_none());
        page.update(cx, |page, cx| {
            page.finish_mutation(token);
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(window.find("home-tun").checked(), Some(true));
        assert!(window.try_find("home-tun-disabled-reason").is_none());
        page.update(cx, |page, cx| {
            let capture = &mut page.home.design_validation.as_mut().unwrap().0.capture;
            capture.tun = Observation::Stale {
                value: capture.tun.value().unwrap().clone(),
                observed_at_ms: 1,
                failure: zenclash_core::OperationalFailure {
                    message: "controller unavailable".into(),
                    occurred_at_ms: 2,
                },
            };
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(window.find("home-tun").checked(), Some(true));
        let expected = zenclash_i18n::text_with(
            "home.controls.tun_stale",
            &[
                ("status", zenclash_i18n::text("home.controls.tun_on")),
                ("age", format_profile_age(0)),
            ],
        );
        assert_eq!(
            window.find("home-tun-status").label(),
            Some(expected.as_str())
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn fresh_tun_readback_replaces_a_completed_off_transition(cx: &mut gpui_kit::TestAppContext) {
    use crate::test_support::{RuntimePageFixture as Fixture, open_runtime_page_window as open};
    use gpui_kit::AppContext;
    use gpui_kit::test::TestWindowExt;
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Home);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            let capture = |enabled, time| CaptureStatus {
                system_proxy: Observation::Fresh {
                    value: SystemProxySessionSnapshot {
                        intent_enabled: false,
                        actual: SystemProxyStatus::default(),
                        ownership: SystemProxyOwnershipState::Unowned,
                    },
                    observed_at_ms: time,
                },
                tun: Observation::Fresh {
                    value: TunCaptureStatus {
                        requested: enabled,
                        configured: enabled,
                        permission: CapabilityState::Active,
                        runtime: TunRuntimeObservation {
                            device_name: Some("test-tun".into()),
                            device: if enabled {
                                CapabilityState::Active
                            } else {
                                CapabilityState::Inactive
                            },
                            route: if enabled {
                                CapabilityState::Active
                            } else {
                                CapabilityState::Inactive
                            },
                            detail: String::new(),
                        },
                        observed: if enabled {
                            CapabilityState::Active
                        } else {
                            CapabilityState::Inactive
                        },
                    },
                    observed_at_ms: time,
                },
            };
            page.home.capture_transition = Some(CaptureTransition {
                generation: page.core_session.generation(),
                displayed: CapturePlan::Off,
                pending: false,
                confirmed: Some(capture(false, 1)),
            });
            let actual = capture(true, 2);
            page.reconcile_home_capture_transition(&actual);
            assert!(page.home.capture_transition.is_none());
            // Service completion must publish readback before the slower global observer.
            let readback = zenclash_core::TrafficCaptureSnapshot {
                system_proxy: actual.system_proxy.clone(),
                tun: actual.tun.clone(),
                observed_plan: zenclash_core::ObservedCapturePlan::TunConfigured,
                system_proxy_port: Some(7890),
                core_available: true,
            };
            page.accept_home_capture_snapshot(CapturePlan::Tun, &readback);
            page.reconcile_home_capture_transition(&capture(false, 1));
            assert!(
                page.home.capture_transition.is_some(),
                "old observer data must not replace service readback"
            );
            let mut snapshot = page.monitors.operational_status.snapshot();
            snapshot.capture = capture(false, 1);
            page.home.design_validation = Some((snapshot, Default::default()));
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(window.find("home-tun").checked(), Some(true));
        assert_eq!(
            window.find("home-tun-status").label(),
            Some(zenclash_i18n::text("home.controls.tun_on").as_str())
        );
        page.update(cx, |page, cx| {
            // A failed attempt must display the actual off snapshot, even when TUN was requested.
            let off = &page.home.design_validation.as_ref().unwrap().0.capture;
            let mut readback = zenclash_core::TrafficCaptureSnapshot {
                system_proxy: off.system_proxy.clone(),
                tun: off.tun.clone(),
                observed_plan: zenclash_core::ObservedCapturePlan::Off,
                system_proxy_port: Some(7890),
                core_available: true,
            };
            if let Observation::Fresh { observed_at_ms, .. } = &mut readback.tun {
                *observed_at_ms = 4;
            }
            if let Observation::Fresh { observed_at_ms, .. } = &mut readback.system_proxy {
                *observed_at_ms = 4;
            }
            page.accept_home_capture_snapshot(CapturePlan::Tun, &readback);
            let older = &mut page.home.design_validation.as_mut().unwrap().0.capture;
            if let Observation::Fresh {
                value,
                observed_at_ms,
            } = &mut older.tun
            {
                value.requested = true;
                value.configured = true;
                value.observed = CapabilityState::Active;
                *observed_at_ms = 2;
            }
            let older = older.clone();
            page.reconcile_home_capture_transition(&older);
            assert!(
                page.home.capture_transition.is_some(),
                "an older matching target must not overwrite actual rollback readback"
            );
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(window.find("home-tun").checked(), Some(false));
        window.remove_window();
    })
    .unwrap();
}

#[test]
fn traffic_chart_points_keep_upload_and_download_separate() {
    let samples = VecDeque::from([
        TrafficSample {
            upload: 10,
            download: 20,
        },
        TrafficSample {
            upload: 30,
            download: 40,
        },
    ]);

    let points = traffic_chart_points(&samples);

    assert_eq!((points[1].upload, points[1].download), (30., 40.));
}

#[test]
fn traffic_chart_points_have_unique_x_axis_labels() {
    let samples = VecDeque::from([TrafficSample::default(); 24]);

    let points = traffic_chart_points(&samples);

    assert!(points.windows(2).all(|pair| pair[0].label != pair[1].label));
}

#[test]
fn traffic_chart_ceiling_is_stable_within_one_power_of_two_band() {
    let lower = VecDeque::from([TrafficSample {
        download: 700 * 1_024,
        ..TrafficSample::default()
    }]);
    let higher = VecDeque::from([TrafficSample {
        download: 800 * 1_024,
        ..TrafficSample::default()
    }]);

    assert_eq!(
        traffic_chart_ceiling(&lower),
        traffic_chart_ceiling(&higher)
    );
}

#[test]
fn traffic_chart_last_point_is_labeled_as_now() {
    let samples = VecDeque::from([TrafficSample::default(); 3]);

    let points = traffic_chart_points(&samples);

    assert!(matches!(points[2].label.as_ref(), "现在" | "Now"));
}

#[test]
fn pending_mode_transition_is_presented_before_controller_readback() {
    let presentation = mode_presentation(
        OutboundMode::Rule,
        Some(ModeTransition {
            displayed: OutboundMode::Global,
            pending: true,
        }),
    );

    assert_eq!(
        (presentation.displayed, presentation.pending),
        (OutboundMode::Global, true)
    );
}

#[test]
fn pending_system_proxy_transition_is_presented_before_platform_readback() {
    let transition = CaptureTransition {
        generation: 0,
        displayed: CapturePlan::SystemProxy,
        pending: true,
        confirmed: None,
    };

    let presentation = capture_presentation(false, false, Some(&transition));

    assert_eq!(
        presentation,
        CapturePresentation {
            system_proxy_enabled: true,
            tun_enabled: false,
            pending: true,
        }
    );
}

#[test]
fn pending_tun_transition_hides_stale_system_proxy_state() {
    let transition = CaptureTransition {
        generation: 0,
        displayed: CapturePlan::Tun,
        pending: true,
        confirmed: None,
    };

    let presentation = capture_presentation(true, false, Some(&transition));

    assert_eq!(
        presentation,
        CapturePresentation {
            system_proxy_enabled: false,
            tun_enabled: true,
            pending: true,
        }
    );
}

#[test]
fn pending_off_transition_clears_system_proxy_before_platform_readback() {
    let transition = CaptureTransition {
        generation: 0,
        displayed: CapturePlan::Off,
        pending: true,
        confirmed: None,
    };

    let presentation = capture_presentation(true, false, Some(&transition));

    assert_eq!(
        presentation,
        CapturePresentation {
            system_proxy_enabled: false,
            tun_enabled: false,
            pending: true,
        }
    );
}

#[test]
fn completed_transition_stays_visible_until_global_observation_catches_up() {
    let transition = CaptureTransition {
        generation: 0,
        displayed: CapturePlan::SystemProxy,
        pending: false,
        confirmed: None,
    };

    let presentation = capture_presentation(false, false, Some(&transition));

    assert_eq!(
        presentation,
        CapturePresentation {
            system_proxy_enabled: true,
            tun_enabled: false,
            pending: false,
        }
    );
}
