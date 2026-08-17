//! Tests for the cache service.

use cluster_sdk::grpc::stubs::cache as stubs;
use cluster_sdk::grpc::stubs::cache::cluster_cache_api_server::ClusterCacheApi as _;

use super::super::test_harness::{Harness, request};
use super::CacheService;

fn put(profile: &str, key: &str, value: &[u8]) -> stubs::PutRequest {
    stubs::PutRequest {
        profile: profile.to_owned(),
        key: key.to_owned(),
        value: value.to_vec(),
        ttl_ms: None,
        client_request_id: None,
    }
}

#[tokio::test]
async fn the_cache_service_serves_a_write_then_a_read() {
    let harness = Harness::wired(&["orders"]).await;
    let service = CacheService::new(harness.ctx.clone());

    service
        .put(request(put("orders", "ledger", b"41")))
        .await
        .expect("put succeeds");

    let response = service
        .get(request(stubs::GetRequest {
            profile: "orders".to_owned(),
            key: "ledger".to_owned(),
        }))
        .await
        .expect("get succeeds")
        .into_inner();

    let entry = response.entry.expect("the key was just written");
    assert_eq!(entry.value, b"41");
    assert_eq!(entry.version, 1);

    harness.stop().await;
}

#[tokio::test]
async fn a_missing_key_is_ok_with_no_entry_not_an_error() {
    let harness = Harness::wired(&["orders"]).await;
    let service = CacheService::new(harness.ctx.clone());

    let response = service
        .get(request(stubs::GetRequest {
            profile: "orders".to_owned(),
            key: "never-written".to_owned(),
        }))
        .await
        .expect("a miss is not an error")
        .into_inner();
    assert!(response.entry.is_none());

    harness.stop().await;
}

#[tokio::test]
async fn an_unknown_profile_is_the_not_found_mapped_profile_not_bound() {
    // One of `S1`'s exit criteria, on the service that carries the hot path.
    let harness = Harness::wired(&["orders"]).await;
    let service = CacheService::new(harness.ctx.clone());

    let status = service
        .get(request(stubs::GetRequest {
            profile: "not-a-profile".to_owned(),
            key: "ledger".to_owned(),
        }))
        .await
        .expect_err("an unbound profile is refused");

    assert_eq!(status.code(), tonic::Code::NotFound);
    assert!(
        status.message().contains("no backend bound"),
        "the message must be the frozen error model's own: {}",
        status.message()
    );

    harness.stop().await;
}

#[tokio::test]
async fn a_request_arriving_before_start_publishes_is_profile_not_bound() {
    // The `init` -> `start` window. The services are collected in the framework's
    // phase 6 and the backends exist only after phase 7 (DESIGN section 4.2), so
    // this window is unavoidable; answering it from the frozen error model is
    // what makes it harmless (invariant I3).
    let harness = Harness::unpublished().await;
    let service = CacheService::new(harness.ctx.clone());

    let status = service
        .get(request(stubs::GetRequest {
            profile: "orders".to_owned(),
            key: "ledger".to_owned(),
        }))
        .await
        .expect_err("nothing is bound yet");
    assert_eq!(status.code(), tonic::Code::NotFound);

    harness.stop().await;
}

#[tokio::test]
async fn cas_conflict_travels_as_aborted_and_reconstructs() {
    use cluster_sdk::{ClusterError, LeaseContext, to_cluster_error};
    use toolkit_canonical_errors::Problem;
    use toolkit_transport_grpc::extract_problem;

    let harness = Harness::wired(&["orders"]).await;
    let service = CacheService::new(harness.ctx.clone());

    service
        .put(request(put("orders", "counter", b"1")))
        .await
        .expect("put succeeds");

    let status = service
        .compare_and_swap(request(stubs::CasRequest {
            profile: "orders".to_owned(),
            key: "counter".to_owned(),
            expected_version: 99,
            new_value: b"2".to_vec(),
            ttl_ms: None,
        }))
        .await
        .expect_err("the expected version does not match");

    assert_eq!(status.code(), tonic::Code::Aborted);

    // And the caller gets the typed variant back, not a code it has to guess
    // from - which is what makes the CAS retry loop writable (DESIGN section 6.9).
    let problem: Problem = extract_problem(status.metadata())
        .expect("the trailer decodes")
        .expect("a cluster status carries the problem trailer");
    let decoded = to_cluster_error(problem, LeaseContext::None).expect("a typed error");
    assert!(
        matches!(decoded, ClusterError::CasConflict { ref key, .. } if key == "counter"),
        "expected CasConflict, got: {decoded:?}"
    );

    harness.stop().await;
}

#[tokio::test]
async fn scan_prefix_pages_and_caps_the_page_size() {
    let harness = Harness::wired(&["orders"]).await;
    let service = CacheService::new(harness.ctx.clone());

    for index in 0..5_u32 {
        service
            .put(request(put("orders", &format!("k/{index}"), b"v")))
            .await
            .expect("put succeeds");
    }

    let first = service
        .scan_prefix(request(stubs::ScanRequest {
            profile: "orders".to_owned(),
            prefix: "k/".to_owned(),
            page_size: Some(2),
            page_token: None,
        }))
        .await
        .expect("scan succeeds")
        .into_inner();
    assert_eq!(first.keys, vec!["k/0".to_owned(), "k/1".to_owned()]);
    assert_eq!(first.next_page_token.as_deref(), Some("k/1"));

    // The cursor is the last key returned, not an offset, so the next page starts
    // strictly after it.
    let second = service
        .scan_prefix(request(stubs::ScanRequest {
            profile: "orders".to_owned(),
            prefix: "k/".to_owned(),
            page_size: Some(2),
            page_token: first.next_page_token,
        }))
        .await
        .expect("scan succeeds")
        .into_inner();
    assert_eq!(second.keys, vec!["k/2".to_owned(), "k/3".to_owned()]);

    // The last page reports no cursor, which is what ends the client's loop.
    let last = service
        .scan_prefix(request(stubs::ScanRequest {
            profile: "orders".to_owned(),
            prefix: "k/".to_owned(),
            page_size: Some(1_000_000),
            page_token: second.next_page_token,
        }))
        .await
        .expect("scan succeeds")
        .into_inner();
    assert_eq!(last.keys, vec!["k/4".to_owned()]);
    assert!(last.next_page_token.is_none());

    harness.stop().await;
}

#[tokio::test]
async fn every_acknowledgement_carries_the_registry_generation() {
    // Section 5.6's staleness detector: a client learns the server's profile set
    // moved without waiting for its descriptor poll.
    let harness = Harness::wired(&["orders"]).await;
    let service = CacheService::new(harness.ctx.clone());

    let before = service
        .put(request(put("orders", "k", b"v")))
        .await
        .expect("put succeeds")
        .into_inner();
    assert_eq!(before.generation, harness.registry.generation());

    harness.registry.publish(
        harness
            .registry
            .snapshot()
            .profiles
            .values()
            .cloned()
            .collect(),
    );

    let after = service
        .put(request(put("orders", "k", b"v")))
        .await
        .expect("put succeeds")
        .into_inner();
    assert!(
        after.generation > before.generation,
        "a republished registry must be visible to a client that never polled"
    );

    harness.stop().await;
}

#[tokio::test]
async fn a_watch_delivers_the_key_events_that_follow_it() {
    use tokio_stream::StreamExt as _;

    let harness = Harness::wired(&["orders"]).await;
    let service = CacheService::new(harness.ctx.clone());

    let mut stream = service
        .watch(request(stubs::WatchRequest {
            profile: "orders".to_owned(),
            key: "ledger".to_owned(),
        }))
        .await
        .expect("the watch subscribes")
        .into_inner();

    service
        .put(request(put("orders", "ledger", b"41")))
        .await
        .expect("put succeeds");

    let event = stream
        .next()
        .await
        .expect("an event arrives")
        .expect("and it is not a transport error");
    assert_eq!(
        event.kind,
        i32::from(stubs::CacheWatchEventKind::Changed),
        "a write to the watched key is a Changed event"
    );
    assert_eq!(event.key.as_deref(), Some("ledger"));

    harness.stop().await;
}

#[tokio::test]
async fn audit_a_cancelled_cache_watch_stream_drops_the_backend_subscription() {
    // `WATCH-1`. Profile 1 frees a watch synchronously on `Drop`; Profile 3 must
    // agree (invariant I1). The resource under test is the *backend* half of the
    // subscription - the `CacheWatch` the pump owns - and the observable is the
    // paired sender noticing its receiver is gone.
    //
    // The key is quiet on purpose: with the pump parked on `watch.recv()` alone
    // there is no next send to fail, so nothing ever wakes it.
    use cluster_sdk::cache::CacheWatch;

    let (sender, watch) = CacheWatch::channel(8);
    let stream = super::watch_stream(watch);

    // What tonic does when the subscriber cancels: the response stream, and with
    // it the pump's outbound receiver, is dropped.
    drop(stream);

    let released = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !sender.is_closed() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;

    assert!(
        released.is_ok(),
        "a cancelled cache-watch stream must drop its backend `CacheWatch`; it is still held \
         (sender.is_closed() == false) 5 s after the subscriber went away"
    );
}

#[tokio::test]
async fn a_cancelled_watch_leaves_the_backend_free_to_prune_its_registration() {
    // The half of `WATCH-1` that is about the *registration*, not the task: both
    // plugins prune a watcher when a matching broadcast finds its channel closed
    // (`standalone-cluster-plugin/src/cache.rs` `broadcast`,
    // `postgres-cluster-plugin/src/cache/watch.rs` `deliver_to_key`). That prune
    // is reachable only if the sender reports `Closed`, which is what this
    // asserts - and it is exactly the state an in-process consumer's dropped
    // watch leaves behind, so the residue is the same in both profiles.
    use cluster_sdk::cache::{CacheEvent, CacheWatch, CacheWatchEvent, CacheWatchSender};

    // No event is ever published here: on a *busy* key the pre-existing
    // `TrySendError::Closed` arm already tore the pump down, so a test that
    // broadcasts proves nothing. The leak is the quiet key, and so is this.
    let (sender, watch) = CacheWatch::channel(8);
    let stream = super::watch_stream(watch);
    drop(stream);

    let pruned = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            // Precisely the call a plugin's fan-out makes, with precisely the
            // answer that puts the watcher on its dead list.
            let outcome = CacheWatchSender::try_send(
                &sender,
                CacheWatchEvent::Event(CacheEvent::Changed {
                    key: "ledger".to_owned(),
                }),
            );
            if matches!(
                outcome,
                Err(cluster_sdk::cache::CacheWatchTrySendError::Closed)
            ) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;

    assert!(
        pruned.is_ok(),
        "a backend broadcasting to a cancelled watch must see `Closed`, which is what prunes \
         the watcher registration"
    );
}

#[tokio::test]
async fn a_watch_stream_still_ends_when_the_backend_ends_first() {
    // The other exit from the same loop, so the added `tx.closed()` arm cannot
    // have swallowed it: a backend that drops its sender without a terminal event
    // is an end of stream, and it must still reach the subscriber as one.
    use cluster_sdk::cache::CacheWatch;
    use tokio_stream::StreamExt as _;

    let (sender, watch) = CacheWatch::channel(8);
    let mut stream = super::watch_stream(watch);
    drop(sender);

    let ended = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .expect("the pump must end promptly when the backend does");
    assert!(
        ended.is_none(),
        "a backend that ends without a terminal event ends the stream, not errors it"
    );
}
