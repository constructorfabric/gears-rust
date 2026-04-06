use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn resolve_called_once_returns_same_str() {
    let selector = GtsPluginSelector::new();
    let calls = Arc::new(AtomicUsize::new(0));

    let calls_a = calls.clone();
    let id_a = selector
        .get_or_init(|| async move {
            calls_a.fetch_add(1, Ordering::SeqCst);
            Ok::<_, std::convert::Infallible>(
                "gts.x.core.modkit.plugin.v1~x.core.test.plugin.v1~a.test._.plugin.v1".to_owned(),
            )
        })
        .await
        .unwrap();

    let calls_b = calls.clone();
    let id_b = selector
        .get_or_init(|| async move {
            calls_b.fetch_add(1, Ordering::SeqCst);
            Ok::<_, std::convert::Infallible>(
                "gts.x.core.modkit.plugin.v1~x.core.test.plugin.v1~b.test._.plugin.v1".to_owned(),
            )
        })
        .await
        .unwrap();

    assert_eq!(id_a, id_b);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn reset_triggers_reselection() {
    let selector = GtsPluginSelector::new();
    let calls = Arc::new(AtomicUsize::new(0));

    let calls_a = calls.clone();
    let id_a = selector
        .get_or_init(|| async move {
            calls_a.fetch_add(1, Ordering::SeqCst);
            Ok::<_, std::convert::Infallible>(
                "gts.x.core.modkit.plugin.v1~x.core.test.plugin.v1~a.test._.plugin.v1".to_owned(),
            )
        })
        .await;
    assert_eq!(
        &*id_a.unwrap(),
        "gts.x.core.modkit.plugin.v1~x.core.test.plugin.v1~a.test._.plugin.v1"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(selector.reset().await);

    let calls_b = calls.clone();
    let id_b = selector
        .get_or_init(|| async move {
            calls_b.fetch_add(1, Ordering::SeqCst);
            Ok::<_, std::convert::Infallible>(
                "gts.x.core.modkit.plugin.v1~x.core.test.plugin.v1~b.test._.plugin.v1".to_owned(),
            )
        })
        .await;
    assert_eq!(
        &*id_b.unwrap(),
        "gts.x.core.modkit.plugin.v1~x.core.test.plugin.v1~b.test._.plugin.v1"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn concurrent_get_or_init_resolves_once() {
    let selector = Arc::new(GtsPluginSelector::new());
    let calls = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..10 {
        let selector = Arc::clone(&selector);
        let calls = Arc::clone(&calls);
        handles.push(tokio::spawn(async move {
            selector
                .get_or_init(|| async {
                    // Small delay to increase chance of concurrent access
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, std::convert::Infallible>(
                        "gts.x.core.modkit.plugin.v1~x.core.test.plugin.v1~concurrent.test._.plugin.v1"
                            .to_owned(),
                    )
                })
                .await
        }));
    }

    // Await each handle in a loop (no futures_util dependency)
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap().unwrap());
    }

    // All results should be the same
    for id in &results {
        assert_eq!(
            &**id,
            "gts.x.core.modkit.plugin.v1~x.core.test.plugin.v1~concurrent.test._.plugin.v1"
        );
    }

    // Resolve should have been called exactly once
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
