use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use async_trait::async_trait;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::*;
use crate::models::{CacheControl, ClientCredentialsRequest};

/// One scripted backend reaction, consumed in call order.
enum Script {
    /// Succeed; durations are seconds from the wall clock at completion.
    /// Negative `expires_in` means already expired.
    Ok {
        cache_for: Option<u64>,
        expires_in: Option<i64>,
    },
    Unauthorized,
    /// Wait for the sender, then run the inner script.
    Gated(oneshot::Receiver<()>, Box<Self>),
    /// Never complete.
    Hang,
    Panic,
}

fn cached(secs: u64) -> Script {
    Script::Ok {
        cache_for: Some(secs),
        expires_in: None,
    }
}

fn cached_expiring(cache_for: u64, expires_in: i64) -> Script {
    Script::Ok {
        cache_for: Some(cache_for),
        expires_in: Some(expires_in),
    }
}

fn no_store(expires_in: Option<i64>) -> Script {
    Script::Ok {
        cache_for: None,
        expires_in,
    }
}

fn gated(script: Script) -> (oneshot::Sender<()>, Script) {
    let (tx, rx) = oneshot::channel();
    (tx, Script::Gated(rx, Box::new(script)))
}

struct Fake {
    wall: WallClock,
    script: Mutex<VecDeque<Script>>,
    /// Tokens seen, in call order.
    tokens: Mutex<Vec<String>>,
    /// Cache control of each request, in call order.
    requests: Mutex<Vec<RequestCacheControl>>,
    serial: AtomicUsize,
    /// Validation futures currently alive.
    alive: Arc<AtomicUsize>,
}

struct Alive(Arc<AtomicUsize>);

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Fake {
    fn calls(&self) -> usize {
        self.tokens.lock().len()
    }

    fn alive(&self) -> usize {
        self.alive.load(Ordering::SeqCst)
    }

    fn respond(&self, script: &Script) -> Outcome {
        match script {
            Script::Ok {
                cache_for,
                expires_in,
            } => {
                let wall = (self.wall)();
                let (cache_for, expires_in) = (*cache_for, *expires_in);
                let serial = self.serial.fetch_add(1, Ordering::SeqCst) + 1;
                let mut builder = SecurityContext::builder()
                    .subject_id(Uuid::from_u128(serial as u128))
                    .subject_tenant_id(Uuid::from_u128(7));
                if let Some(secs) = expires_in {
                    builder = builder.expires_at(wall + time::Duration::seconds(secs));
                }
                let control = cache_for.map_or_else(CacheControl::no_store, |secs| {
                    CacheControl::until(wall + Duration::from_secs(secs))
                });
                Ok(AuthenticationResult::new(builder.build().unwrap()).with_cache_control(control))
            }
            Script::Unauthorized => Err(AuthNResolverError::Unauthorized("bad token".into())),
            Script::Gated(..) | Script::Hang => unreachable!("handled by the caller"),
            Script::Panic => panic!("backend panicked"),
        }
    }
}

#[async_trait]
impl AuthNResolverClient for Fake {
    async fn authenticate(&self, request: &AuthenticationRequest) -> Outcome {
        let token = request.bearer_token.expose_secret();
        self.tokens.lock().push(token.to_owned());
        self.requests.lock().push(request.cache_control.clone());
        self.alive.fetch_add(1, Ordering::SeqCst);
        let _alive = Alive(Arc::clone(&self.alive));
        let script = self.script.lock().pop_front().expect("unscripted call");
        match script {
            Script::Gated(gate, inner) => {
                drop(gate.await);
                self.respond(&inner)
            }
            Script::Hang => std::future::pending().await,
            other => self.respond(&other),
        }
    }

    async fn exchange_client_credentials(&self, _: &ClientCredentialsRequest) -> Outcome {
        unreachable!("not used by the authenticator")
    }
}

fn wall_clock() -> WallClock {
    let start = Instant::now();
    let base = OffsetDateTime::from_unix_timestamp(1_900_000_000).unwrap();
    Arc::new(move || base + (Instant::now() - start))
}

fn config() -> CachingAuthenticatorConfig {
    CachingAuthenticatorConfig::default().with_max_ttl(Duration::from_secs(3600))
}

fn setup(
    config: CachingAuthenticatorConfig,
    script: Vec<Script>,
) -> (CachingAuthenticator, Arc<Fake>) {
    let wall = wall_clock();
    let fake = Arc::new(Fake {
        wall: Arc::clone(&wall),
        script: Mutex::new(script.into()),
        tokens: Mutex::default(),
        requests: Mutex::default(),
        serial: AtomicUsize::new(0),
        alive: Arc::default(),
    });
    let auth = CachingAuthenticator::from_parts(fake.clone(), config, wall).unwrap();
    (auth, fake)
}

fn serial(result: &AuthenticationResult) -> u128 {
    result.security_context.subject_id().as_u128()
}

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

async fn advance(secs: u64) {
    tokio::time::advance(Duration::from_secs(secs)).await;
    settle().await;
}

fn spawn_auth(
    auth: &CachingAuthenticator,
    token: &'static str,
) -> JoinHandle<AuthenticatorOutcome> {
    let auth = auth.clone();
    tokio::spawn(async move { auth.authenticate(token).await })
}

fn running(auth: &CachingAuthenticator) -> usize {
    *auth.handle.inner.running.borrow()
}

fn entries(auth: &CachingAuthenticator) -> usize {
    let state = auth.handle.inner.state.lock();
    state
        .records
        .values()
        .filter(|record| record.cached.is_some())
        .count()
}

fn is_unauthorized(outcome: &AuthenticatorOutcome) -> bool {
    matches!(
        outcome,
        Err(CachingAuthenticatorError::Resolver(
            AuthNResolverError::Unauthorized(_)
        ))
    )
}

// --- reuse ------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_cacheable_result_is_reused_for_its_token_only() {
    let (auth, fake) = setup(config(), vec![cached(100), cached(100)]);

    let a = auth.authenticate("token-a").await.unwrap();
    let again = auth.authenticate("token-a").await.unwrap();
    let b = auth.authenticate("token-b").await.unwrap();

    assert_eq!(serial(&a), serial(&again));
    assert_ne!(serial(&a), serial(&b));
    assert_eq!(*fake.tokens.lock(), ["token-a", "token-b"]);
}

#[tokio::test(start_paused = true)]
async fn reuse_ends_at_the_earliest_of_cache_until_expiry_and_max_ttl() {
    for (script, max_ttl, reuse) in [
        (cached(10), 3600, 10),
        (cached_expiring(100, 20), 3600, 20),
        (cached_expiring(100, 200), 30, 30),
        // Unknown expiry with a finite producer deadline: cached, then capped.
        (cached(100), 40, 40),
    ] {
        let config = config().with_max_ttl(Duration::from_secs(max_ttl));
        let (auth, fake) = setup(config, vec![script, cached(100)]);
        let first = auth.authenticate("t").await.unwrap();
        advance(reuse - 1).await;
        assert_eq!(
            serial(&auth.authenticate("t").await.unwrap()),
            serial(&first)
        );
        advance(1).await;
        assert_ne!(
            serial(&auth.authenticate("t").await.unwrap()),
            serial(&first)
        );
        assert_eq!(fake.calls(), 2, "reuse {reuse}");
    }
}

#[tokio::test(start_paused = true)]
async fn no_store_and_elapsed_deadlines_are_delivered_but_never_cached() {
    let elapsed = Script::Ok {
        cache_for: Some(0),
        expires_in: Some(60),
    };
    let (auth, fake) = setup(config(), vec![no_store(Some(60)), elapsed, no_store(None)]);

    auth.authenticate("t").await.unwrap();
    let closed = auth.authenticate("t").await.unwrap();
    assert!(
        closed.cache_control.is_no_store(),
        "an elapsed window is reported as closed"
    );
    let unbounded = auth.authenticate("t").await.unwrap();
    assert!(unbounded.security_context.expires_at().is_none());

    assert_eq!(fake.calls(), 3);
    assert_eq!(entries(&auth), 0);
}

// --- expiry -----------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_known_expired_success_is_refused_once_and_not_cached() {
    for script in [cached_expiring(100, 0), no_store(Some(-5))] {
        let (auth, fake) = setup(config(), vec![script, cached(100)]);
        assert!(is_unauthorized(&auth.authenticate("t").await));
        assert_eq!(fake.calls(), 1, "no revalidation loop");
        assert_eq!(entries(&auth), 0);
    }
}

#[tokio::test(start_paused = true)]
async fn a_delivery_expiring_before_collection_is_validated_again() {
    let (auth, fake) = setup(config(), vec![no_store(Some(10)), no_store(Some(-1))]);
    let delivery = uncollected(&auth, "t").await;
    advance(10).await;
    assert!(!collectable(&auth, &delivery), "expired before collection");

    // The caller loops: the new validation's expired outcome ends the call.
    assert!(is_unauthorized(&auth.authenticate("t").await));
    assert_eq!(fake.calls(), 2);
}

/// Like [`setup`], with a wall clock the test can move forward by whole
/// seconds while Tokio time stays put.
fn setup_with_offset(
    config: CachingAuthenticatorConfig,
    script: Vec<Script>,
) -> (CachingAuthenticator, Arc<Fake>, Arc<AtomicU64>) {
    let offset = Arc::new(AtomicU64::new(0));
    let base = wall_clock();
    let jump = Arc::clone(&offset);
    let wall: WallClock =
        Arc::new(move || base() + Duration::from_secs(jump.load(Ordering::SeqCst)));
    let fake = Arc::new(Fake {
        wall: Arc::clone(&wall),
        script: Mutex::new(script.into()),
        tokens: Mutex::default(),
        requests: Mutex::default(),
        serial: AtomicUsize::new(0),
        alive: Arc::default(),
    });
    let auth = CachingAuthenticator::from_parts(fake.clone(), config, wall).unwrap();
    (auth, fake, offset)
}

#[tokio::test(start_paused = true)]
async fn a_held_delivery_outlives_the_reuse_window_but_not_credential_expiry() {
    // Reuse ends at 5s (`max_ttl`); the credential, when known, at 20s.
    for (script, collect_at, held) in [
        (cached(10), 30, true),
        (cached_expiring(10, 20), 15, true),
        (cached_expiring(10, 20), 20, false),
    ] {
        let config = config().with_max_ttl(Duration::from_secs(5));
        let (auth, fake) = setup(config, vec![script, cached(100)]);
        let delivery = uncollected(&auth, "t").await;
        advance(collect_at).await;
        assert_eq!(
            collectable(&auth, &delivery),
            held,
            "collected at {collect_at}s"
        );
        let next = auth.authenticate("t").await.unwrap();
        assert_ne!(serial(&next), serial(&delivered(&delivery)));
        assert_eq!(fake.calls(), 2, "the expired entry is not reused");
    }
}

#[tokio::test(start_paused = true)]
async fn a_forward_wall_jump_refuses_hits_and_deliveries_by_their_own_deadlines() {
    // The jump passes the 10s reuse window; it passes credential expiry only
    // when that is 5s. Tokio time does not move, so only the wall bound can
    // refuse.
    for (expires_in, held) in [(None, true), (Some(15), true), (Some(5), false)] {
        let script = Script::Ok {
            cache_for: Some(10),
            expires_in,
        };
        let (auth, fake, offset) = setup_with_offset(config(), vec![script, Script::Hang]);
        let delivery = uncollected(&auth, "t").await;
        offset.store(12, Ordering::SeqCst);
        assert_eq!(
            collectable(&auth, &delivery),
            held,
            "expires in {expires_in:?}"
        );
        assert!(
            matches!(
                auth.handle.inner.next_step(TokenKey::of("t"), "t"),
                Step::Wait(_)
            ),
            "the cache hit is refused: expires in {expires_in:?}"
        );
        settle().await;
        assert_eq!(fake.calls(), 2);
    }
}

#[tokio::test(start_paused = true)]
async fn a_call_gives_up_after_three_discarded_validations() {
    // Each `clear` cancels the running validation, so the caller starts
    // another; the third discard ends the call instead of a fourth start.
    for clears in [2, 3] {
        let mut script: Vec<Script> = (0..clears).map(|_| Script::Hang).collect();
        script.push(cached(100));
        let (auth, fake) = setup(config(), script);
        let caller = spawn_auth(&auth, "t");
        for _ in 0..clears {
            settle().await;
            auth.clear();
        }
        let outcome = caller.await.unwrap();
        if clears < 3 {
            assert!(outcome.is_ok(), "{clears} discards are retried");
        } else {
            assert!(
                matches!(
                    outcome,
                    Err(CachingAuthenticatorError::Resolver(
                        AuthNResolverError::ServiceUnavailable(_)
                    ))
                ),
                "{clears} discards: {outcome:?}"
            );
        }
        assert_eq!(fake.calls(), 3);
    }
}

#[tokio::test(start_paused = true)]
async fn shutdown_during_the_last_validation_reports_shut_down() {
    let (auth, _fake) = setup(config(), vec![Script::Hang, Script::Hang, Script::Hang]);
    let caller = spawn_auth(&auth, "t");
    for _ in 0..2 {
        settle().await;
        auth.clear();
    }
    settle().await;
    auth.shutdown().await;
    assert!(matches!(
        caller.await.unwrap(),
        Err(CachingAuthenticatorError::ShutDown)
    ));
}

// --- failures ---------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn failures_are_shared_by_waiters_and_never_cached() {
    let (gate, script) = gated(Script::Unauthorized);
    let (auth, fake) = setup(config(), vec![script, cached(100)]);

    let waiters: Vec<_> = (0..3).map(|_| spawn_auth(&auth, "t")).collect();
    settle().await;
    gate.send(()).unwrap();
    for waiter in waiters {
        assert!(is_unauthorized(&waiter.await.unwrap()));
    }
    assert_eq!(fake.calls(), 1, "one validation for all waiters");

    // No negative cache, no backoff: the next call validates at once.
    auth.authenticate("t").await.unwrap();
    assert_eq!(fake.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_timeout_fails_as_service_unavailable() {
    let config = config().with_acquisition_timeout(Duration::from_secs(5));
    let (auth, fake) = setup(config, vec![Script::Hang]);
    let waiter = spawn_auth(&auth, "t");
    advance(5).await;
    assert!(matches!(
        waiter.await.unwrap(),
        Err(CachingAuthenticatorError::Resolver(
            AuthNResolverError::ServiceUnavailable(_)
        ))
    ));
    assert_eq!(fake.alive(), 0);
    assert_eq!(running(&auth), 0);
}

#[tokio::test(start_paused = true)]
async fn a_panicking_validation_fails_its_waiters_without_poisoning() {
    let (auth, _fake) = setup(config(), vec![Script::Panic, cached(100)]);
    let waiters: Vec<_> = (0..2).map(|_| spawn_auth(&auth, "t")).collect();
    for waiter in waiters {
        assert!(matches!(
            waiter.await.unwrap(),
            Err(CachingAuthenticatorError::Resolver(
                AuthNResolverError::Internal(_)
            ))
        ));
    }
    assert_eq!(running(&auth), 0);
    auth.authenticate("t").await.unwrap();
}

// --- single flight and cancellation ------------------------------------------

#[tokio::test(start_paused = true)]
async fn concurrent_callers_share_one_validation_per_token() {
    let (gate, script) = gated(cached(100));
    let (auth, fake) = setup(config(), vec![script, cached(100)]);

    let same: Vec<_> = (0..4).map(|_| spawn_auth(&auth, "t")).collect();
    let other = spawn_auth(&auth, "u");
    settle().await;
    gate.send(()).unwrap();
    let serials: Vec<_> = futures_join(same).await.iter().map(serial).collect();
    assert!(serials.windows(2).all(|pair| pair[0] == pair[1]));
    other.await.unwrap().unwrap();
    assert_eq!(fake.calls(), 2);
}

async fn futures_join(handles: Vec<JoinHandle<AuthenticatorOutcome>>) -> Vec<AuthenticationResult> {
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap().unwrap());
    }
    results
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_caller_leaves_the_validation_running_and_cached() {
    let (gate, script) = gated(cached(100));
    let (auth, fake) = setup(config(), vec![script]);

    let caller = spawn_auth(&auth, "t");
    settle().await;
    caller.abort();
    settle().await;
    gate.send(()).unwrap();
    settle().await;

    assert_eq!(entries(&auth), 1);
    auth.authenticate("t").await.unwrap();
    assert_eq!(fake.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_finished_validation_leaves_no_flight_or_permit_behind() {
    let (gate, script) = gated(cached(100));
    let (auth, _fake) = setup(config(), vec![script]);
    let caller = spawn_auth(&auth, "t");
    settle().await;
    assert_eq!(running(&auth), 1);
    gate.send(()).unwrap();
    caller.await.unwrap().unwrap();
    // The flight owned the validation's token copy; it is gone. (The cached
    // result may still carry the token in its `SecurityContext`.)
    assert!(auth.handle.inner.state.lock().flights.is_empty());
    assert_eq!(running(&auth), 0);
}

// --- pressure ---------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn saturation_rejects_new_tokens_but_serves_hits_and_joiners() {
    let (gate, script) = gated(cached(100));
    let config = config().with_max_in_flight(1);
    let (auth, fake) = setup(config, vec![cached(100), script]);

    auth.authenticate("warm").await.unwrap();
    let first = spawn_auth(&auth, "held");
    settle().await;
    let joiner = spawn_auth(&auth, "held");
    settle().await;

    auth.authenticate("warm").await.unwrap();
    let error = auth.authenticate("new").await.unwrap_err();
    assert!(
        matches!(
            error,
            CachingAuthenticatorError::Overloaded { max_in_flight: 1 }
        ),
        "{error:?}"
    );
    assert!(error.to_string().contains('1'));
    assert_eq!(fake.calls(), 2, "overload never reaches the resolver");

    gate.send(()).unwrap();
    assert_eq!(
        serial(&first.await.unwrap().unwrap()),
        serial(&joiner.await.unwrap().unwrap())
    );
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_validation_counts_until_its_task_has_ended() {
    let config = config().with_max_in_flight(1);
    let (auth, fake) = setup(config, vec![Script::Hang, cached(100)]);

    let caller = spawn_auth(&auth, "a");
    settle().await;
    caller.abort();
    // The abort is requested; the task cannot observe it before it is polled.
    auth.clear();
    assert_eq!(
        (running(&auth), fake.alive()),
        (1, 1),
        "aborted but not yet ended"
    );
    assert!(matches!(
        auth.authenticate("b").await,
        Err(CachingAuthenticatorError::Overloaded { .. })
    ));
    settle().await;
    assert_eq!((running(&auth), fake.alive()), (0, 0));
    auth.authenticate("b").await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn repeated_clears_cannot_exceed_the_validation_bound() {
    let config = config().with_max_in_flight(2);
    let (auth, fake) = setup(config, vec![Script::Hang, Script::Hang, Script::Hang]);
    let _a = spawn_auth(&auth, "a");
    let _b = spawn_auth(&auth, "b");
    settle().await;
    auth.clear();
    // Waiters retry immediately, but aborted tasks still hold their permits.
    let _c = spawn_auth(&auth, "c");
    tokio::task::yield_now().await;
    assert!(running(&auth) <= 2);
    assert!(fake.alive() <= 2);
}

#[tokio::test(start_paused = true)]
async fn a_full_cache_evicts_the_entry_expiring_first() {
    let config = config().with_max_entries(2);
    let (auth, fake) = setup(
        config,
        vec![cached(50), cached(10), cached(100), cached(100)],
    );
    let long = auth.authenticate("long").await.unwrap();
    auth.authenticate("short").await.unwrap();
    auth.authenticate("third").await.unwrap();

    assert_eq!(entries(&auth), 2);
    assert_eq!(
        serial(&auth.authenticate("long").await.unwrap()),
        serial(&long)
    );
    auth.authenticate("short").await.unwrap();
    assert_eq!(fake.calls(), 4, "only the short-lived entry was evicted");
}

// --- invalidation and lifecycle -----------------------------------------------

#[tokio::test(start_paused = true)]
async fn invalidate_drops_only_the_entry_behind_this_result() {
    let (auth, fake) = setup(config(), vec![cached(100), cached(100), cached(100)]);
    let (other, _other_fake) = setup(config(), vec![cached(100)]);

    let first = auth.authenticate("t").await.unwrap();
    let foreign = other.authenticate("t").await.unwrap();
    // Same flight id and generation in another cache: must not match.
    assert_eq!(
        foreign.origin().unwrap().slot(),
        first.origin().unwrap().slot()
    );
    let raw = AuthenticationResult::new(first.security_context.clone());

    assert!(!auth.invalidate(&foreign));
    assert!(!auth.invalidate(&raw));
    assert!(auth.invalidate(&first));
    assert!(!auth.invalidate(&first), "already gone");

    let second = auth.authenticate("t").await.unwrap();
    assert_ne!(serial(&second), serial(&first));
    assert!(!auth.invalidate(&first), "a newer entry is never touched");
    assert_eq!(
        serial(&auth.authenticate("t").await.unwrap()),
        serial(&second)
    );
    let unrelated = auth.authenticate("u").await.unwrap();
    assert!(auth.invalidate(&second));
    assert_eq!(
        serial(&auth.authenticate("u").await.unwrap()),
        serial(&unrelated)
    );
    assert_eq!(fake.calls(), 3);
}

/// Run a validation for `token` to completion and return its delivery as a
/// waiter that has not collected it yet would see it.
async fn uncollected(auth: &CachingAuthenticator, token: &str) -> Delivery {
    let inner = &auth.handle.inner;
    let (step, launch) = inner.poll(TokenKey::of(token), token);
    inner.launch(launch.expect("a new validation"));
    let Step::Wait(slot) = step else {
        panic!("expected a flight")
    };
    settle().await;
    let delivery = slot.borrow().clone();
    delivery.expect("completed")
}

fn delivered(delivery: &Delivery) -> AuthenticationResult {
    match delivery {
        Delivery::Success { result, .. } => AuthenticationResult::clone(result),
        Delivery::Failure(error) => panic!("expected success, got {error}"),
    }
}

fn collectable(auth: &CachingAuthenticator, delivery: &Delivery) -> bool {
    auth.handle.inner.accept(delivery).is_some()
}

#[tokio::test(start_paused = true)]
async fn invalidation_fences_only_that_publications_uncollected_delivery() {
    let (auth, fake) = setup(config(), vec![cached(100), cached(100), cached(100)]);
    let a = uncollected(&auth, "a").await;
    let b = uncollected(&auth, "b").await;

    assert!(auth.invalidate(&delivered(&a)));
    assert!(
        !collectable(&auth, &a),
        "the paused waiter must not get the rejected result"
    );
    assert!(
        collectable(&auth, &b),
        "another token's delivery is unaffected"
    );

    // A newer publication for `a`; the old result is now a stale no-op.
    let newer_a = uncollected(&auth, "a").await;
    assert!(!auth.invalidate(&delivered(&a)));
    assert!(collectable(&auth, &newer_a));
    assert!(collectable(&auth, &b));
    assert_eq!(fake.calls(), 3);
}

#[tokio::test(start_paused = true)]
async fn no_op_invalidations_fence_nothing() {
    let config = config().with_max_entries(1);
    let (auth, _fake) = setup(config, vec![no_store(Some(60)), cached(100), cached(100)]);
    let uncached = uncollected(&auth, "n").await;
    let evicted = uncollected(&auth, "e").await;
    let current = uncollected(&auth, "c").await; // evicts `e`

    assert!(!auth.invalidate(&delivered(&uncached)), "never cached");
    assert!(!auth.invalidate(&delivered(&evicted)), "already evicted");
    assert!(!auth.invalidate(&AuthenticationResult::new(SecurityContext::anonymous())));
    for delivery in [&uncached, &evicted, &current] {
        assert!(
            collectable(&auth, delivery),
            "eviction and no-ops do not fence"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn clear_fences_every_uncollected_delivery() {
    let (auth, fake) = setup(config(), vec![cached(100), no_store(None), cached(100)]);
    let a = uncollected(&auth, "a").await;
    let b = uncollected(&auth, "b").await;
    auth.clear();
    assert!(!collectable(&auth, &a));
    assert!(!collectable(&auth, &b));
    let fresh = auth.authenticate("a").await.unwrap();
    assert_ne!(serial(&fresh), serial(&delivered(&a)));
    assert_eq!(fake.calls(), 3);
}

#[tokio::test(start_paused = true)]
async fn clear_cancels_validations_and_their_waiters_validate_again() {
    let (auth, fake) = setup(config(), vec![cached(100), Script::Hang, cached(100)]);
    auth.authenticate("cached").await.unwrap();
    let waiter = spawn_auth(&auth, "t");
    settle().await;

    auth.clear();
    assert_eq!(entries(&auth), 0);
    settle().await;
    assert_eq!(serial(&waiter.await.unwrap().unwrap()), 2);
    assert_eq!(fake.calls(), 3);
    assert_eq!(fake.alive(), 0, "the hung validation was cancelled");
}

#[tokio::test(start_paused = true)]
async fn shutdown_cancels_waits_and_rejects_later_calls() {
    let (auth, fake) = setup(config(), vec![Script::Hang]);
    let waiter = spawn_auth(&auth, "t");
    settle().await;

    auth.shutdown().await;
    assert_eq!(running(&auth), 0);
    assert_eq!(fake.alive(), 0);
    assert!(matches!(
        waiter.await.unwrap(),
        Err(CachingAuthenticatorError::ShutDown)
    ));
    assert!(matches!(
        auth.authenticate("t").await,
        Err(CachingAuthenticatorError::ShutDown)
    ));
    auth.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn dropping_the_last_handle_cancels_validations() {
    let (auth, fake) = setup(config(), vec![Script::Hang]);
    let inner = Arc::clone(&auth.handle.inner);
    let waiter = {
        let auth = auth.clone();
        tokio::spawn(async move { auth.authenticate("t").await })
    };
    settle().await;
    waiter.abort();
    drop(auth);
    settle().await;
    assert_eq!(*inner.running.borrow(), 0);
    assert_eq!(fake.alive(), 0);
}

// --- configuration, transport mapping, secrecy ---------------------------------

#[test]
fn the_config_is_validated_at_build() {
    let fake: Arc<dyn AuthNResolverClient> = setup_less_fake();
    let build = |config| {
        CachingAuthenticator::builder(fake.clone())
            .with_config(config)
            .build()
            .map(drop)
    };
    let base = CachingAuthenticatorConfig::default();
    for (config, error) in [
        (
            base.clone().with_max_entries(0),
            CachingAuthenticatorConfigError::MaxEntries,
        ),
        (
            base.clone().with_max_entries(MAX_ENTRIES + 1),
            CachingAuthenticatorConfigError::MaxEntries,
        ),
        (
            base.clone().with_max_in_flight(0),
            CachingAuthenticatorConfigError::MaxInFlight,
        ),
        (
            base.clone().with_max_in_flight(MAX_IN_FLIGHT + 1),
            CachingAuthenticatorConfigError::MaxInFlight,
        ),
        (
            base.clone().with_max_ttl(Duration::ZERO),
            CachingAuthenticatorConfigError::MaxTtl,
        ),
        (
            base.clone()
                .with_max_ttl(MAX_DURATION + Duration::from_secs(1)),
            CachingAuthenticatorConfigError::MaxTtl,
        ),
        (
            base.clone().with_acquisition_timeout(Duration::ZERO),
            CachingAuthenticatorConfigError::AcquisitionTimeout,
        ),
        (
            base.clone()
                .with_acquisition_timeout(MAX_DURATION + Duration::from_secs(1)),
            CachingAuthenticatorConfigError::AcquisitionTimeout,
        ),
    ] {
        assert_eq!(build(config), Err(error));
    }
    assert_eq!(build(base.clone()), Ok(()));
    let maxima = base
        .clone()
        .with_max_entries(MAX_ENTRIES)
        .with_max_in_flight(MAX_IN_FLIGHT)
        .with_max_ttl(MAX_DURATION)
        .with_acquisition_timeout(MAX_DURATION);
    assert_eq!(build(maxima), Ok(()), "every maximum is inclusive");
    assert_eq!(
        base,
        CachingAuthenticatorConfig {
            max_entries: 10_000,
            max_in_flight: 256,
            max_ttl: Duration::from_secs(30),
            acquisition_timeout: Duration::from_secs(30),
        }
    );
}

fn setup_less_fake() -> Arc<dyn AuthNResolverClient> {
    Arc::new(Fake {
        wall: wall_clock_without_runtime(),
        script: Mutex::default(),
        tokens: Mutex::default(),
        requests: Mutex::default(),
        serial: AtomicUsize::new(0),
        alive: Arc::default(),
    })
}

fn wall_clock_without_runtime() -> WallClock {
    Arc::new(OffsetDateTime::now_utc)
}

#[tokio::test(start_paused = true)]
async fn the_bearer_adapter_maps_errors_to_transport_safe_kinds() {
    let config = config().with_max_in_flight(1);
    let (auth, _fake) = setup(
        config,
        vec![cached(100), Script::Unauthorized, Script::Hang],
    );
    let bearer = toolkit_security::DynBearerAuthenticator::new(auth.clone());

    assert!(bearer.authenticate("ok").await.is_ok());
    assert!(matches!(
        bearer.authenticate("bad").await,
        Err(AuthNError::InvalidToken)
    ));
    let _held = spawn_auth(&auth, "held");
    settle().await;
    assert!(matches!(
        bearer.authenticate("new").await,
        Err(AuthNError::Unavailable)
    ));
    auth.shutdown().await;
    assert!(matches!(
        bearer.authenticate("ok").await,
        Err(AuthNError::Unavailable)
    ));
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn debug_output_and_overload_logs_reveal_neither_token_nor_digest() {
    const TOKEN: &str = "secret-token-value";
    let (auth, _fake) = setup(config().with_max_in_flight(1), vec![Script::Hang]);
    let _held = spawn_auth(&auth, TOKEN);
    settle().await;
    assert!(matches!(
        auth.authenticate("other-token").await,
        Err(CachingAuthenticatorError::Overloaded { .. })
    ));

    let digest = TokenKey::of(TOKEN)
        .0
        .iter()
        .fold(String::new(), |mut hex, byte| {
            use std::fmt::Write as _;
            write!(hex, "{byte:02x}").unwrap();
            hex
        });
    let keys = format!(
        "{:?}",
        auth.handle
            .inner
            .state
            .lock()
            .flights
            .keys()
            .collect::<Vec<_>>()
    );
    assert_eq!(keys, "[TokenKey(..)]");
    let debug = format!("{auth:?}");
    for text in [&keys, &debug] {
        assert!(!text.contains(TOKEN) && !text.contains(&digest), "{text}");
    }
    assert!(logs_contain("all validations in use"));
    assert!(!logs_contain(TOKEN));
    assert!(!logs_contain("other-token"));
    assert!(!logs_contain(&digest));
}
