use super::*;
use gpui_kit::AppContext;

#[test]
fn coalescing_delay_starts_only_after_the_first_measurement() {
    use futures_util::FutureExt;

    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let started = std::cell::Cell::new(false);
    let mut batch = std::pin::pin!(receive_batch(&mut receiver, || {
        started.set(true);
        std::future::pending::<()>()
    }));
    assert!(batch.as_mut().now_or_never().is_none());
    assert!(!started.get());
    sender.try_send(42).unwrap();
    assert!(batch.as_mut().now_or_never().is_none());
    assert!(started.get());
}

#[gpui_kit::test]
fn batch_wait_can_start_on_the_gpui_executor_without_a_tokio_runtime(
    cx: &mut gpui_kit::TestAppContext,
) {
    use futures_util::FutureExt;

    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    sender.try_send(42).unwrap();
    let _ = receive_batch(&mut receiver, || {
        cx.background_executor
            .timer(std::time::Duration::from_millis(50))
    })
    .now_or_never();
}

#[tokio::test]
async fn batches_bound_ui_work_without_losing_results() {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(128);
    for index in 0..100 {
        sender.send(index).await.unwrap();
    }
    drop(sender);
    assert_eq!(
        receive_batch(&mut receiver, || tokio::time::sleep(
            std::time::Duration::from_millis(50)
        ))
        .await
        .unwrap(),
        (0..64).collect::<Vec<_>>()
    );
    assert_eq!(
        receive_batch(&mut receiver, || tokio::time::sleep(
            std::time::Duration::from_millis(50)
        ))
        .await
        .unwrap(),
        (64..100).collect::<Vec<_>>()
    );
    assert!(
        receive_batch(&mut receiver, || tokio::time::sleep(
            std::time::Duration::from_millis(50)
        ))
        .await
        .is_none()
    );
}

#[tokio::test]
async fn measurement_results_are_published_before_the_producer_finishes() {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    sender.send(42).await.unwrap();
    let batch = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        receive_batch(&mut receiver, || {
            tokio::time::sleep(std::time::Duration::from_millis(50))
        }),
    )
    .await
    .unwrap();
    assert_eq!(batch, Some(vec![42]));
    assert!(!sender.is_closed());
}

#[tokio::test]
async fn suspending_presentation_cancels_all_registered_work() {
    let mut tasks = PresentationTasks::default();
    let first = tokio::spawn(std::future::pending::<()>());
    let second = tokio::spawn(std::future::pending::<()>());
    tasks.track(&first);
    tasks.track(&second);
    tasks.cancel();
    assert!(first.await.unwrap_err().is_cancelled());
    assert!(second.await.unwrap_err().is_cancelled());
}

#[gpui_kit::test]
fn prepare_search_preserves_previous_projection_and_skips_unrelated_delay_refresh(
    cx: &mut gpui_kit::TestAppContext,
) {
    use super::super::{ProxyCatalog, ProxyGroup, ProxyGroupBehavior, ProxyNode};
    use crate::test_support::proxies::open_catalog;

    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        let catalog = ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup::new("Proxy").with_behavior(ProxyGroupBehavior::Selector),
                ["Tokyo-01", "Tokyo-02", "London"]
                    .into_iter()
                    .map(|name| ProxyNode {
                        name: name.into(),
                        ..Default::default()
                    })
                    .collect(),
            )],
            3,
        );
        let (window, page, runtime) = open_catalog(cx, catalog);
        page.update(cx, |page, cx| {
            page.search_query = "tokyo".into();
            assert!(!page.should_refresh_search_on_delay());
            page.prepare_search(cx);
        });
        for _ in 0..200 {
            if cx.update(|cx| page.read(cx).search_projection.is_some()) {
                break;
            }
            runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        let search_guard = cx
            .update(|cx| page.read(cx).search_gate.clone())
            .try_lock_owned()
            .expect("search must be idle");
        page.update(cx, |page, cx| {
            page.sort_by_latency = true;
            assert!(page.should_refresh_search_on_delay());
            page.prepare_search(cx);
        });
        cx.update(|cx| {
            let state = page.read(cx);
            assert!(
                state.search_projection.is_some(),
                "re-running search must keep the previous search projection visible"
            );
            let catalog = state.catalog.as_ref().unwrap();
            assert_eq!(
                &*state.displayed_nodes(catalog, &catalog.groups()[0]),
                &[0, 1]
            );
        });
        drop(search_guard);
        page.update(cx, |page, cx| {
            page.search_query.clear();
            page.prepare_search(cx);
            assert!(page.search_projection.is_none());
            page.suspend();
        });
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
    });
}
