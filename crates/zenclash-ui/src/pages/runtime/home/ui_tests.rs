use crate::pages::runtime::test_support::{
    ControllerFixture, RuntimePageFixture as Fixture, open_runtime_page_window as open,
};
use crate::pages::runtime::*;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{TestAppContext, VisualTestContext};

#[gpui_kit::test]
fn home_selector_cards_and_overflow_menu_apply_to_the_displayed_group(cx: &mut TestAppContext) {
    let controller = ControllerFixture::new(false);
    let mut proxies = serde_json::Map::new();
    for index in 0..20 {
        proxies.insert(
            format!("node-{index}"),
            serde_json::json!({"name":format!("node-{index}"),"type":"Shadowsocks"}),
        );
    }
    for index in 0..8 {
        proxies.insert(
            format!("group-{index}"),
            serde_json::json!({
                "name":format!("group-{index}"),"type":"Selector","now":"node-19",
                "all":(0..20).map(|index| format!("node-{index}")).collect::<Vec<_>>()
            }),
        );
    }
    *controller.proxy_catalog.lock().unwrap() = serde_json::json!({"proxies":proxies});
    let fixture = Fixture::with_controller(controller.url.clone());
    let (window, page) = open(cx, &fixture, Page::Home);
    fixture.settle(cx, &page, |page| {
        !page.loading
            && !page.persistent_loading
            && matches!(
                page.page_observation.data(),
                PageSnapshotData::Dashboard { .. }
            )
    });
    cx.run_until_parked();
    let group_card = |name: &str| {
        gpui_kit::ElementId::from((
            gpui_kit::ElementId::from("home-group-card"),
            name.to_owned(),
        ))
    };
    let node_card = |name: &str| {
        gpui_kit::ElementId::from((gpui_kit::ElementId::from("home-node-card"), name.to_owned()))
    };
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(group_card("group-0")).selected(), Some(true));
        assert_eq!(window.find(node_card("node-19")).selected(), Some(true));
        window.click(group_card("group-1"), cx);
        window.render_frame(cx);
        assert_eq!(window.find(group_card("group-1")).selected(), Some(true));
        assert!(controller.proxy_selections.lock().unwrap().is_empty());
        assert!(window.try_find("home-open-proxies").is_none());
        assert!(window.try_find("home-open-profiles").is_none());
        assert!(window.try_find("home-activity-panel").is_none());
    })
    .unwrap();
    VisualTestContext::from_window(window, cx).deactivate_window();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(group_card("group-1")).selected(), Some(true));
        assert_eq!(window.find(node_card("node-19")).selected(), Some(true));
        window.activate_window();
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(group_card("group-1")).selected(), Some(true));
        window.click("home-more-groups", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(group_card("group-1")).selected(), Some(true));
        window.click("home-more-groups", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(group_card("group-0")).selected(), Some(true));
        window.click(node_card("node-0"), cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.loading
            && !page.home.proxy_selection.any_pending()
            && !controller.proxy_selections.lock().unwrap().is_empty()
            && page.home.proxy_selection.status_text("group-0").is_none()
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            controller.proxy_selections.lock().unwrap().as_slice(),
            &[(
                "/proxies/group-0".into(),
                serde_json::json!({"name":"node-0"})
            )]
        );
        assert_eq!(window.find(node_card("node-0")).selected(), Some(true));
        assert_eq!(window.find(group_card("group-0")).selected(), Some(true));
        window.click("home-more-nodes", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("escape", cx);
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn open_home_menus_rescale_without_losing_keyboard_selection(cx: &mut TestAppContext) {
    let controller = ControllerFixture::new(false);
    let mut proxies = serde_json::Map::new();
    for index in 0..20 {
        proxies.insert(
            format!("node-{index}"),
            json!({"name":format!("node-{index}"),"type":"Shadowsocks"}),
        );
    }
    for index in 0..8 {
        proxies.insert(
            format!("group-{index}"),
            json!({"name":format!("group-{index}"),"type":"Selector","now":"node-19",
                "all":(0..20).map(|index| format!("node-{index}")).collect::<Vec<_>>() }),
        );
    }
    *controller.proxy_catalog.lock().unwrap() = json!({"proxies":proxies});
    let fixture = Fixture::with_controller(controller.url.clone());
    let (window, page) = open(cx, &fixture, Page::Home);
    fixture.settle(cx, &page, |page| {
        !page.loading
            && !page.persistent_loading
            && matches!(
                page.page_observation.data(),
                PageSnapshotData::Dashboard { .. }
            )
    });
    for (trigger, minimum_width, maximum_width, maximum_height) in [
        ("home-profile-picker", 15., 26.25, 22.5),
        ("home-more-groups", 15., 26., 22.),
        ("home-more-nodes", 20., 32., 22.),
    ] {
        cx.update(|cx| gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(12.)));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.focus(&page.read(cx).focus_handle.clone(), cx);
            window.click(trigger, cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.press("down", cx);
            window.press("down", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let selected = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                let menu = window.find("popup-menu");
                assert_eq!(menu.focused(), Some(true));
                let selected = window.within("popup-menu").find(1usize).selected();
                assert_eq!(selected, Some(true));
                selected
            })
            .unwrap();
        for font in [20., 16., 12.] {
            if font == 20. {
                cx.update_window(window, |_, _, cx| {
                    gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(font));
                })
                .unwrap();
            } else {
                cx.update(|cx| {
                    gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(font))
                });
            }
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                let menu = window.find("popup-menu");
                assert_eq!(menu.focused(), Some(true));
                assert_eq!(
                    window.within("popup-menu").find(1usize).selected(),
                    selected
                );
                assert!(
                    f32::from(menu.bounds().size.width) >= minimum_width * font,
                    "{trigger} did not grow with rem: {:?}",
                    menu.bounds()
                );
                assert!(f32::from(menu.bounds().size.width) <= maximum_width * font + 2.);
                assert!(f32::from(menu.bounds().size.height) <= maximum_height * font + 2.);
                if trigger == "home-more-nodes" {
                    assert!(
                        (f32::from(menu.bounds().size.height) - maximum_height * font).abs() <= 2.
                    );
                }
                assert!(menu.bounds().right() <= window.viewport_size().width);
                assert!(menu.bounds().bottom() <= window.viewport_size().height);
            })
            .unwrap();
        }
        cx.update_window(window, |_, window, cx| window.press("escape", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("popup-menu").is_none());
            assert!(page.read(cx).focus_handle.is_focused(window));
        })
        .unwrap();
    }
    assert!(controller.proxy_selections.lock().unwrap().is_empty());
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}
