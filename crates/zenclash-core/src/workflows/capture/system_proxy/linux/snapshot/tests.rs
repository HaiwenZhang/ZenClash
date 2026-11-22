use super::*;

#[test]
fn gnome_port_variant_type_requires_int32_and_rejects_uint32() {
    assert!(validate_gnome_port_type("i").is_ok());
    for invalid in ["u", "q", "s", ""] {
        assert!(matches!(
            validate_gnome_port_type(invalid),
            Err(crate::MihomoError::SystemProxy(error))
                if matches!(
                    error.as_ref(),
                    crate::SystemProxyError::InvalidNativeSnapshot { .. }
                )
        ));
    }
}

#[test]
fn gnome_port_validation_accepts_u16_range_and_rejects_negative_or_overflow() {
    assert_eq!(parse_gnome_port(0).unwrap(), 0);
    assert_eq!(parse_gnome_port(7890).unwrap(), 7890);
    assert_eq!(parse_gnome_port(65535).unwrap(), 65535);

    for out_of_range in [-1, i32::MIN, 65536, i32::MAX] {
        assert!(matches!(
            parse_gnome_port(out_of_range),
            Err(crate::MihomoError::SystemProxy(error))
                if matches!(
                    error.as_ref(),
                    crate::SystemProxyError::InvalidNativeSnapshot { .. }
                )
        ));
    }
}

#[test]
fn gnome_manual_enable_predicate_requires_manual_mode_host_and_nonzero_port() {
    let active = build_summary(
        "manual",
        "127.0.0.1".into(),
        7890,
        "127.0.0.1".into(),
        7890,
        vec!["localhost".into()],
        String::new(),
    )
    .unwrap();
    assert!(active.enabled);
    assert!(active.secure_enabled);
    assert_eq!(active.port, 7890);
    assert_eq!(active.secure_port, 7890);

    let missing_host = build_summary(
        "manual",
        String::new(),
        7890,
        "127.0.0.1".into(),
        0,
        Vec::new(),
        String::new(),
    )
    .unwrap();
    assert!(!missing_host.enabled);
    assert!(!missing_host.secure_enabled);

    let asymmetric = build_summary(
        "manual",
        "127.0.0.1".into(),
        7890,
        String::new(),
        0,
        Vec::new(),
        String::new(),
    )
    .unwrap();
    assert!(asymmetric.enabled);
    assert!(!asymmetric.secure_enabled);

    let disabled = build_summary(
        "none",
        "127.0.0.1".into(),
        7890,
        "127.0.0.1".into(),
        7890,
        Vec::new(),
        String::new(),
    )
    .unwrap();
    assert!(!disabled.enabled);
    assert!(!disabled.secure_enabled);

    assert!(matches!(
        build_summary(
            "manual",
            "127.0.0.1".into(),
            -1,
            "127.0.0.1".into(),
            7890,
            Vec::new(),
            String::new(),
        ),
        Err(crate::MihomoError::SystemProxy(error))
            if matches!(
                error.as_ref(),
                crate::SystemProxyError::InvalidNativeSnapshot { .. }
            )
    ));
}
