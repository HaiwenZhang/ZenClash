use super::super::test_support::open_catalog;
use super::super::*;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{TestAppContext, size};

#[gpui_kit::test]
fn large_node_grid_renders_visible_rows_and_reveals_keyboard_targets(cx: &mut TestAppContext) {
    let nodes = (0..360)
        .map(|index| ProxyNode {
            name: format!("node-{index:03}"),
            ..Default::default()
        })
        .collect();
    let (window, page, _runtime) = open_catalog(
        cx,
        ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup::new("group")
                    .with_behavior(ProxyGroupBehavior::Selector)
                    .with_current_member("node-000"),
                nodes,
            )],
            360,
        ),
    );
    let node_element = |index: usize| {
        view::proxy_element_id(
            "select-proxy",
            "group",
            &ProxyNodeId::new(format!("node-{index:03}"), None),
        )
    };
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(node_element(0)).is_some());
        assert!(
            window.try_find(node_element(359)).is_none(),
            "offscreen cards should not be built"
        );
        let registered = (0..360)
            .filter(|&index| window.try_find(node_element(index)).is_some())
            .count();
        assert!(
            registered < 36,
            "constructed {registered} cards for one viewport"
        );
        let first = page.read(cx).node_focus
            [&("group".into(), ProxyNodeId::new("node-000".into(), None))]
            .clone();
        window.focus(&first, cx);
    })
    .unwrap();
    cx.run_until_parked();
    for step in 1..=50 {
        cx.update_window(window, |_, window, cx| {
            window.press("down", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let focused = page
                .read(cx)
                .node_focus
                .iter()
                .find(|(_, focus)| focus.is_focused(window))
                .map(|(key, _)| key.1.controller_name().to_owned());
            assert_eq!(
                focused.as_deref(),
                Some(format!("node-{:03}", step * 3).as_str()),
                "keyboard step {step}"
            );
            assert_eq!(window.find(node_element(step * 3)).focused(), Some(true));
            assert!(window.find(node_element(step * 3)).visible());
        })
        .unwrap();
    }
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(node_element(150)).focused(), Some(true));
        assert!(window.find(node_element(150)).visible());
    })
    .unwrap();
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn removing_the_displayed_group_releases_node_focus_and_grid_state(cx: &mut TestAppContext) {
    let (window, page, _runtime) = open_catalog(
        cx,
        ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup::new("group")
                    .with_behavior(ProxyGroupBehavior::Selector)
                    .with_current_member("current"),
                vec![ProxyNode {
                    name: "current".into(),
                    ..Default::default()
                }],
            )],
            1,
        ),
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.focus(
            &page.read(cx).node_focus[&("group".into(), ProxyNodeId::new("current".into(), None))]
                .clone(),
            cx,
        );
        page.update(cx, |page, cx| {
            page.install_catalog(ProxyCatalog::default(), "rule".into(), Vec::new());
            cx.notify();
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(page.read(cx).node_focus.is_empty());
        assert!(page.read(cx).details_focus.is_empty());
        assert!(page.read(cx).focus_handle.is_focused(window));
        assert!(window.try_find("proxy-node-panel").is_none());
    })
    .unwrap();
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn scrolled_node_grid_keeps_its_viewport_through_delay_feedback_and_zoom(cx: &mut TestAppContext) {
    let nodes = (0..360)
        .map(|index| ProxyNode {
            name: format!("node-{index:03}"),
            history: vec![DelayHistory {
                delay: index + 1,
                ..Default::default()
            }],
            ..Default::default()
        })
        .collect();
    let (window, page, _runtime) = open_catalog(
        cx,
        ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup::new("group")
                    .with_behavior(ProxyGroupBehavior::Selector)
                    .with_current_member("node-000"),
                nodes,
            )],
            360,
        ),
    );
    let node_element = |index: usize| {
        view::proxy_element_id(
            "select-proxy",
            "group",
            &ProxyNodeId::new(format!("node-{index:03}"), None),
        )
    };
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.focus(&page.read(cx).focus_handle.clone(), cx);
        window.scroll(
            "proxy-node-viewport",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-1200.))),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    let (anchor, before_bounds, before_scroll) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let anchor = (0..360)
                .find(|&index| {
                    window
                        .try_find(node_element(index))
                        .is_some_and(|node| node.visible())
                })
                .unwrap();
            assert!(
                anchor >= 9,
                "wheel did not move the viewport: node {anchor}, viewport {:?}, scroll {:?}",
                window.find("proxy-node-viewport").bounds(),
                page.read(cx).node_grid.scroll_top()
            );
            let bounds = window.find(node_element(anchor)).bounds();
            page.update(cx, |page, cx| {
                page.record_node_delay(&ProxyNodeId::new("node-000".into(), None), 999, 999, None);
                cx.notify();
            });
            (anchor, bounds, page.read(cx).node_grid.scroll_top())
        })
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(page.read(cx).focus_handle.is_focused(window));
        let after = window.find(node_element(anchor)).bounds();
        assert_eq!(after.top(), before_bounds.top());
        assert!(window.find(node_element(anchor)).visible());
    })
    .unwrap();
    cx.update(|cx| gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(20.)));
    cx.simulate_window_resize(window, size(px(720.), px(560.)));
    cx.update_window(window, |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let row_anchor = before_scroll.item_ix * 3;
        let node = window.try_find(node_element(row_anchor));
        assert!(node.is_some(), "zoom lost row anchor {row_anchor}; previous {:?}/{before_bounds:?}; viewport {:?}; scroll {:?}; registered {:?}", before_scroll,
            window.find("proxy-node-viewport").bounds(), page.read(cx).node_grid.scroll_top(),
            (0..360).filter(|&index| window.try_find(node_element(index)).is_some()).collect::<Vec<_>>());
        let node = node.unwrap();
        assert!(node.visible(), "row anchor bounds {:?}; viewport {:?}; scroll {:?}",
            node.bounds(), window.find("proxy-node-viewport").bounds(), page.read(cx).node_grid.scroll_top());
        assert!(node.bounds().right() <= window.viewport_size().width);
        assert!(node.bounds().size.height > before_bounds.size.height);
        assert!(window.try_find(node_element(0)).is_none());
    }).unwrap();
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn virtual_node_grid_preserves_offscreen_focus_during_sort_and_releases_filtered_nodes(
    cx: &mut TestAppContext,
) {
    let nodes: Vec<_> = (0..96)
        .map(|index| ProxyNode {
            name: format!("node-{index:03}"),
            history: vec![DelayHistory {
                delay: 96 - index,
                ..Default::default()
            }],
            ..Default::default()
        })
        .collect();
    let (window, page, _runtime) = open_catalog(
        cx,
        ProxyCatalog::from_group_nodes(
            ["group", "other"]
                .into_iter()
                .map(|group| {
                    (
                        ProxyGroup::new(group)
                            .with_behavior(ProxyGroupBehavior::Selector)
                            .with_current_member("node-000"),
                        nodes.clone(),
                    )
                })
                .collect(),
            96,
        ),
    );
    let first_id = ProxyNodeId::new("node-000".into(), None);
    let first_element = view::proxy_element_id("select-proxy", "group", &first_id);
    let first_focus = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let focus = page.read(cx).node_focus[&("group".into(), first_id.clone())].clone();
            window.focus(&focus, cx);
            window.scroll(
                "proxy-node-viewport",
                gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-1000.))),
                cx,
            );
            focus
        })
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(first_element.clone()).is_some(),
            "scroll discarded the focused row"
        );
        assert_eq!(window.find(first_element.clone()).focused(), Some(true));
        assert!(!window.find(first_element.clone()).visible());
        window.click("sort-proxies-by-latency", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(page.read(cx).sort_by_latency);
        assert_eq!(
            page.read(cx).node_focus[&("group".into(), first_id.clone())],
            first_focus
        );
        assert!(
            window.try_find(first_element.clone()).is_some(),
            "sort discarded the focused row; native focus: {}",
            first_focus.is_focused(window)
        );
        assert_eq!(window.find(first_element.clone()).focused(), Some(true));
        let state = page.read(cx);
        let catalog = state.catalog.as_ref().unwrap();
        assert_eq!(
            state.displayed_nodes(catalog, &catalog.groups()[0]).last(),
            Some(&0)
        );
        window.press("left", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let neighbor = view::proxy_element_id(
        "select-proxy",
        "group",
        &ProxyNodeId::new("node-001".into(), None),
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(neighbor.clone()).focused(), Some(true));
        window.scroll(
            "proxy-node-viewport",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(1000.))),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(neighbor.clone()).focused(), Some(true));
        assert!(!window.find(neighbor).visible());
        page.update(cx, |page, cx| {
            page.record_node_delay(&first_id, 0, 0, None);
            cx.notify();
        });
        window.click("hide-unavailable-proxies", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            !page
                .read(cx)
                .node_focus
                .contains_key(&("group".into(), first_id.clone()))
        );
        assert!(!first_focus.is_focused(window));
        assert!(window.try_find(first_element).is_none());
        window.click(
            (
                gpui_kit::ElementId::from("toggle-group"),
                "other".to_owned(),
            ),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            page.read(cx)
                .node_focus
                .keys()
                .all(|(group, _)| group == "other")
        );
        assert!(
            page.read(cx)
                .details_focus
                .keys()
                .all(|(group, _)| group == "other")
        );
        let other_first = view::proxy_element_id(
            "select-proxy",
            "other",
            &ProxyNodeId::new("node-095".into(), None),
        );
        assert!(window.find(other_first).visible());
    })
    .unwrap();
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn same_column_resize_keeps_deep_grid_scrolling_and_its_node_anchor(cx: &mut TestAppContext) {
    let nodes = (0..360)
        .map(|index| ProxyNode {
            name: format!("node-{index:03}"),
            ..Default::default()
        })
        .collect();
    let (window, page, _runtime) = open_catalog(
        cx,
        ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup::new("group").with_behavior(ProxyGroupBehavior::Selector),
                nodes,
            )],
            360,
        ),
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.scroll(
            "proxy-node-viewport",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-1200.))),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    let before = cx.update(|cx| page.read(cx).node_grid.scroll_top());
    assert!(before.item_ix >= 5);
    cx.simulate_window_resize(window, size(px(1280.), px(1000.)));
    cx.update_window(window, |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(page.read(cx).node_grid.scroll_top().item_ix, before.item_ix);
        window.scroll(
            "proxy-node-viewport",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-1200.))),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let after = page.read(cx).node_grid.scroll_top();
        assert!(
            after.item_ix >= before.item_ix + 5,
            "resized grid no longer scrolls: {before:?} -> {after:?}"
        );
        let id = ProxyNodeId::new(format!("node-{:03}", (after.item_ix + 1) * 3), None);
        assert!(
            window
                .find(view::proxy_element_id("select-proxy", "group", &id))
                .visible()
        );
    })
    .unwrap();
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn retiring_a_grid_hands_nested_measurement_focus_back_to_the_page(cx: &mut TestAppContext) {
    for retirement in ["group_change", "empty_catalog", "filtered_member"] {
        let nodes = vec![ProxyNode {
            name: "node".into(),
            ..Default::default()
        }];
        let catalog = ProxyCatalog::from_group_nodes(
            ["group", "other"]
                .into_iter()
                .map(|group| {
                    (
                        ProxyGroup::new(group).with_behavior(ProxyGroupBehavior::Selector),
                        nodes.clone(),
                    )
                })
                .collect(),
            1,
        );
        let (window, page, _runtime) = open_catalog(cx, catalog);
        let control = view::proxy_element_id(
            "test-proxy",
            "group",
            &ProxyNodeId::new("node".into(), None),
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.focus(&page.read(cx).focus_handle.clone(), cx);
            for _ in 0..40 {
                if window.find(control.clone()).focused() == Some(true) {
                    break;
                }
                window.press("tab", cx);
            }
            assert_eq!(window.find(control.clone()).focused(), Some(true));
            page.update(cx, |page, cx| {
                if retirement == "empty_catalog" {
                    page.install_catalog(ProxyCatalog::default(), "rule".into(), Vec::new());
                } else if retirement == "filtered_member" {
                    page.hide_unavailable = true;
                    page.record_node_delay(&ProxyNodeId::new("node".into(), None), 0, 0, None);
                } else {
                    page.selected_group = Some("other".into());
                }
                cx.notify();
            });
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(page.read(cx).focus_handle.is_focused(window));
            assert!(window.try_find(control).is_none());
        })
        .unwrap();
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
    }
}

#[gpui_kit::test]
fn background_latency_reordering_keeps_the_focused_measurement_control_actionable(
    cx: &mut TestAppContext,
) {
    let nodes = (0..96)
        .map(|index| ProxyNode {
            name: format!("node-{index:03}"),
            history: vec![DelayHistory {
                delay: index + 1,
                ..Default::default()
            }],
            ..Default::default()
        })
        .collect();
    let (window, page, _runtime) = open_catalog(
        cx,
        ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup::new("group")
                    .with_behavior(ProxyGroupBehavior::Selector)
                    .with_current_member("node-000"),
                nodes,
            )],
            96,
        ),
    );
    let id = ProxyNodeId::new("node-000".into(), None);
    let control = view::proxy_element_id("test-proxy", "group", &id);
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.sort_by_latency = true;
            cx.notify();
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.focus(&page.read(cx).focus_handle.clone(), cx);
        for _ in 0..40 {
            if window.find(control.clone()).focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find(control.clone()).focused(), Some(true));
        page.update(cx, |page, cx| {
            page.record_node_delay(&id, 9999, 9999, None);
            cx.notify();
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(control.clone()).focused(), Some(true));
        window.scroll(
            "proxy-node-viewport",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-1000.))),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(control.clone()).focused(), Some(true));
        assert!(window.find(control.clone()).visible());
        window.scroll(
            "proxy-node-viewport",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(1000.))),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(control.clone()).focused(), Some(true));
        assert!(!window.find(control).visible());
    })
    .unwrap();
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}
