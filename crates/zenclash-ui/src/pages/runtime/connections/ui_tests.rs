use crate::pages::runtime::test_support::{
    RuntimePageFixture as Fixture, open_runtime_page_window as open,
};
use crate::pages::runtime::*;
use gpui_kit::TestAppContext;
use gpui_kit::component::WindowExt;
use gpui_kit::test::TestWindowExt;

#[gpui_kit::test]
fn closed_list_and_pause_preserve_global_metrics_and_history(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Connections);
    fixture.wait_for_initial_reads(cx, &page);
    let snapshot = |ids: &[&str], bytes| {
        PageSnapshotData::Connections(Arc::new(ConnectionsSnapshot {
            connections: ids
                .iter()
                .map(|id| zenclash_core::Connection {
                    id: (*id).into(),
                    ..Default::default()
                })
                .collect(),
            upload_total: bytes,
            download_total: bytes * 2,
            memory: bytes * 3,
        }))
    };
    for (ids, bytes) in [(vec!["gone", "live"], 10), (vec!["live"], 20)] {
        cx.update(|cx| {
            page.update(cx, |page, cx| {
                page.invalidate_page_load();
                page.replace_page_data(
                    page.page_task_token_for(Page::Connections),
                    snapshot(&ids, bytes),
                    cx,
                );
            })
        });
        fixture.settle_connections(cx, &page);
    }
    cx.update_window(window, |_, window, cx| {
        window.clear_notifications(cx);
        window.render_frame(cx);
        window.click("connections-closed", cx);
    })
    .unwrap();
    fixture.settle_connections(cx, &page);
    cx.update_window(window, |_, window, cx| {
        let view = page.read(cx).connections.read(cx);
        assert_eq!(view.history.samples.len(), 2);
        assert_eq!(*view.history.samples.back().unwrap(), [1, 20, 40, 60]);
        assert_eq!(
            view.projection.as_ref().unwrap().snapshot.connections[0].id,
            "gone"
        );
        window.render_frame(cx);
        window.click("pause-connections-display", cx);
        page.update(cx, |page, cx| {
            page.replace_page_data(
                page.page_task_token_for(Page::Connections),
                snapshot(&["live"], 100),
                cx,
            );
        });
    })
    .unwrap();
    fixture.settle_connections(cx, &page);
    cx.update_window(window, |_, window, cx| {
        let view = page.read(cx).connections.read(cx);
        assert_eq!(view.paused_snapshot.as_ref().unwrap().upload_total, 20);
        assert_eq!(*view.history.samples.back().unwrap(), [1, 20, 40, 60]);
        window.render_frame(cx);
        window.click("pause-connections-display", cx);
    })
    .unwrap();
    fixture.settle_connections(cx, &page);
    cx.update_window(window, |_, window, cx| {
        let view = page.read(cx).connections.read(cx);
        assert_eq!(view.history.samples.len(), 3);
        assert_eq!(*view.history.samples.back().unwrap(), [1, 100, 200, 300]);
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn previous_page_uses_the_visible_page_after_connections_shrink(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Connections);
    fixture.wait_for_initial_reads(cx, &page);
    for count in [501, 51] {
        cx.update(|cx| {
            page.update(cx, |page, cx| {
                page.invalidate_page_load();
                page.replace_page_data(
                    page.page_task_token_for(Page::Connections),
                    PageSnapshotData::Connections(Arc::new(ConnectionsSnapshot {
                        connections: (0..count)
                            .map(|index| zenclash_core::Connection {
                                id: format!("row-{index}"),
                                ..Default::default()
                            })
                            .collect(),
                        ..Default::default()
                    })),
                    cx,
                );
                if count == 501 {
                    page.connections
                        .update(cx, |view, cx| view.set_connections_page(10, cx));
                }
            })
        });
        fixture.settle_connections(cx, &page);
    }
    cx.update_window(window, |_, window, cx| {
        window.clear_notifications(cx);
        window.render_frame(cx);
        let first_row = gpui_kit::ElementId::from((
            gpui_kit::ElementId::from("connection-details"),
            "row-0".to_owned(),
        ));
        assert!(window.try_find(first_row.clone()).is_none());
        window.click("previous-connections-page", cx);
        window.render_frame(cx);
        assert_eq!(page.read(cx).connections.read(cx).page_index, 0);
        assert!(window.find(first_row).visible());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn busy_hidden_connections_cancel_presentation_and_resume_retained_data(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Connections);
    fixture.settle(cx, &page, |page| !page.persistent_loading && !page.loading);
    cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.invalidate_page_load();
            page.replace_page_data(
                page.page_task_token_for(Page::Connections),
                PageSnapshotData::Connections(Arc::new(ConnectionsSnapshot {
                    connections: vec![zenclash_core::Connection {
                        id: "retained".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                })),
                cx,
            );
            let mutation = page.begin_mutation(Page::Connections).unwrap();
            assert!(!page.connections.read(cx).is_idle());
            page.set_window_visible(false, cx);
            assert!(page.connections.read(cx).is_idle());
            assert!(
                page.core_busy(),
                "hiding presentation must preserve the admitted command"
            );
            assert!(matches!(
                page.page_observation.data(),
                PageSnapshotData::Connections(_)
            ));
            page.set_window_visible(true, cx);
            page.finish_mutation(mutation);
            cx.notify();
        });
    });
    fixture.settle_connections(cx, &page);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find(gpui_kit::ElementId::from((
                    gpui_kit::ElementId::from("connection-details"),
                    "retained".to_owned(),
                )))
                .is_some()
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn connection_rule_cells_display_the_rule_and_payload(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Connections);
    fixture.settle(cx, &page, |page| !page.persistent_loading && !page.loading);
    cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.invalidate_page_load();
            page.replace_page_data(
                page.page_task_token_for(Page::Connections),
                PageSnapshotData::Connections(Arc::new(ConnectionsSnapshot {
                    connections: vec![
                        zenclash_core::Connection {
                            id: "domain".into(),
                            rule: "DomainSuffix".into(),
                            rule_payload: "example.com".into(),
                            ..Default::default()
                        },
                        zenclash_core::Connection {
                            id: "match".into(),
                            rule: "Match".into(),
                            ..Default::default()
                        },
                        zenclash_core::Connection {
                            id: "unknown".into(),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                })),
                cx,
            );
        });
    });
    fixture.settle_connections(cx, &page);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        for (id, label) in [
            ("domain", "DomainSuffix (example.com)"),
            ("match", "Match"),
            ("unknown", "—"),
        ] {
            let rule = window.find(gpui_kit::ElementId::from((
                gpui_kit::ElementId::from("connection-rule"),
                id.to_owned(),
            )));
            assert_eq!(rule.label(), Some(label));
        }
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn connection_navigation_cancels_projection_and_preserves_the_transport_filter(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Connections);
    fixture.settle(cx, &page, |page| !page.persistent_loading && !page.loading);
    let snapshot = Arc::new(ConnectionsSnapshot {
        connections: ["TCP", "UDP"]
            .into_iter()
            .map(|network| zenclash_core::Connection {
                id: network.into(),
                metadata: zenclash_core::ConnectionMetadata {
                    network: network.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    });
    cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.invalidate_page_load();
            page.replace_page_data(
                page.page_task_token_for(Page::Connections),
                PageSnapshotData::Connections(snapshot.clone()),
                cx,
            );
        });
    });
    fixture.settle_connections(cx, &page);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(("connection-transport", 1usize), cx);
        page.update(cx, |page, cx| {
            assert!(!page.connections.read(cx).is_idle());
            page.switch_to(Page::Rules, cx);
            assert!(page.connections.read(cx).is_idle());
        });
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("connection-statistics").is_none());
        page.update(cx, |page, cx| {
            page.switch_to(Page::Connections, cx);
            page.invalidate_page_load();
            page.replace_page_data(
                page.page_task_token_for(Page::Connections),
                PageSnapshotData::Connections(snapshot),
                cx,
            );
        });
    })
    .unwrap();
    fixture.settle_connections(cx, &page);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let row = |id: &str| {
            gpui_kit::ElementId::from((
                gpui_kit::ElementId::from("connection-details"),
                id.to_owned(),
            ))
        };
        assert!(window.try_find(row("TCP")).is_some());
        assert!(window.try_find(row("UDP")).is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn connection_transport_buttons_filter_rows_with_pointer_and_keyboard(cx: &mut TestAppContext) {
    use gpui_kit::component::WindowExt;
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Connections);
    fixture.settle(cx, &page, |page| !page.persistent_loading && !page.loading);
    cx.update_window(window, |_, _, cx| {
        page.update(cx, |page, cx| {
            page.invalidate_page_load();
            page.replace_page_data(
                page.page_task_token_for(Page::Connections),
                PageSnapshotData::Connections(Arc::new(zenclash_core::ConnectionsSnapshot {
                    connections: ["TCP", "UDP"]
                        .into_iter()
                        .map(|network| zenclash_core::Connection {
                            id: network.into(),
                            metadata: zenclash_core::ConnectionMetadata {
                                network: network.into(),
                                host: format!("{}.example", network.to_ascii_lowercase()),
                                ..Default::default()
                            },
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                })),
                cx,
            );
        });
    })
    .unwrap();
    fixture.settle_connections(cx, &page);
    let row = |id: &str| {
        gpui_kit::ElementId::from((
            gpui_kit::ElementId::from("connection-details"),
            id.to_owned(),
        ))
    };
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(row("TCP"));
        window.find(row("UDP"));
        window.click(("connection-transport", 1usize), cx);
    })
    .unwrap();
    fixture.settle_connections(cx, &page);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(row("TCP"));
        assert!(window.try_find(row("UDP")).is_none());
        let focus = page.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        for _ in 0..12 {
            if window.find(("connection-transport", 2usize)).focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(
            window.find(("connection-transport", 2usize)).focused(),
            Some(true)
        );
        window.press("enter", cx);
    })
    .unwrap();
    fixture.settle_connections(cx, &page);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(row("UDP"));
        assert!(window.try_find(row("TCP")).is_none());
        window.click("pause-connections-display", cx);
        page.update(cx, |page, cx| {
            page.replace_page_data(
                page.page_task_token_for(Page::Connections),
                PageSnapshotData::Connections(Arc::new(zenclash_core::ConnectionsSnapshot {
                    connections: vec![zenclash_core::Connection {
                        id: "latest".into(),
                        metadata: zenclash_core::ConnectionMetadata {
                            network: "UDP".into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }],
                    ..Default::default()
                })),
                cx,
            );
        });
        window.render_frame(cx);
        window.find(row("UDP"));
        assert!(window.try_find(row("latest")).is_none());
        window.click("close-all-connections", cx);
        assert!(window.has_active_dialog(cx));
        window.press("escape", cx);
        assert!(!window.has_active_dialog(cx));
        window.find(row("UDP"));
        assert!(window.try_find(row("latest")).is_none());
        window.render_frame(cx);
        window.click(("connection-transport", 0usize), cx);
    })
    .unwrap();
    fixture.settle_connections(cx, &page);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(row("TCP"));
        window.find(row("UDP"));
        assert!(window.try_find(row("latest")).is_none());
        window.click("pause-connections-display", cx);
    })
    .unwrap();
    fixture.settle_connections(cx, &page);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(row("latest"));
        assert!(window.try_find(row("UDP")).is_none());
        assert!(window.try_find(row("TCP")).is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn connections_paginate_fifty_rows_and_keep_statistics_visible(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Connections);
    fixture.settle(cx, &page, |page| !page.persistent_loading && !page.loading);
    cx.update_window(window, |_, _, cx| {
        page.update(cx, |page, cx| {
            page.invalidate_page_load();
            page.replace_page_data(
                page.page_task_token_for(Page::Connections),
                PageSnapshotData::Connections(Arc::new(zenclash_core::ConnectionsSnapshot {
                    connections: (0..101)
                        .map(|index| zenclash_core::Connection {
                            id: format!("row-{index}"),
                            metadata: zenclash_core::ConnectionMetadata {
                                host: format!("host-{index}.example"),
                                ..Default::default()
                            },
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                })),
                cx,
            );
        })
    })
    .unwrap();
    fixture.settle_connections(cx, &page);
    for (expected_page, expected_rows) in [(0, 50), (1, 50), (2, 1)] {
        cx.update_window(window, |_, window, cx| {
            // The offline controller fixture leaves an error toast over the footer.
            window.clear_notifications(cx);
            window.render_frame(cx);
            assert!(
                window
                    .try_find(gpui_kit::ElementId::from((
                        gpui_kit::ElementId::from("connection-details"),
                        format!("row-{}", expected_page * 50)
                    )))
                    .is_some()
            );
            let rendered = (0..101)
                .filter(|index| {
                    window
                        .try_find(gpui_kit::ElementId::from((
                            gpui_kit::ElementId::from("connection-details"),
                            format!("row-{index}"),
                        )))
                        .is_some()
                })
                .count();
            assert_eq!(rendered, expected_rows);
            assert!(window.find("connection-statistics").visible());
            assert!(window.try_find("connection-analytics-toggle").is_none());
            if expected_page < 2 {
                let next = window.find("next-connections-page");
                assert_ne!(
                    next.disabled(),
                    Some(true),
                    "page {expected_page} next is disabled"
                );
                assert!(
                    next.visible(),
                    "page {expected_page} next is outside viewport: {:?}",
                    next.bounds()
                );
                window.hover("connection-statistics", cx);
                window.click("next-connections-page", cx);
            }
        })
        .unwrap();
        cx.run_until_parked();
    }
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}
