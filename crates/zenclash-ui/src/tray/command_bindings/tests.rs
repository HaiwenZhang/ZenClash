use super::*;
use zenclash_core::{CoreKind, CoreSession, MihomoClient, MihomoEndpoint};

#[tokio::test]
async fn resolved_native_command_keeps_its_installed_observation_when_the_menu_is_replaced() {
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let first = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let second = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let commands = HashMap::from([(
        "old-id".to_owned(),
        TrayCommand::SelectProxy {
            group: "group".into(),
            proxy: "node".into(),
        },
    )]);
    let mut bindings = TrayCommandBindings::new(commands, Some(first.clone()));
    let resolved = bindings.resolve("old-id").unwrap();
    bindings = TrayCommandBindings::new(
        HashMap::from([(
            "new-id".to_owned(),
            TrayCommand::SelectProxy {
                group: "group".into(),
                proxy: "second".into(),
            },
        )]),
        Some(second.clone()),
    );
    assert!(bindings.resolve("old-id").is_none());
    let TrayEvent::Command(_, Some(captured)) = resolved else {
        panic!("missing installed menu provenance");
    };
    assert!(captured.is_same_observation(&first));
    assert!(!captured.is_same_observation(&second));
    let Some(TrayEvent::Command(_, Some(current))) = bindings.resolve("new-id") else {
        panic!("missing current menu provenance");
    };
    assert!(current.is_same_observation(&second));
}

#[tokio::test]
async fn updating_observation_in_place_preserves_existing_menu_command_ids() {
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let first = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let second = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let mut bindings = TrayCommandBindings::new(
        HashMap::from([(
            "stable-id".to_owned(),
            TrayCommand::SelectProxy {
                group: "group".into(),
                proxy: "node".into(),
            },
        )]),
        Some(first),
    );
    bindings.update_observation(Some(second.clone()));
    let Some(TrayEvent::Command(_, Some(current))) = bindings.resolve("stable-id") else {
        panic!("missing updated menu provenance");
    };
    assert!(current.is_same_observation(&second));
}

#[test]
fn an_unobserved_menu_cannot_fabricate_runtime_provenance_for_its_commands() {
    let bindings = TrayCommandBindings::new(
        HashMap::from([(
            "id".to_owned(),
            TrayCommand::SelectProxy {
                group: "group".into(),
                proxy: "node".into(),
            },
        )]),
        None,
    );
    assert!(matches!(
        bindings.resolve("id"),
        Some(TrayEvent::Command(_, None))
    ));
}
