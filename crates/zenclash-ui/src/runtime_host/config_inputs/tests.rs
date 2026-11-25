use serde_json::json;

use super::fields::FieldBaseline;
use super::{config_input_snapshot, config_source};

#[test]
fn unchanged_refresh_leaves_text_and_selection_untouched() {
    let mut baseline = FieldBaseline("saved".into());
    assert_eq!(baseline.refresh("saved", "saved".into()), None);
}

#[test]
fn background_refresh_preserves_edits_and_accepts_save_acknowledgements() {
    let mut baseline = FieldBaseline("original".into());
    assert_eq!(baseline.refresh("editing", "remote".into()), None);
    assert_eq!(baseline.refresh("editing", "editing".into()), None);
    assert_eq!(
        baseline.refresh("editing", "next remote".into()),
        Some("next remote".into())
    );
}

#[test]
fn typing_after_submit_survives_the_older_save_readback() {
    let mut baseline = FieldBaseline("original".into());
    assert_eq!(baseline.refresh("newer typing", "submitted".into()), None);
    assert_eq!(baseline.refresh("newer typing", "submitted".into()), None);
}

#[test]
fn acknowledged_input_is_normalized_without_discarding_later_typing() {
    let mut baseline = FieldBaseline("old".into());
    baseline.accept("  saved  ", "  saved  ");
    assert_eq!(
        baseline.refresh("  saved  ", "saved".into()),
        Some("saved".into())
    );
    baseline.accept("new typing", "submitted");
    assert_eq!(baseline.refresh("new typing", "submitted".into()), None);
}

#[test]
fn input_snapshot_discards_large_runtime_only_sections() {
    let snapshot = config_input_snapshot(json!({
        "mixed-port": 7890,
        "geodata-mode": false,
        "geo-auto-update": true,
        "dns": { "enable": false, "nameserver": ["1.1.1.1"] },
        "rules": ["DOMAIN-SUFFIX,example.com,DIRECT"],
        "proxies": [{ "name": "large-runtime-section" }]
    }));

    assert_eq!(snapshot.pointer("/mixed-port"), Some(&json!(7890)));
    assert_eq!(
        snapshot.pointer("/dns/nameserver/0"),
        Some(&json!("1.1.1.1"))
    );
    assert_eq!(snapshot.pointer("/dns/enable"), Some(&json!(false)));
    assert_eq!(snapshot.get("geodata-mode"), Some(&json!(false)));
    assert_eq!(snapshot.get("geo-auto-update"), Some(&json!(true)));
    assert!(snapshot.get("rules").is_none());
    assert!(snapshot.get("proxies").is_none());
}

#[test]
fn section_source_keeps_only_fields_used_by_its_patch() {
    let config = json!({
        "dns": { "enable": true },
        "hosts": { "router.local": "192.0.2.1" },
        "tun": { "enable": true }
    });

    assert_eq!(
        config_source(&config, &["dns", "hosts"]),
        json!({
            "dns": { "enable": true },
            "hosts": { "router.local": "192.0.2.1" }
        })
    );
}

#[gpui_kit::test]
fn reset_page_mihomo_restores_all_mihomo_inputs_without_touching_other_pages(
    cx: &mut gpui_kit::TestAppContext,
) {
    use std::path::Path;

    use gpui_kit::component::Root;
    use gpui_kit::{AppContext, IntoElement, Render, Window, div, px, size};

    struct EmptyView;
    impl Render for EmptyView {
        fn render(&mut self, _: &mut Window, _: &mut gpui_kit::Context<Self>) -> impl IntoElement {
            div()
        }
    }

    cx.update(gpui_kit::init);
    let handle = cx.open_window(size(px(400.), px(300.)), |window, cx| {
        let view = cx.new(|_| EmptyView);
        Root::new(view, window, cx)
    });
    cx.update_window(handle.into(), |_, window, cx| {
        let mut inputs = super::ConfigInputs::new(
            &json!({
                "port": 7891,
                "socks-port": 7892,
                "mixed-port": 7890,
                "redir-port": 7893,
                "tproxy-port": 7894,
                "bind-address": "127.0.0.1",
                "interface-name": "en0",
                "log-level": "info",
                "dns": { "nameserver": ["1.1.1.1"] }
            }),
            Some(Path::new("profile.yaml")),
            window,
            cx,
        );

        inputs
            .core
            .port
            .update(cx, |input, cx| input.set_value("10001", window, cx));
        inputs
            .core
            .socks_port
            .update(cx, |input, cx| input.set_value("10002", window, cx));
        inputs
            .core
            .mixed_port
            .update(cx, |input, cx| input.set_value("10000", window, cx));
        inputs
            .core
            .redir_port
            .update(cx, |input, cx| input.set_value("10003", window, cx));
        inputs
            .core
            .tproxy_port
            .update(cx, |input, cx| input.set_value("10004", window, cx));
        inputs
            .core
            .bind_address
            .update(cx, |input, cx| input.set_value("0.0.0.0", window, cx));
        inputs
            .core
            .interface_name
            .update(cx, |input, cx| input.set_value("eth0", window, cx));
        inputs
            .core
            .log_level
            .update(cx, |input, cx| input.set_value("debug", window, cx));
        inputs
            .dns
            .nameserver
            .update(cx, |input, cx| input.set_value("8.8.8.8", window, cx));

        inputs.reset_page(crate::foundation::actions::Page::Mihomo, window, cx);

        assert_eq!(inputs.core.port.read(cx).value(), "7891");
        assert_eq!(inputs.core.socks_port.read(cx).value(), "7892");
        assert_eq!(inputs.core.mixed_port.read(cx).value(), "7890");
        assert_eq!(inputs.core.redir_port.read(cx).value(), "7893");
        assert_eq!(inputs.core.tproxy_port.read(cx).value(), "7894");
        assert_eq!(inputs.core.bind_address.read(cx).value(), "127.0.0.1");
        assert_eq!(inputs.core.interface_name.read(cx).value(), "en0");
        assert_eq!(inputs.core.log_level.read(cx).value(), "info");
        assert_eq!(inputs.dns.nameserver.read(cx).value(), "8.8.8.8");

        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn clearing_tun_mtu_and_dns_fallback_geoip_emits_null_patches(cx: &mut gpui_kit::TestAppContext) {
    use std::path::Path;

    use gpui_kit::component::Root;
    use gpui_kit::{AppContext, IntoElement, Render, Window, div, px, size};

    struct EmptyView;
    impl Render for EmptyView {
        fn render(&mut self, _: &mut Window, _: &mut gpui_kit::Context<Self>) -> impl IntoElement {
            div()
        }
    }

    cx.update(gpui_kit::init);
    let handle = cx.open_window(size(px(400.), px(300.)), |window, cx| {
        let view = cx.new(|_| EmptyView);
        Root::new(view, window, cx)
    });
    cx.update_window(handle.into(), |_, window, cx| {
        let inputs = super::ConfigInputs::new(
            &json!({
                "tun": { "mtu": 1500 },
                "dns": { "fallback-filter": { "geoip": true } }
            }),
            Some(Path::new("profile.yaml")),
            window,
            cx,
        );

        inputs
            .tun
            .mtu
            .update(cx, |input, cx| input.set_value("", window, cx));
        inputs
            .dns
            .fallback_geoip
            .update(cx, |input, cx| input.set_value("", window, cx));

        let tun_patch = inputs.tun.patch(cx).expect("clearing mtu must succeed");
        assert_eq!(
            tun_patch.pointer("/tun/mtu"),
            Some(&serde_json::Value::Null)
        );

        let dns_patch = inputs
            .dns
            .patch(cx)
            .expect("clearing fallback geoip must succeed");
        assert_eq!(
            dns_patch.pointer("/dns/fallback-filter/geoip"),
            Some(&serde_json::Value::Null)
        );

        window.remove_window();
    })
    .unwrap();
}
