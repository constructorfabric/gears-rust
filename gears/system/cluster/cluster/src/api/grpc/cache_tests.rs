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
    // from - which makes the CAS retry loop writable (DESIGN section 6.9).
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

    // The last page reports no cursor, which ends the client's loop.
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
    // is reachable only if the sender reports `Closed`, which this
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

// The reserved lease keyspace (B2)
//
// The cache RPC and the two cache-backed default coordination backends used to
// share one `Arc<dyn ClusterCacheBackend>` *and* one keyspace: lease records sat
// at plain `lock/`/`election/` keys, `LeaseRecord`'s layout is fixed and
// documented, and nothing checked the key. So `Cache.Get("lock/x")` returned a
// decodable lease, `Cache.Put` installed one held by nobody, and `Cache.Delete`
// reset the row so the fence restarted at 1 and a stale token matched again —
// mutual exclusion forgeable through the public cache API, by any caller that
// could reach it. The tests below are the two halves of the fix: the namespaces
// no longer alias, and the reserved one is not addressable.

mod reserved_lease_keyspace {
    use std::time::Duration;

    use cluster_sdk::LeaseRecord;
    use cluster_sdk::grpc::stubs::cache as stubs;
    use cluster_sdk::grpc::stubs::cache::cluster_cache_api_server::ClusterCacheApi as _;
    use cluster_sdk::grpc::stubs::lock as lock_stubs;
    use cluster_sdk::grpc::stubs::lock::distributed_lock_api_server::DistributedLockApi as _;
    use tokio_stream::StreamExt as _;

    use super::super::super::lock::DistributedLockService;
    use super::super::super::test_harness::{Harness, request};
    use super::super::CacheService;
    use super::put;

    /// The lock name every test here takes, chosen to collide with the consumer
    /// key written alongside it: the aliasing being ruled out is between
    /// `lock/<name>` and the consumer keyspace, so the name has to be one a
    /// consumer would plausibly also use.
    const NAME: &str = "ledger";

    fn try_lock(profile: &str, name: &str) -> lock_stubs::TryLockRequest {
        lock_stubs::TryLockRequest {
            profile: profile.to_owned(),
            name: name.to_owned(),
            ttl_ms: 30_000,
            client_request_id: None,
        }
    }

    fn get(profile: &str, key: &str) -> stubs::GetRequest {
        stubs::GetRequest {
            profile: profile.to_owned(),
            key: key.to_owned(),
        }
    }

    /// How long "and nothing else arrived" waits before it is believed. Paid
    /// once, by the passing path, so it is a wall-clock cost rather than a
    /// flakiness one: only a spurious event can turn it into a failure.
    const QUIET_PERIOD: Duration = Duration::from_millis(500);

    /// Bounds a `next()` so an event the pump wrongly dropped fails the test
    /// instead of hanging it — the pump delivers asynchronously, so a missing
    /// event is otherwise indistinguishable from a slow one.
    async fn next_event(stream: &mut super::super::CacheWatchStream) -> stubs::WireCacheWatchEvent {
        tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("an event within the timeout")
            .expect("the stream is still open")
            .expect("and the event is not a transport error")
    }

    /// **The direct regression test.** A lock held through the lock RPC must be
    /// invisible, unreadable and untouchable through the cache RPC — the whole
    /// of B2 in one assertion chain, stated as consequences (the forgeries fail)
    /// rather than as mechanism (a prefix is applied).
    #[tokio::test]
    async fn a_held_lock_is_neither_visible_nor_forgeable_through_the_cache_rpc() {
        let harness = Harness::wired(&["orders"]).await;
        let cache = CacheService::new(harness.ctx.clone());
        let lock = DistributedLockService::new(harness.ctx.clone());

        // A consumer key under the same name, so every "nothing there" below is
        // known to be a real answer from a live keyspace rather than an empty
        // store agreeing with anything.
        cache
            .put(request(put("orders", NAME, b"consumer-data")))
            .await
            .expect("a consumer writes its own key");

        let acquired = lock
            .try_lock(request(try_lock("orders", NAME)))
            .await
            .expect("the lock is free")
            .into_inner();
        let token = acquired.token.expect("an acquisition mints a token");

        // 1. Not readable, at either default's key or at the reserved prefix
        //    spelled without its sigil.
        for key in ["lock/ledger", "election/ledger", "lease/lock/ledger"] {
            let response = cache
                .get(request(get("orders", key)))
                .await
                .expect("a public key is a legal request whatever it names")
                .into_inner();
            assert!(
                response.entry.is_none(),
                "`{key}` must hold nothing: the lease lives in a keyspace this API does not serve"
            );
        }

        // 2. Not enumerable. `scan_prefix("")` is every key in the profile, so
        //    this is the assertion that the two namespaces do not alias — and
        //    nothing reachable through the cache decodes as a lease.
        let scanned = cache
            .scan_prefix(request(stubs::ScanRequest {
                profile: "orders".to_owned(),
                prefix: String::new(),
                page_size: None,
                page_token: None,
            }))
            .await
            .expect("scan succeeds")
            .into_inner();
        assert_eq!(
            scanned.keys,
            [NAME],
            "the consumer's key is the only thing in the cache keyspace"
        );
        for key in &scanned.keys {
            let entry = cache
                .get(request(get("orders", key)))
                .await
                .expect("get succeeds")
                .into_inner()
                .entry
                .expect("a key the scan just returned exists");
            assert!(
                LeaseRecord::decode(&entry.value).is_none(),
                "`{key}` decoded as a lease record: the cache API is still serving the lease \
                 keyspace"
            );
        }

        // 3. Not forgeable. The three writes that used to break exclusion —
        //    delete the row, overwrite it, re-create it — all miss the lease.
        let deleted = cache
            .delete(request(stubs::DeleteRequest {
                profile: "orders".to_owned(),
                key: "lock/ledger".to_owned(),
            }))
            .await
            .expect("delete succeeds")
            .into_inner();
        assert!(
            !deleted.existed,
            "deleting `lock/ledger` used to reset the lease and restart the fence at 1"
        );
        cache
            .put(request(put("orders", "lock/ledger", b"forged")))
            .await
            .expect("writing a public key succeeds; it simply is not the lease");

        // And the lock is *still* held, by its original holder, at its original
        // fence — which is what all of the above was protecting.
        let contended = lock
            .try_lock(request(try_lock("orders", NAME)))
            .await
            .expect_err("the lease is live and untouched");
        assert_eq!(contended.code(), tonic::Code::Aborted);
        lock.release(request(lock_stubs::LeaseRef {
            profile: "orders".to_owned(),
            token: Some(token),
            ttl_ms: None,
            client_request_id: None,
        }))
        .await
        .expect("the original token still matches: no forgery displaced it");

        harness.stop().await;
    }

    /// The boundary check, on **every** method — the reserved prefix is
    /// inexpressible under the public key rule, but this service hands `req.key`
    /// to the backend without running that validator, so on the wire the refusal
    /// has to be made here. Exhaustive on purpose: one unguarded method is the
    /// whole hole again.
    #[tokio::test]
    async fn every_cache_method_refuses_the_reserved_keyspace() {
        const RESERVED: &str = "$lease/lock/ledger";

        let harness = Harness::wired(&["orders"]).await;
        let cache = CacheService::new(harness.ctx.clone());
        let profile = "orders".to_owned();

        let refusals = [
            cache
                .get(request(get("orders", RESERVED)))
                .await
                .err()
                .map(|status| ("get", status)),
            cache
                .put(request(put("orders", RESERVED, b"forged")))
                .await
                .err()
                .map(|status| ("put", status)),
            cache
                .put_if_absent(request(put("orders", RESERVED, b"forged")))
                .await
                .err()
                .map(|status| ("put_if_absent", status)),
            cache
                .compare_and_swap(request(stubs::CasRequest {
                    profile: profile.clone(),
                    key: RESERVED.to_owned(),
                    expected_version: 1,
                    new_value: b"forged".to_vec(),
                    ttl_ms: None,
                }))
                .await
                .err()
                .map(|status| ("compare_and_swap", status)),
            cache
                .compare_and_delete(request(stubs::CadRequest {
                    profile: profile.clone(),
                    key: RESERVED.to_owned(),
                    expected_value: b"forged".to_vec(),
                }))
                .await
                .err()
                .map(|status| ("compare_and_delete", status)),
            cache
                .delete(request(stubs::DeleteRequest {
                    profile: profile.clone(),
                    key: RESERVED.to_owned(),
                }))
                .await
                .err()
                .map(|status| ("delete", status)),
            cache
                .contains(request(stubs::ContainsRequest {
                    profile: profile.clone(),
                    key: RESERVED.to_owned(),
                }))
                .await
                .err()
                .map(|status| ("contains", status)),
            cache
                .scan_prefix(request(stubs::ScanRequest {
                    profile: profile.clone(),
                    prefix: "$lease/".to_owned(),
                    page_size: None,
                    page_token: None,
                }))
                .await
                .err()
                .map(|status| ("scan_prefix", status)),
            cache
                .watch(request(stubs::WatchRequest {
                    profile: profile.clone(),
                    key: RESERVED.to_owned(),
                }))
                .await
                .err()
                .map(|status| ("watch", status)),
            cache
                .watch_prefix(request(stubs::WatchPrefixRequest {
                    profile: profile.clone(),
                    prefix: "$lease/".to_owned(),
                }))
                .await
                .err()
                .map(|status| ("watch_prefix", status)),
        ];

        assert_eq!(
            refusals.len(),
            10,
            "every method on the service is covered; add the new one here too"
        );
        for (method, status) in refusals.into_iter().map(|refusal| {
            refusal.expect("a reserved key must be refused, not served or silently accepted")
        }) {
            assert_eq!(
                status.code(),
                tonic::Code::InvalidArgument,
                "`{method}` refused with the wrong code"
            );
            assert!(
                status.message().contains("reserved keyspace"),
                "`{method}` must say why: {}",
                status.message()
            );
        }

        harness.stop().await;
    }

    /// **The watch pump's half of the boundary**, and the one no other test
    /// here reaches. `watch_prefix("")` is a perfectly public subscription —
    /// `reject_reserved` has nothing to refuse — but physically it spans the
    /// whole store, so the lease backends' own writes arrive at the pump. The
    /// filter that drops them there is all that stands between a caller with
    /// cache access and a live feed of every lock and election mutation in the
    /// profile: not the values, but the names and the timing, and without ever
    /// naming a reserved key.
    ///
    /// **Ordering is what makes this an assertion rather than a wait for an
    /// absence.** The standalone backend fans out synchronously inside the call
    /// that mutates, so each lease write below is already queued at the pump
    /// *before* the consumer write that follows it. A leak therefore arrives
    /// first, and is caught by asserting what the first event is — no timing
    /// window, no sleep. (The trailing quiet check is belt-and-braces for
    /// anything the two lock calls write that this ordering does not cover.)
    #[tokio::test]
    async fn a_public_prefix_watch_never_delivers_a_lease_event() {
        let harness = Harness::wired(&["orders"]).await;
        let cache = CacheService::new(harness.ctx.clone());
        let lock = DistributedLockService::new(harness.ctx.clone());

        let mut stream = cache
            .watch_prefix(request(stubs::WatchPrefixRequest {
                profile: "orders".to_owned(),
                prefix: String::new(),
            }))
            .await
            .expect("`\"\"` is a legal public prefix: every key in the profile")
            .into_inner();

        // A lease *write*, then the consumer write that must be delivered first.
        let acquired = lock
            .try_lock(request(try_lock("orders", NAME)))
            .await
            .expect("the lock is free")
            .into_inner();
        let token = acquired.token.expect("an acquisition mints a token");
        cache
            .put(request(put("orders", NAME, b"consumer-data")))
            .await
            .expect("put succeeds");

        // A lease *delete*, then the consumer delete behind it — the other event
        // kind the lease keyspace produces, and the one a filter keyed on
        // `Changed` alone would miss.
        lock.release(request(lock_stubs::LeaseRef {
            profile: "orders".to_owned(),
            token: Some(token),
            ttl_ms: None,
            client_request_id: None,
        }))
        .await
        .expect("the holder releases its own lease");
        cache
            .delete(request(stubs::DeleteRequest {
                profile: "orders".to_owned(),
                key: NAME.to_owned(),
            }))
            .await
            .expect("delete succeeds");

        let first = next_event(&mut stream).await;
        assert_eq!(
            (first.kind, first.key.as_deref()),
            (i32::from(stubs::CacheWatchEventKind::Changed), Some(NAME)),
            "the acquisition's lease write reached a cache subscriber ahead of the consumer's \
             own write"
        );
        let second = next_event(&mut stream).await;
        assert_eq!(
            (second.kind, second.key.as_deref()),
            (i32::from(stubs::CacheWatchEventKind::Deleted), Some(NAME)),
            "the release's lease delete reached a cache subscriber ahead of the consumer's own \
             delete"
        );

        // ...and nothing else at all. Whatever else the two lock calls touched in
        // the reserved keyspace, none of it is owed to a cache subscriber.
        let extra = tokio::time::timeout(QUIET_PERIOD, stream.next()).await;
        assert!(
            extra.is_err(),
            "a public watch was delivered an event it is not owed: {extra:?}"
        );

        harness.stop().await;
    }

    /// The complement, and the reason the reserved check tests a leading sigil
    /// rather than a substring: a key that merely *resembles* the reserved
    /// keyspace — the same path spelled without the leading `$lease/` sigil — is
    /// ordinary consumer data and must be served normally. A guard that
    /// over-reached here would break every consumer whose key looks like the
    /// reserved one.
    #[tokio::test]
    async fn a_neighbouring_key_is_served_normally() {
        let harness = Harness::wired(&["orders"]).await;
        let cache = CacheService::new(harness.ctx.clone());

        for key in ["lease/lock/ledger", "lock/ledger", "ledger"] {
            cache
                .put(request(put("orders", key, b"consumer-data")))
                .await
                .unwrap_or_else(|status| panic!("`{key}` is a consumer key: {status}"));
            let entry = cache
                .get(request(get("orders", key)))
                .await
                .unwrap_or_else(|status| panic!("`{key}` is a consumer key: {status}"))
                .into_inner()
                .entry
                .unwrap_or_else(|| panic!("`{key}` was just written"));
            assert_eq!(entry.value, b"consumer-data");
        }

        // But a key that *carries* the sigil anywhere — `ledger$`, once accepted
        // here because the wire ran no key validator — is now refused at the
        // boundary (H8): `$` is outside `CACHE_KEY_RULE`, so the facade rejected
        // it in Profile 1 all along, and the wire rejects it too rather than
        // diverging (invariant I1). This is the same reason the reserved sigil is
        // *inexpressible* under the public key rule (B2).
        let status = cache
            .put(request(put("orders", "ledger$", b"consumer-data")))
            .await
            .expect_err("a `$`-bearing key is not a legal cache key on the wire");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);

        harness.stop().await;
    }
}

/// B3 — the terminal `Closed` under back-pressure.
///
/// These drive [`watch_stream`](super::watch_stream) directly over a
/// [`CacheWatch`] channel rather than through the service, because the property
/// is about the pump's buffer accounting and a service-level test cannot fill a
/// 256-slot buffer without also testing the backend's fan-out.
mod terminal_close_under_backpressure {
    use std::time::Duration;

    use cluster_sdk::ClusterError;
    use cluster_sdk::cache::{CacheEvent, CacheWatch, CacheWatchEvent, CacheWatchSender};
    use cluster_sdk::grpc::stubs::cache as stubs;
    use tokio_stream::StreamExt as _;

    use super::super::{WATCH_STREAM_BUFFER, watch_stream};

    /// More events than the stream buffer holds, so the pump is guaranteed to
    /// have started dropping before the terminal event arrives.
    const OVERFLOW: usize = WATCH_STREAM_BUFFER + 44;

    /// Waits until the pump has finished — it drops the [`CacheWatch`] receiver
    /// when it returns, which is what closes `sender`.
    ///
    /// A deterministic signal rather than a sleep: the assertions below are about
    /// what the pump did with a *full* buffer, so the test must not start reading
    /// (and so freeing room) until the pump has already made its decisions.
    async fn wait_for_the_pump_to_finish(sender: &CacheWatchSender) {
        let ended = tokio::time::timeout(Duration::from_secs(5), async {
            while !sender.is_closed() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(
            ended.is_ok(),
            "the pump must end after a terminal event; it is still running 5 s later"
        );
    }

    /// Fills the pump's outbound buffer past capacity, then sends
    /// `Closed(Shutdown)`, and returns everything the subscriber saw.
    async fn events_after_a_close_on_a_full_buffer() -> Vec<stubs::WireCacheWatchEvent> {
        // Small, so the pump is the thing under back-pressure rather than this
        // channel: the pump drains it eagerly whatever the subscriber does.
        let (sender, watch) = CacheWatch::channel(8);
        let mut stream = watch_stream(watch);

        for index in 0..OVERFLOW {
            let event = CacheWatchEvent::Event(CacheEvent::Changed {
                key: format!("ledger-{index}"),
            });
            assert!(
                sender.send(event).await.is_ok(),
                "the pump drains this channel regardless of the subscriber"
            );
        }
        assert!(
            sender
                .send(CacheWatchEvent::Closed(ClusterError::Shutdown))
                .await
                .is_ok(),
            "the backend's terminal event reaches the pump"
        );
        wait_for_the_pump_to_finish(&sender).await;

        // Only now does the subscriber read, so every send above was made against
        // a buffer this test knows the state of.
        let mut seen = Vec::new();
        while let Ok(Some(frame)) =
            tokio::time::timeout(Duration::from_secs(5), stream.next()).await
        {
            seen.push(frame.expect("no frame is a transport error"));
        }
        seen
    }

    /// The regression test. A subscriber that stopped draining still observes the
    /// close, and the stream ends after it — where before the terminal event was
    /// dropped by the `Full` arm's `continue` and the subscriber saw only an end
    /// of stream, which the SDK's `RestartingWatch` reconnects on.
    #[tokio::test]
    async fn a_full_watch_stream_still_delivers_its_terminal_close() {
        let seen = events_after_a_close_on_a_full_buffer().await;

        let last = seen.last().expect("the subscriber saw something");
        assert_eq!(
            last.kind,
            i32::from(stubs::CacheWatchEventKind::Closed),
            "a subscriber that fell behind must still be told the stream closed, not left to \
             infer an end of stream: got {:?}",
            seen.iter().map(|event| event.kind).collect::<Vec<_>>()
        );
        // The loop above ran until `next()` yielded `None`, so the stream ending
        // is what got us here; asserting the close is *last* is what says the
        // ending followed it rather than replaced it.
        assert_eq!(
            seen.iter()
                .filter(|event| event.kind == i32::from(stubs::CacheWatchEventKind::Closed))
                .count(),
            1,
            "exactly one terminal event, and it is the end of the stream"
        );
    }

    /// The reservation is made at stream *open*, not on first use.
    ///
    /// Asserted through its only observable consequence: with the slot taken
    /// before the loop starts, exactly [`WATCH_STREAM_BUFFER`] ordinary events
    /// can be in flight ahead of the close. A pump that reserved lazily would fit
    /// one more ordinary event and then find no capacity for the reservation —
    /// failing precisely under the back-pressure the reservation exists to
    /// survive.
    #[tokio::test]
    async fn the_terminal_slot_is_reserved_before_any_event_can_fill_the_buffer() {
        let seen = events_after_a_close_on_a_full_buffer().await;

        let ordinary = seen
            .iter()
            .filter(|event| event.kind == i32::from(stubs::CacheWatchEventKind::Changed))
            .count();
        assert_eq!(
            ordinary,
            WATCH_STREAM_BUFFER,
            "the consumer-visible buffer must stay at the documented size, with the extra slot \
             held for the close: saw {ordinary} ordinary events out of {} total",
            seen.len()
        );
        assert_eq!(
            seen.len(),
            WATCH_STREAM_BUFFER + 1,
            "and nothing else got through: the {} dropped events are owed as a `Lagged` the full \
             buffer had no room for, which is the documented behaviour",
            OVERFLOW - WATCH_STREAM_BUFFER
        );
    }
}

// H8: cache keys validated at the wire boundary

/// The service boundary now runs the same key rule the in-process facade does,
/// because on the wire the server cannot trust the client to have run it (H8).
mod boundary_validation {
    use super::super::CacheService;
    use super::*;

    fn get(profile: &str, key: &str) -> stubs::GetRequest {
        stubs::GetRequest {
            profile: profile.to_owned(),
            key: key.to_owned(),
        }
    }

    /// H8 verify (1): an exact-key method rejects a key the facade rejects, with
    /// the contract's `InvalidName` (mapped to `InvalidArgument`), on every
    /// key-bearing method — one unguarded method is the whole hole again.
    #[tokio::test]
    async fn an_invalid_key_is_refused_on_every_exact_key_method() {
        // A space is outside `CACHE_KEY_RULE`; `validate_cache_key` rejects it.
        const BAD: &str = "bad key";

        let harness = Harness::wired(&["orders"]).await;
        let cache = CacheService::new(harness.ctx.clone());
        let profile = "orders".to_owned();

        let refusals = [
            ("get", cache.get(request(get("orders", BAD))).await.err()),
            (
                "put",
                cache.put(request(put("orders", BAD, b"v"))).await.err(),
            ),
            (
                "put_if_absent",
                cache
                    .put_if_absent(request(put("orders", BAD, b"v")))
                    .await
                    .err(),
            ),
            (
                "compare_and_swap",
                cache
                    .compare_and_swap(request(stubs::CasRequest {
                        profile: profile.clone(),
                        key: BAD.to_owned(),
                        expected_version: 1,
                        new_value: b"v".to_vec(),
                        ttl_ms: None,
                    }))
                    .await
                    .err(),
            ),
            (
                "compare_and_delete",
                cache
                    .compare_and_delete(request(stubs::CadRequest {
                        profile: profile.clone(),
                        key: BAD.to_owned(),
                        expected_value: b"v".to_vec(),
                    }))
                    .await
                    .err(),
            ),
            (
                "delete",
                cache
                    .delete(request(stubs::DeleteRequest {
                        profile: profile.clone(),
                        key: BAD.to_owned(),
                    }))
                    .await
                    .err(),
            ),
            (
                "contains",
                cache
                    .contains(request(stubs::ContainsRequest {
                        profile: profile.clone(),
                        key: BAD.to_owned(),
                    }))
                    .await
                    .err(),
            ),
            (
                "watch",
                cache
                    .watch(request(stubs::WatchRequest {
                        profile: profile.clone(),
                        key: BAD.to_owned(),
                    }))
                    .await
                    .err(),
            ),
        ];

        for (method, refusal) in refusals {
            let status = refusal.unwrap_or_else(|| panic!("`{method}` accepted an invalid key"));
            assert_eq!(
                status.code(),
                tonic::Code::InvalidArgument,
                "`{method}` refused with the wrong code"
            );
            assert!(
                matches!(
                    cluster_sdk::convert::from_status(&status),
                    cluster_sdk::ClusterError::InvalidName { .. }
                ),
                "`{method}` must reconstruct as `InvalidName`, not a provider error"
            );
        }

        harness.stop().await;
    }

    /// H8 verify (2), invariant I1: the same key yields the same error variant in
    /// Profile 1 (the facade's `validate_cache_key`) and Profile 3 (the wire,
    /// reconstructed through the client's own `from_status`).
    #[tokio::test]
    async fn the_wire_and_the_facade_reject_a_bad_key_alike() {
        const BAD: &str = "bad key";

        let harness = Harness::wired(&["orders"]).await;
        let cache = CacheService::new(harness.ctx.clone());

        let profile_1 =
            cluster_sdk::validate_cache_key(BAD).expect_err("the facade rejects this key");
        assert!(matches!(
            profile_1,
            cluster_sdk::ClusterError::InvalidName { .. }
        ));

        let wire = cache
            .get(request(get("orders", BAD)))
            .await
            .expect_err("the wire rejects it too");
        let profile_3 = cluster_sdk::convert::from_status(&wire);

        assert_eq!(
            std::mem::discriminant(&profile_1),
            std::mem::discriminant(&profile_3),
            "Profile 1 and Profile 3 must agree on the error variant (I1)"
        );

        harness.stop().await;
    }

    /// H8 verify (3): `""` stays legal on the **prefix** methods — the trap B2's
    /// F2 fix hit once. An exact-key method rejects it (empty is not a valid
    /// key), but `scan_prefix`/`watch_prefix` treat `""` as "everything in my
    /// scope", so validating them with the key rule would be wrong.
    #[tokio::test]
    async fn an_empty_string_is_a_legal_prefix_but_not_a_legal_key() {
        let harness = Harness::wired(&["orders"]).await;
        let cache = CacheService::new(harness.ctx.clone());

        // Prefix methods: `""` is legal, exactly as before.
        cache
            .scan_prefix(request(stubs::ScanRequest {
                profile: "orders".to_owned(),
                prefix: String::new(),
                page_size: None,
                page_token: None,
            }))
            .await
            .expect("`\"\"` is a legal public prefix for scan");
        cache
            .watch_prefix(request(stubs::WatchPrefixRequest {
                profile: "orders".to_owned(),
                prefix: String::new(),
            }))
            .await
            .expect("`\"\"` is a legal public prefix for watch");

        // Exact-key method: `""` is not a valid key, and now says so.
        let status = cache
            .get(request(get("orders", "")))
            .await
            .expect_err("an empty exact key is invalid");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);

        harness.stop().await;
    }
}
