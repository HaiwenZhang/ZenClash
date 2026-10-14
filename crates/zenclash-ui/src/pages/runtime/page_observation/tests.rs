use super::*;
use zenclash_core::{CoreKind, MihomoClient, MihomoEndpoint, RuleCatalog, RuntimeConfig};

fn session() -> CoreSession {
    CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::default()).unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn accepted_snapshot_keeps_its_original_source_through_cache_and_restore() {
    let session = session();
    let source = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let catalog = std::sync::Arc::new(RuleCatalog::default());
    let mut owner = PageObservationOwner::new(session.generation());
    assert!(
        source
            .publish_if_current(|| owner.adopt(
                PageSnapshotData::Rules {
                    catalog: catalog.clone(),
                    config: None,
                    proxies: None,
                },
                source.clone(),
            ))
            .is_some()
    );
    owner.retain_current(Page::Rules);
    assert!(matches!(owner.data(), PageSnapshotData::Empty));
    assert!(owner.source().is_none());
    owner.restore_current(Page::Rules, &session);
    let PageSnapshotData::Rules {
        catalog: restored, ..
    } = owner.data()
    else {
        panic!("expected cached rule catalog");
    };
    assert!(std::sync::Arc::ptr_eq(&catalog, restored));
    assert!(
        owner
            .source()
            .as_ref()
            .unwrap()
            .is_same_observation(&source)
    );
    assert_eq!(
        owner.configuration_generation(),
        source.configuration_generation()
    );
    session
        .switch_to_direct(MihomoEndpoint::new("http://127.0.0.1:9", ""))
        .await
        .unwrap();
    owner.retain_current(Page::Rules);
    owner.restore_current(Page::Rules, &session);
    assert!(matches!(owner.data(), PageSnapshotData::Empty));
    assert!(owner.source().is_none());
}

#[tokio::test]
async fn unobserved_replacement_retires_the_source_without_inventing_read_authority() {
    let session = session();
    let source = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let mut owner = PageObservationOwner::new(session.generation());
    assert!(
        source
            .publish_if_current(|| owner.adopt(
                PageSnapshotData::Config(RuntimeConfig::default()),
                source.clone()
            ))
            .is_some()
    );
    owner.replace_unobserved(
        PageSnapshotData::Config(RuntimeConfig {
            mode: "global".into(),
            ..RuntimeConfig::default()
        }),
        session.generation(),
    );
    assert!(owner.source().is_none());
    assert_eq!(owner.config(session.generation()).unwrap().mode, "global");
    assert!(owner.config(session.generation().wrapping_add(1)).is_none());
    owner.retain_current(Page::Dns);
    assert!(owner.restore(Page::Dns, &session).is_none());
}

#[tokio::test]
async fn dashboard_adoption_retains_success_only_from_the_same_configuration() {
    use zenclash_core::{Observation, OperationalFailure, ProxyCatalog, RecoveryAction};
    let session = session();
    let source = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let mut owner = PageObservationOwner::new(source.configuration_generation());
    let previous = PageSnapshotData::Dashboard {
        config: Observation::Fresh {
            value: RuntimeConfig::default(),
            observed_at_ms: 10,
        },
        proxies: Observation::Fresh {
            value: ProxyCatalog::from_group_nodes(Vec::new(), 42),
            observed_at_ms: 10,
        },
    };
    owner.replace_unobserved(previous.clone(), source.configuration_generation());
    let failed = PageSnapshotData::Dashboard {
        config: Observation::Fresh {
            value: RuntimeConfig::default(),
            observed_at_ms: 20,
        },
        proxies: Observation::Failed {
            failure: OperationalFailure {
                message: "offline".into(),
                occurred_at_ms: 20,
            },
            recovery: RecoveryAction::Retry,
        },
    };
    assert!(
        source
            .publish_if_current(|| owner.adopt(failed.clone(), source.clone()))
            .is_some()
    );
    let PageSnapshotData::Dashboard { proxies, .. } = owner.data() else {
        panic!("expected dashboard");
    };
    assert_eq!(proxies.value().unwrap().proxy_count, 42);
    assert!(
        owner
            .source()
            .as_ref()
            .unwrap()
            .is_same_observation(&source)
    );
    owner.replace_unobserved(previous, source.configuration_generation().wrapping_add(1));
    assert!(
        source
            .publish_if_current(|| owner.adopt(failed, source.clone()))
            .is_some()
    );
    let PageSnapshotData::Dashboard { proxies, .. } = owner.data() else {
        panic!("expected dashboard");
    };
    assert!(proxies.value().is_none());
    assert_eq!(
        owner.configuration_generation(),
        source.configuration_generation()
    );
}

#[tokio::test]
async fn navigation_does_not_reuse_config_from_a_different_snapshot_generation() {
    let session = session();
    let source = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let generation = source.configuration_generation();
    let mut owner = PageObservationOwner::new(generation);
    owner.reuse_config(
        Some(RuntimeConfig::default()),
        Some(source.clone()),
        generation,
    );
    assert!(
        owner
            .source()
            .as_ref()
            .unwrap()
            .is_same_observation(&source)
    );
    owner.release_data();
    owner.reuse_config(
        Some(RuntimeConfig::default()),
        Some(source),
        generation.wrapping_add(1),
    );
    assert!(matches!(owner.data(), PageSnapshotData::Empty));
    assert!(owner.config(generation.wrapping_add(1)).is_none());
}

#[tokio::test]
async fn cached_observation_moves_the_rule_catalog_and_rejects_replaced_runtime() {
    let session = session();
    let identity = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let catalog = std::sync::Arc::new(RuleCatalog::default());
    let mut owner = PageObservationOwner::default();
    owner.retain(
        Page::Rules,
        Some(identity.clone()),
        PageSnapshotData::Rules {
            catalog: catalog.clone(),
            config: None,
            proxies: None,
        },
    );
    let mut restored = None;
    assert!(
        owner
            .restore(Page::Rules, &session)
            .unwrap()
            .publish(|data, _| restored = Some(data.unwrap()))
    );
    let PageSnapshotData::Rules {
        catalog: returned, ..
    } = restored.unwrap()
    else {
        panic!("expected rule snapshot");
    };
    assert!(std::sync::Arc::ptr_eq(&catalog, &returned));
    owner.retain(
        Page::Dns,
        Some(identity),
        PageSnapshotData::Config(RuntimeConfig::default()),
    );
    session
        .switch_to_direct(MihomoEndpoint::new("http://127.0.0.1:9", ""))
        .await
        .unwrap();
    assert!(owner.restore(Page::Dns, &session).is_none());
}

#[tokio::test]
async fn cache_bounds_retained_pages_and_does_not_cross_session_owners() {
    let first = session();
    let second = session();
    let identity = first.begin_observation().await.unwrap().identity().clone();
    let mut owner = PageObservationOwner::default();
    for page in [
        Page::Dns,
        Page::Sniffer,
        Page::Tun,
        Page::Mihomo,
        Page::Resources,
    ] {
        owner.retain(
            page,
            Some(identity.clone()),
            PageSnapshotData::Config(RuntimeConfig::default()),
        );
    }
    assert!(owner.restore(Page::Dns, &first).is_none());
    assert!(owner.restore(Page::Sniffer, &first).is_some());
    assert!(owner.restore(Page::Resources, &second).is_none());
}

#[tokio::test]
async fn cancelled_page_read_never_cancels_an_admitted_business_completion() {
    let mut owner = PageObservationOwner::default();
    let request = owner.begin(Page::Dns, 1);
    let session = session();
    let admission = session.begin_data_application().await.unwrap();
    let read = tokio::spawn(async { std::future::pending::<()>().await });
    owner.track(request, &read);
    let (release, released) = tokio::sync::oneshot::channel();
    let command = tokio::spawn(async move {
        let _admission = admission;
        released.await.unwrap();
        7
    });
    owner.cancel();
    assert!(read.await.unwrap_err().is_cancelled());
    assert!(!owner.finish(request, Page::Dns, 1));
    release.send(()).unwrap();
    assert_eq!(command.await.unwrap(), 7);
}

#[gpui_kit::test]
fn a_panicked_page_read_retains_its_configuration_source_for_error_publication(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::pages::runtime::{
        test_support::controller_fixture::ControllerFixture,
        test_support::{RuntimePageFixture as Fixture, open_runtime_page_window as open},
    };
    use gpui_kit::AppContext;
    let controller = ControllerFixture::new(false);
    let fixture = Fixture::with_controller(controller.endpoint_url().to_owned());
    let services = fixture.services();
    let (window, page) = open(cx, &fixture, Page::Home);
    fixture.settle(cx, &page, |page| !page.loading && !page.persistent_loading);
    let source = std::sync::Arc::new(RuntimeReadSource::default());
    let submitted = source.clone();
    let session = services.core_session.clone();
    let error = services
        .runtime
        .block_on(services.runtime.spawn(async move {
            ObservedPage::load_with_source(session, Page::Home, &submitted)
                .await
                .unwrap();
            panic!("fixture page worker failed after its controller read");
        }))
        .unwrap_err()
        .to_string();
    cx.update(|cx| {
        page.update(cx, |page, _| {
            assert!(page.publish_page_refresh_error(&source, error.clone()));
            assert!(
                page.error
                    .as_deref()
                    .unwrap()
                    .contains("fixture page worker")
            );
        })
    });
    services
        .runtime
        .block_on(
            services
                .core_session
                .set_mode(&services.controlled_config_store, "global"),
        )
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, _| {
            page.error = Some("current configuration feedback".into());
            assert!(!page.publish_page_refresh_error(&source, error));
            assert_eq!(
                page.error.as_deref(),
                Some("current configuration feedback")
            );
        });
        window.remove_window();
    })
    .unwrap();
}

#[tokio::test]
async fn replacing_a_page_read_cancels_only_the_previous_request() {
    let mut owner = super::PageReadTask::default();
    let previous = tokio::spawn(std::future::pending::<()>());
    let current = tokio::spawn(std::future::pending::<()>());
    owner.replace(&previous);
    owner.replace(&current);
    assert!(previous.await.unwrap_err().is_cancelled());
    assert!(!current.is_finished());
    drop(owner);
    assert!(current.await.unwrap_err().is_cancelled());
}
#[tokio::test]
async fn invalidating_a_read_leaves_mutation_tasks_running() {
    let mut owner = super::PageReadTask::default();
    let read = tokio::spawn(std::future::pending::<()>());
    let (finish, pending) = tokio::sync::oneshot::channel();
    let mutation = tokio::spawn(async move { pending.await.unwrap() });
    owner.replace(&read);
    owner.cancel();
    assert!(read.await.unwrap_err().is_cancelled());
    finish.send(42).unwrap();
    assert_eq!(mutation.await.unwrap(), 42);
}
