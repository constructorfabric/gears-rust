//! Scope slots: exact identity, capacity, eviction and slot-scoped invalidation.

use super::*;

fn set(scopes: &[&str]) -> ScopeSet {
    ScopeSet::new(scopes).unwrap()
}

/// `dynamic` slots besides the always-present default one.
fn with_dynamic_slots(dynamic: usize) -> ClientCredentialsProviderConfig {
    plain().with_max_entries(dynamic + 1)
}

fn spawn_get_with(
    provider: &ClientCredentialsProvider,
    scopes: &[&str],
) -> JoinHandle<ProviderOutcome> {
    let provider = provider.clone();
    let scopes = set(scopes);
    tokio::spawn(async move { provider.get_with_scopes(&scopes).await })
}

async fn get_with(provider: &ClientCredentialsProvider, scopes: &[&str]) -> ProviderOutcome {
    provider.get_with_scopes(&set(scopes)).await
}

fn is_capacity_error(outcome: &ProviderOutcome) -> bool {
    matches!(
        outcome,
        Err(ClientCredentialsProviderError::CapacityExhausted { .. })
    )
}

fn dynamic_slots(provider: &ClientCredentialsProvider) -> usize {
    provider.handle.registry.lock().slots.len()
}

// --- identity ------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn explicit_scopes_equal_to_the_configured_set_share_its_slot() {
    let (provider, fake) = setup_scoped(plain(), Some(&["b", "a"]), vec![cached(100)]);

    let default = provider.get().await.unwrap();
    let explicit = get_with(&provider, &["a", "b", "a"]).await.unwrap();

    assert_eq!(serial(&explicit), serial(&default));
    assert_eq!(explicit.origin(), default.origin());
    assert_eq!(fake.calls(), 1);
    assert_eq!(fake.scopes(), [Some(set(&["a", "b"]))]);
    assert_eq!(dynamic_slots(&provider), 0, "no dynamic slot was used");
}

#[tokio::test(start_paused = true)]
async fn the_configured_slot_shares_one_flight_with_explicit_callers() {
    let (release, gated) = gate();
    let (provider, fake) = setup_scoped(plain(), Some(&["a"]), vec![gated]);

    let default = spawn_get(&provider);
    let explicit = spawn_get_with(&provider, &["a"]);
    settle().await;
    release.send(cached(100)).ok().unwrap();

    let default = default.await.unwrap().unwrap();
    let explicit = explicit.await.unwrap().unwrap();
    assert_eq!(serial(&default), serial(&explicit));
    assert_eq!(fake.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn each_exact_set_has_its_own_cache_and_requests_exactly_its_scopes() {
    let script = vec![cached(100), cached(100), cached(100)];
    let (provider, fake) = setup_scoped(plain(), None, script);

    let default = provider.get().await.unwrap();
    let read = get_with(&provider, &["read"]).await.unwrap();
    let both = get_with(&provider, &["write", "read"]).await.unwrap();
    assert_eq!(
        serial(&get_with(&provider, &["read"]).await.unwrap()),
        serial(&read)
    );
    assert_eq!(serial(&provider.get().await.unwrap()), serial(&default));

    assert_eq!(fake.calls(), 3, "no subset or superset reuse");
    assert_eq!(
        fake.scopes(),
        [None, Some(set(&["read"])), Some(set(&["read", "write"]))]
    );
    assert_ne!(serial(&both), serial(&read));
    let tags = [&default, &read, &both].map(AuthenticationResult::origin);
    assert!(tags[0] != tags[1] && tags[1] != tags[2] && tags[0] != tags[2]);
}

#[tokio::test(start_paused = true)]
async fn the_configured_slot_starts_with_allow_cached_and_new_dynamic_slots_with_refresh() {
    let (provider, fake) = setup_scoped(plain(), None, vec![cached(100), cached(100)]);

    provider.get().await.unwrap();
    get_with(&provider, &["a"]).await.unwrap();

    assert_eq!(
        fake.requests(),
        [RequestCacheControl::AllowCached, refresh_if(None)]
    );
}

// --- capacity and eviction ----------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_single_entry_allows_only_the_default_scopes() {
    let (provider, fake) = setup_scoped(with_dynamic_slots(0), Some(&["a"]), vec![cached(100)]);

    assert!(matches!(
        get_with(&provider, &["b"]).await,
        Err(ClientCredentialsProviderError::CapacityExhausted { max_entries: 1 })
    ));
    provider.get().await.unwrap();
    get_with(&provider, &["a"]).await.unwrap();
    assert_eq!(fake.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn eviction_prefers_slots_without_a_usable_entry_then_the_least_recently_used() {
    let script = vec![
        cached(100),
        no_store(),
        cached(100),
        cached(100),
        cached(100),
    ];
    let (provider, fake) = setup_scoped(with_dynamic_slots(2), None, script);

    get_with(&provider, &["a"]).await.unwrap(); // serial 1, cached
    get_with(&provider, &["b"]).await.unwrap(); // serial 2, nothing to keep
    get_with(&provider, &["c"]).await.unwrap(); // evicts b although a is older
    assert_eq!(serial(&get_with(&provider, &["a"]).await.unwrap()), 1);
    assert_eq!(fake.calls(), 3);

    // Both usable now; c was admitted before a's last use.
    get_with(&provider, &["d"]).await.unwrap(); // serial 4, evicts c
    assert_eq!(serial(&get_with(&provider, &["a"]).await.unwrap()), 1);
    assert_eq!(serial(&get_with(&provider, &["c"]).await.unwrap()), 5);
    assert_eq!(dynamic_slots(&provider), 2);
}

#[tokio::test(start_paused = true)]
async fn a_pinned_slot_is_not_evicted_even_without_a_flight() {
    let (provider, fake) =
        setup_scoped(with_dynamic_slots(1), None, vec![cached(100), cached(100)]);
    get_with(&provider, &["a"]).await.unwrap();

    // A caller admitted to `a` that has not polled the slot yet.
    let lease = provider.handle.admit(&set(&["a"])).unwrap();
    assert!(is_capacity_error(&get_with(&provider, &["b"]).await));

    drop(lease);
    get_with(&provider, &["b"]).await.unwrap();
    assert_eq!(fake.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_waiting_caller_pins_its_slot() {
    let (release, gated) = gate();
    let (provider, _fake) = setup_scoped(with_dynamic_slots(1), None, vec![gated, cached(100)]);

    let waiter = spawn_get_with(&provider, &["a"]);
    settle().await;
    assert!(is_capacity_error(&get_with(&provider, &["b"]).await));

    release.send(cached(100)).ok().unwrap();
    assert_eq!(serial(&waiter.await.unwrap().unwrap()), 1);
    assert_eq!(serial(&get_with(&provider, &["b"]).await.unwrap()), 2);
}

#[tokio::test(start_paused = true)]
async fn a_background_refresh_pins_its_slot_after_the_caller_returned() {
    let config = with_dynamic_slots(1);
    let (release, gated) = gate();
    let (provider, fake) = setup_scoped(config, None, vec![cached(100), gated, cached(100)]);

    get_with(&provider, &["a"]).await.unwrap();
    advance(80).await;
    assert_eq!(serial(&get_with(&provider, &["a"]).await.unwrap()), 1);
    settle().await;
    assert_eq!(fake.live(), 1, "refresh running without callers");
    assert!(is_capacity_error(&get_with(&provider, &["b"]).await));

    release.send(cached(100)).ok().unwrap();
    settle().await;
    assert_eq!(serial(&get_with(&provider, &["b"]).await.unwrap()), 3);
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_task_pins_its_slot_until_it_has_ended() {
    let (provider, fake) =
        setup_scoped(with_dynamic_slots(1), None, vec![Script::Hang, cached(100)]);

    let waiter = spawn_get_with(&provider, &["a"]);
    settle().await;
    waiter.abort();
    settle().await;
    assert_eq!(fake.live(), 1, "the flight outlives its caller");
    assert!(is_capacity_error(&get_with(&provider, &["b"]).await));

    provider.clear(); // abort requested, the task has not observed it yet
    assert!(is_capacity_error(&get_with(&provider, &["b"]).await));

    settle().await;
    assert_eq!(fake.live(), 0);
    get_with(&provider, &["b"]).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_slot_in_backoff_is_pinned_until_retry_at_and_capacity_errors_leave_it_untouched() {
    let config =
        with_dynamic_slots(1).with_backoff(Duration::from_secs(10), Duration::from_secs(10));
    let script = vec![Script::Err(rejected()), cached(100)];
    let (provider, fake) = setup_scoped(config, None, script);

    assert!(get_with(&provider, &["a"]).await.is_err());
    assert!(is_capacity_error(&get_with(&provider, &["b"]).await));
    assert!(
        matches!(
            get_with(&provider, &["a"]).await,
            Err(ClientCredentialsProviderError::Resolver(
                AuthNResolverError::TokenAcquisitionFailed(_)
            ))
        ),
        "backoff still served from the same slot"
    );
    assert_eq!(fake.calls(), 1);

    advance(10).await;
    assert_eq!(serial(&get_with(&provider, &["b"]).await.unwrap()), 1);
    assert_eq!(fake.calls(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_admissions_never_exceed_capacity() {
    const CALLERS: usize = 16;
    let script = (0..CALLERS).map(|_| Script::Hang).collect();
    let (provider, fake) = setup_with_scopes(
        with_dynamic_slots(4),
        None,
        script,
        wall_clock(),
        no_jitter(),
    );

    let start = Arc::new(tokio::sync::Barrier::new(CALLERS));
    let admitted = Arc::new(tokio::sync::Barrier::new(CALLERS + 1));
    let tasks: Vec<_> = (0..CALLERS)
        .map(|i| {
            let provider = provider.clone();
            let (start, admitted) = (Arc::clone(&start), Arc::clone(&admitted));
            let scopes = set(&[format!("s{i}").as_str()]);
            tokio::spawn(async move {
                start.wait().await;
                // Admission happens in the first poll.
                let mut get = Box::pin(provider.get_with_scopes(&scopes));
                let first = futures_poll_once(get.as_mut()).await;
                admitted.wait().await;
                match first {
                    std::task::Poll::Ready(outcome) => Some(outcome),
                    std::task::Poll::Pending => {
                        drop(get.await); // ends with shutdown
                        None
                    }
                }
            })
        })
        .collect();

    admitted.wait().await;
    assert_eq!(dynamic_slots(&provider), 4);
    provider.shutdown().await;

    let mut rejected = 0;
    for task in tasks {
        if let Some(outcome) = task.await.unwrap() {
            assert!(is_capacity_error(&outcome), "{outcome:?}");
            rejected += 1;
        }
    }
    assert_eq!(rejected, CALLERS - 4);
    // Shutdown may cancel a reserved acquisition before its task first runs.
    assert!(fake.calls() <= 4);
}

#[tokio::test(start_paused = true)]
async fn saturation_reports_the_configured_total_capacity() {
    let (provider, _fake) = setup_scoped(with_dynamic_slots(1), None, vec![Script::Hang]);

    let _waiter = spawn_get_with(&provider, &["a"]);
    settle().await;
    let error = get_with(&provider, &["b"]).await.unwrap_err();

    assert!(
        matches!(
            error,
            ClientCredentialsProviderError::CapacityExhausted { max_entries: 2 }
        ),
        "{error:?}"
    );
    assert!(error.to_string().contains('2'), "{error}");
}

#[tokio::test(start_paused = true)]
async fn acquisition_failures_keep_the_resolver_error_as_their_source() {
    let (provider, _fake) = setup_scoped(plain(), None, vec![Script::Err(rejected())]);

    let error = get_with(&provider, &["a"]).await.unwrap_err();
    let ClientCredentialsProviderError::Resolver(resolver) = &error else {
        panic!("expected a resolver failure, got {error:?}");
    };
    assert!(matches!(
        resolver,
        AuthNResolverError::TokenAcquisitionFailed(_)
    ));
    let source = std::error::Error::source(&error).expect("source kept");
    assert_eq!(source.to_string(), resolver.to_string());
}

// --- invalidation across slots --------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_conditional_clear_affects_only_the_slot_that_issued_the_tag() {
    let script = vec![cached(100), cached(100), cached(100), cached(100)];
    let (provider, fake) = setup_scoped(plain(), None, script);

    let default = provider.get().await.unwrap();
    let a = get_with(&provider, &["a"]).await.unwrap();
    let b = get_with(&provider, &["b"]).await.unwrap();

    assert!(provider.invalidate(&a));
    assert_eq!(serial(&provider.get().await.unwrap()), serial(&default));
    assert_eq!(
        serial(&get_with(&provider, &["b"]).await.unwrap()),
        serial(&b)
    );
    assert_eq!(fake.calls(), 3);

    assert_eq!(serial(&get_with(&provider, &["a"]).await.unwrap()), 4);
    assert!(!provider.invalidate(&a));
}

#[tokio::test(start_paused = true)]
async fn a_tag_from_an_evicted_slot_does_not_clear_its_recreation() {
    let script = vec![cached(100), cached(100), cached(100)];
    let (provider, fake) = setup_scoped(with_dynamic_slots(1), None, script);

    let old = get_with(&provider, &["a"]).await.unwrap();
    get_with(&provider, &["b"]).await.unwrap(); // evicts a
    let recreated = get_with(&provider, &["a"]).await.unwrap(); // evicts b

    assert_ne!(old.origin(), recreated.origin());
    assert!(!provider.invalidate(&old));
    assert_eq!(serial(&get_with(&provider, &["a"]).await.unwrap()), 3);
    assert_eq!(fake.calls(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_rejected_token_is_not_resurrected_by_eviction_and_recreation() {
    let plugin = || Script::PluginCache {
        cache_for: 100,
        tag: "p",
    };
    let script = vec![plugin(), plugin(), plugin()];
    let (provider, fake) = setup_scoped(with_dynamic_slots(1), None, script);

    let rejected = get_with(&provider, &["a"]).await.unwrap();
    assert!(provider.invalidate(&rejected));
    get_with(&provider, &["b"]).await.unwrap(); // evicts the cleared slot

    let fresh = get_with(&provider, &["a"]).await.unwrap();
    assert_ne!(serial(&fresh), serial(&rejected), "plugin cache bypassed");
    assert_eq!(fake.requests()[2], refresh_if(None));
}

#[tokio::test(start_paused = true)]
async fn a_global_clear_survives_eviction_and_recreation() {
    let plugin = || Script::PluginCache {
        cache_for: 100,
        tag: "p",
    };
    let script = vec![plugin(), plugin(), plugin()];
    let (provider, fake) = setup_scoped(with_dynamic_slots(1), None, script);

    let rejected = get_with(&provider, &["a"]).await.unwrap();
    provider.clear();
    get_with(&provider, &["b"]).await.unwrap();

    let fresh = get_with(&provider, &["a"]).await.unwrap();
    assert_eq!(serial(&fresh), 3, "neither a's rejected nor b's result");
    assert_ne!(serial(&fresh), serial(&rejected));
    assert_eq!(fake.scopes()[2], Some(set(&["a"])));
    assert_eq!(fake.requests()[2], refresh_if(None));
}

#[tokio::test(start_paused = true)]
async fn clear_covers_every_slot_and_requests_refresh() {
    let script = (0..6).map(|_| cached_tagged(100, "t")).collect();
    let (provider, fake) = setup_scoped(plain(), None, script);

    provider.get().await.unwrap();
    get_with(&provider, &["a"]).await.unwrap();
    get_with(&provider, &["b"]).await.unwrap();
    provider.clear();
    provider.get().await.unwrap();
    get_with(&provider, &["a"]).await.unwrap();
    get_with(&provider, &["b"]).await.unwrap();

    assert_eq!(fake.calls(), 6);
    assert!(
        fake.requests()[3..]
            .iter()
            .all(|r| *r == refresh_if(Some("t")))
    );
}

// --- lifecycle -------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn shutdown_closes_admission_and_waits_for_dynamic_tasks() {
    let (provider, fake) = setup_scoped(plain(), None, vec![Script::Hang]);

    let waiter = spawn_get_with(&provider, &["a"]);
    settle().await;
    assert_eq!(fake.live(), 1);
    provider.shutdown().await;

    assert_eq!(fake.live(), 0);
    assert!(matches!(
        waiter.await.unwrap(),
        Err(ClientCredentialsProviderError::ShutDown)
    ));
    for scopes in [&["a"][..], &["b"][..]] {
        assert!(matches!(
            get_with(&provider, scopes).await,
            Err(ClientCredentialsProviderError::ShutDown)
        ));
    }
    assert_eq!(fake.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn dropping_the_last_handle_cancels_dynamic_tasks_without_leaking() {
    let (provider, fake) = setup_scoped(plain(), None, vec![Script::Hang]);

    let waiter = spawn_get_with(&provider, &["a"]);
    settle().await;
    let slot = {
        let registry = provider.handle.registry.lock();
        Arc::downgrade(&registry.slots.values().next().unwrap().shared)
    };
    waiter.abort();
    settle().await;
    assert_eq!(fake.live(), 1);

    drop(provider);
    settle().await;
    assert_eq!(fake.live(), 0);
    assert!(slot.upgrade().is_none());
}
