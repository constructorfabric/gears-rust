use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::*;
use crate::models::{CacheControl, ClientCredentialsRequest};

/// What the fake does on one acquisition.
enum Script {
    /// Succeed; durations are seconds from the wall clock at acquisition.
    Ok {
        cache_for: Option<u64>,
        expires_in: Option<u64>,
        tag: Option<&'static str>,
    },
    Err(AuthNResolverError),
    /// Wait until the test sends the step to perform.
    Gate(oneshot::Receiver<Script>),
    /// Never complete.
    Hang,
    /// Panic inside the acquisition.
    Panic,
    /// Behave like a plugin with its own cache: on `AllowCached`, hand back
    /// the previous result unchanged; otherwise mint a new one.
    PluginCache {
        cache_for: u64,
        tag: &'static str,
    },
}

fn cached(secs: u64) -> Script {
    Script::Ok {
        cache_for: Some(secs),
        expires_in: None,
        tag: None,
    }
}

fn cached_tagged(secs: u64, tag: &'static str) -> Script {
    Script::Ok {
        cache_for: Some(secs),
        expires_in: None,
        tag: Some(tag),
    }
}

fn no_store() -> Script {
    Script::Ok {
        cache_for: None,
        expires_in: None,
        tag: None,
    }
}

fn gate() -> (oneshot::Sender<Script>, Script) {
    let (tx, rx) = oneshot::channel();
    (tx, Script::Gate(rx))
}

fn unavailable() -> AuthNResolverError {
    AuthNResolverError::ServiceUnavailable("idp down".to_owned())
}

fn rejected() -> AuthNResolverError {
    AuthNResolverError::TokenAcquisitionFailed("invalid_client".to_owned())
}

struct Fake {
    wall: WallClock,
    script: Mutex<VecDeque<Script>>,
    requests: Mutex<Vec<RequestCacheControl>>,
    /// Requested scopes, in acquisition order.
    scopes: Mutex<Vec<Option<ScopeSet>>>,
    /// Acquisition futures currently alive.
    live: Arc<AtomicUsize>,
    serial: AtomicU64,
    /// Plugin-cache stand-in, keyed like the plugin's own cache.
    last: Mutex<HashMap<Option<ScopeSet>, AuthenticationResult>>,
}

struct LiveGuard(Arc<AtomicUsize>);

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Fake {
    fn calls(&self) -> usize {
        self.requests.lock().len()
    }

    fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    fn requests(&self) -> Vec<RequestCacheControl> {
        self.requests.lock().clone()
    }

    fn scopes(&self) -> Vec<Option<ScopeSet>> {
        self.scopes.lock().clone()
    }

    fn finish(&self, step: Script) -> Outcome {
        match step {
            Script::Ok {
                cache_for,
                expires_in,
                tag,
            } => {
                let now = (self.wall)();
                let serial = self.serial.fetch_add(1, Ordering::SeqCst) + 1;
                let mut ctx = SecurityContext::builder()
                    .subject_id(Uuid::from_u128(u128::from(serial)))
                    .subject_tenant_id(Uuid::from_u128(7));
                if let Some(secs) = expires_in {
                    ctx = ctx.expires_at(now + Duration::from_secs(secs));
                }
                let mut cache_control = cache_for.map_or_else(CacheControl::no_store, |secs| {
                    CacheControl::until(now + Duration::from_secs(secs))
                });
                if let Some(tag) = tag {
                    cache_control = cache_control.with_tag(CacheTag::new(tag));
                }
                Ok(AuthenticationResult::new(ctx.build().unwrap())
                    .with_cache_control(cache_control))
            }
            Script::Err(error) => Err(error),
            Script::Gate(_) | Script::Hang | Script::Panic | Script::PluginCache { .. } => {
                unreachable!("resolved by acquire")
            }
        }
    }
}

#[async_trait]
impl Acquire for Fake {
    async fn acquire(
        &self,
        scopes: Option<&ScopeSet>,
        cache_control: RequestCacheControl,
    ) -> Outcome {
        self.requests.lock().push(cache_control.clone());
        self.scopes.lock().push(scopes.cloned());
        self.live.fetch_add(1, Ordering::SeqCst);
        let _guard = LiveGuard(Arc::clone(&self.live));
        let step = self
            .script
            .lock()
            .pop_front()
            .expect("unscripted acquisition");
        match step {
            Script::Gate(rx) => {
                let step = rx.await.expect("gate dropped");
                self.finish(step)
            }
            Script::Hang => std::future::pending().await,
            Script::Panic => panic!("acquirer bug"),
            Script::PluginCache { cache_for, tag } => {
                let reuse = cache_control == RequestCacheControl::AllowCached;
                if reuse && let Some(last) = self.last.lock().get(&scopes.cloned()).cloned() {
                    return Ok(last);
                }
                let fresh = self.finish(cached_tagged(cache_for, tag))?;
                self.last.lock().insert(scopes.cloned(), fresh.clone());
                Ok(fresh)
            }
            step => self.finish(step),
        }
    }
}

/// Wall clock that advances with (paused) Tokio time.
fn wall_clock() -> WallClock {
    let start = Instant::now();
    let base = OffsetDateTime::from_unix_timestamp(1_900_000_000).unwrap();
    Arc::new(move || base + (Instant::now() - start))
}

/// No margin: entries remain usable for exactly `cache_for`.
fn plain() -> ClientCredentialsProviderConfig {
    ClientCredentialsProviderConfig::default().with_safety_margin(Duration::ZERO)
}

/// No jitter: refresh windows and backoff delays are exact.
fn no_jitter() -> Jitter {
    Arc::new(|_| Duration::ZERO)
}

fn setup(
    config: ClientCredentialsProviderConfig,
    script: Vec<Script>,
) -> (ClientCredentialsProvider, Arc<Fake>) {
    setup_with_wall(config, script, wall_clock())
}

fn setup_with_wall(
    config: ClientCredentialsProviderConfig,
    script: Vec<Script>,
    wall: WallClock,
) -> (ClientCredentialsProvider, Arc<Fake>) {
    setup_with(config, script, wall, no_jitter())
}

fn setup_with(
    config: ClientCredentialsProviderConfig,
    script: Vec<Script>,
    wall: WallClock,
    jitter: Jitter,
) -> (ClientCredentialsProvider, Arc<Fake>) {
    setup_with_scopes(config, None, script, wall, jitter)
}

/// A provider whose configured slot requests `configured` scopes.
fn setup_scoped(
    config: ClientCredentialsProviderConfig,
    configured: Option<&[&str]>,
    script: Vec<Script>,
) -> (ClientCredentialsProvider, Arc<Fake>) {
    setup_with_scopes(config, configured, script, wall_clock(), no_jitter())
}

fn setup_with_scopes(
    config: ClientCredentialsProviderConfig,
    configured: Option<&[&str]>,
    script: Vec<Script>,
    wall: WallClock,
    jitter: Jitter,
) -> (ClientCredentialsProvider, Arc<Fake>) {
    let fake = Arc::new(Fake {
        wall: Arc::clone(&wall),
        script: Mutex::new(script.into()),
        requests: Mutex::new(Vec::new()),
        scopes: Mutex::new(Vec::new()),
        live: Arc::new(AtomicUsize::new(0)),
        serial: AtomicU64::new(0),
        last: Mutex::new(HashMap::new()),
    });
    let configured = configured.map(|scopes| ScopeSet::new(scopes).unwrap());
    let provider =
        ClientCredentialsProvider::from_acquirer(fake.clone(), configured, config, wall, jitter)
            .unwrap();
    (provider, fake)
}

fn serial(result: &AuthenticationResult) -> u128 {
    result.security_context.subject_id().as_u128()
}

/// Let spawned tasks run without advancing (paused) time.
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

async fn advance(secs: u64) {
    tokio::time::advance(Duration::from_secs(secs)).await;
    settle().await;
}

fn spawn_get(provider: &ClientCredentialsProvider) -> JoinHandle<ProviderOutcome> {
    let provider = provider.clone();
    tokio::spawn(async move { provider.get().await })
}

fn refresh_if(tag: Option<&str>) -> RequestCacheControl {
    RequestCacheControl::Refresh {
        replacing: tag.map(CacheTag::new),
    }
}

// --- reuse bounds --------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn reuses_entry_until_cache_until_minus_safety_margin() {
    let config = plain().with_safety_margin(Duration::from_secs(10));
    let (provider, fake) = setup(config, vec![cached(100), cached(100)]);

    assert_eq!(serial(&provider.get().await.unwrap()), 1);
    advance(89).await;
    assert_eq!(serial(&provider.get().await.unwrap()), 1);
    assert_eq!(fake.calls(), 1);

    advance(2).await;
    assert_eq!(serial(&provider.get().await.unwrap()), 2);
    assert_eq!(fake.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn reuse_stops_at_credential_expiry_when_earlier_than_cache_until() {
    let config = plain().with_safety_margin(Duration::from_secs(10));
    let short_token = Script::Ok {
        cache_for: Some(1000),
        expires_in: Some(50),
        tag: None,
    };
    let (provider, fake) = setup(config, vec![short_token, cached(1000)]);

    let first = provider.get().await.unwrap();
    let expires_at = first.security_context.expires_at().unwrap();
    advance(39).await;
    let again = provider.get().await.unwrap();
    assert_eq!(serial(&again), 1);
    assert_eq!(
        again.security_context.expires_at(),
        Some(expires_at),
        "the reported expiry is the credential's, not recomputed on reuse"
    );

    advance(2).await;
    assert_eq!(serial(&provider.get().await.unwrap()), 2);
    assert_eq!(fake.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn lifetime_shorter_than_safety_margin_is_not_stored() {
    let config = plain().with_safety_margin(Duration::from_secs(10));
    let (provider, fake) = setup(config, vec![cached(5), cached(5)]);

    assert_eq!(serial(&provider.get().await.unwrap()), 1);
    assert_eq!(serial(&provider.get().await.unwrap()), 2);
    assert_eq!(fake.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn no_store_result_reaches_current_waiters_once_and_is_not_reused() {
    let (release, gated) = gate();
    let (provider, fake) = setup(plain(), vec![gated, no_store()]);

    let waiters: Vec<_> = (0..3).map(|_| spawn_get(&provider)).collect();
    settle().await;
    release.send(no_store()).ok().unwrap();
    for waiter in waiters {
        assert_eq!(serial(&waiter.await.unwrap().unwrap()), 1);
    }
    assert_eq!(fake.calls(), 1, "the waiters shared one acquisition");

    // No self-scheduled refresh loop: nothing happens without a caller.
    advance(600).await;
    assert_eq!(fake.calls(), 1);

    assert_eq!(serial(&provider.get().await.unwrap()), 2);
    assert_eq!(fake.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn clones_share_the_cache_and_results_share_the_local_tag() {
    let (provider, fake) = setup(plain(), vec![cached_tagged(100, "plugin-a")]);
    let other = provider.clone();

    let first = provider.get().await.unwrap();
    let second = other.get().await.unwrap();

    assert_eq!(fake.calls(), 1);
    assert_eq!(first.origin(), second.origin());
    assert!(first.origin().is_some());
    assert_eq!(
        first.cache_control.tag,
        Some(CacheTag::new("plugin-a")),
        "the plugin tag is kept alongside the local one"
    );
}

// --- early refresh -------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn early_refresh_runs_once_in_background_and_serves_the_old_entry() {
    let config = plain();
    let (release, gated) = gate();
    let (provider, fake) = setup(config, vec![cached(100), gated]);

    assert_eq!(serial(&provider.get().await.unwrap()), 1);
    advance(79).await;
    assert_eq!(serial(&provider.get().await.unwrap()), 1);
    assert_eq!(fake.calls(), 1, "not yet in the refresh window");

    advance(2).await;
    for _ in 0..5 {
        assert_eq!(serial(&provider.get().await.unwrap()), 1);
    }
    settle().await;
    assert_eq!(fake.calls(), 2, "one background refresh for all callers");
    assert_eq!(
        fake.requests()[1],
        refresh_if(None),
        "refreshing a cacheable result must not accept the plugin's cached copy"
    );

    release.send(cached(100)).ok().unwrap();
    settle().await;
    assert_eq!(serial(&provider.get().await.unwrap()), 2);
}

#[tokio::test(start_paused = true)]
async fn refresh_jitter_moves_the_window_earlier_and_is_drawn_once_per_entry() {
    let config = plain();
    let draws = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&draws);
    // The built-in jitter opens the window up to 10s early, at t=70
    // instead of t=80.
    let jitter: Jitter = Arc::new(move |max| {
        recorded.lock().push(max);
        max
    });
    let (provider, fake) = setup_with(config, vec![cached(100), cached(100)], wall_clock(), jitter);

    provider.get().await.unwrap();
    advance(69).await;
    provider.get().await.unwrap();
    settle().await;
    assert_eq!(fake.calls(), 1, "before the jittered window");
    advance(1).await;
    provider.get().await.unwrap();
    settle().await;
    assert_eq!(fake.calls(), 2, "refresh starts at the jittered window");

    assert_eq!(
        draws.lock()[0],
        Duration::from_secs(10),
        "the built-in jitter bound is 10% of the usable lifetime"
    );
    assert_eq!(
        draws.lock().len(),
        2,
        "one draw per stored entry, not per get"
    );
}

#[test]
fn random_jitter_stays_within_its_bound() {
    assert_eq!(random_jitter(Duration::ZERO), Duration::ZERO);
    let max = Duration::from_millis(10);
    let draws: Vec<_> = (0..1000).map(|_| random_jitter(max)).collect();
    assert!(draws.iter().all(|draw| *draw <= max));
    assert!(
        draws.iter().any(|draw| *draw != draws[0]),
        "actually random"
    );
}

// --- failures and backoff ------------------------------------------------

#[tokio::test(start_paused = true)]
async fn transient_refresh_failure_serves_old_entry_until_hard_deadline_only() {
    let config = plain().with_backoff(Duration::from_secs(60), Duration::from_secs(60));
    let (provider, fake) = setup(
        config,
        vec![cached(100), Script::Err(unavailable()), cached(100)],
    );

    provider.get().await.unwrap();
    advance(80).await;
    assert_eq!(serial(&provider.get().await.unwrap()), 1);
    settle().await;
    assert_eq!(fake.calls(), 2, "refresh attempted and failed");

    advance(10).await;
    assert_eq!(serial(&provider.get().await.unwrap()), 1, "fallback");
    assert_eq!(fake.calls(), 2, "no new attempt during backoff");

    advance(11).await; // t=101: past the hard deadline, still in backoff
    let err = provider.get().await.unwrap_err();
    assert!(matches!(
        err,
        ClientCredentialsProviderError::Resolver(AuthNResolverError::ServiceUnavailable(_))
    ));
    assert_eq!(fake.calls(), 2);

    advance(40).await; // t=141: backoff over
    assert_eq!(serial(&provider.get().await.unwrap()), 2);
}

#[tokio::test(start_paused = true)]
async fn non_transient_refresh_failure_drops_the_old_entry() {
    let config = plain();
    let (provider, fake) = setup(config, vec![cached(100), Script::Err(rejected())]);

    provider.get().await.unwrap();
    advance(80).await;
    provider.get().await.unwrap();
    settle().await;

    let err = provider.get().await.unwrap_err();
    assert!(matches!(
        err,
        ClientCredentialsProviderError::Resolver(AuthNResolverError::TokenAcquisitionFailed(_))
    ));
    assert_eq!(fake.calls(), 2, "served from backoff, no fallback");
}

#[tokio::test(start_paused = true)]
async fn every_error_backs_off_exponentially_and_success_resets_it() {
    let config = plain().with_backoff(Duration::from_secs(1), Duration::from_secs(4));
    let script = vec![
        Script::Err(rejected()),
        Script::Err(rejected()),
        Script::Err(AuthNResolverError::Internal("boom".to_owned())),
        Script::Err(rejected()),
        no_store(),
        Script::Err(rejected()),
        no_store(),
    ];
    let (provider, fake) = setup(config, script);

    // (seconds to advance before the call, expected total calls after it)
    for (wait, calls) in [
        (0, 1),
        (0, 1),
        (1, 2),
        (1, 2),
        (1, 3),
        (3, 3),
        (1, 4),
        (3, 4),
    ] {
        advance(wait).await;
        assert!(provider.get().await.is_err());
        assert_eq!(fake.calls(), calls, "after waiting {wait}s");
    }
    advance(1).await; // cap of 4s reached
    assert!(provider.get().await.is_ok());
    assert_eq!(fake.calls(), 5);

    assert!(provider.get().await.is_err());
    advance(1).await; // reset to min_backoff
    assert!(provider.get().await.is_ok());
    assert_eq!(fake.calls(), 7);
}

#[tokio::test(start_paused = true)]
async fn one_failure_is_shared_with_every_waiter() {
    let (release, gated) = gate();
    let (provider, fake) = setup(plain(), vec![gated]);

    let waiters: Vec<_> = (0..5).map(|_| spawn_get(&provider)).collect();
    settle().await;
    release.send(Script::Err(unavailable())).ok().unwrap();

    for waiter in waiters {
        assert!(matches!(
            waiter.await.unwrap(),
            Err(ClientCredentialsProviderError::Resolver(
                AuthNResolverError::ServiceUnavailable(_)
            ))
        ));
    }
    assert_eq!(fake.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn hung_attempt_times_out_as_service_unavailable() {
    let config = plain().with_acquisition_timeout(Duration::from_secs(5));
    let (provider, fake) = setup(config, vec![Script::Hang]);
    let start = Instant::now();

    let err = provider.get().await.unwrap_err();

    assert!(matches!(
        err,
        ClientCredentialsProviderError::Resolver(AuthNResolverError::ServiceUnavailable(_))
    ));
    assert_eq!(
        start.elapsed(),
        Duration::from_secs(5),
        "the configured timeout"
    );
    settle().await;
    assert_eq!(fake.live(), 0, "the timed-out attempt was dropped");
}

// --- ownership and cancellation -----------------------------------------

#[tokio::test(start_paused = true)]
async fn a_caller_giving_up_does_not_cancel_the_attempt() {
    let (release, gated) = gate();
    let (provider, fake) = setup(plain(), vec![gated]);

    let waiter = spawn_get(&provider);
    settle().await;
    waiter.abort();
    settle().await;
    assert_eq!(fake.live(), 1, "still running for the next caller");

    release.send(cached(100)).ok().unwrap();
    settle().await;
    assert_eq!(serial(&provider.get().await.unwrap()), 1);
    assert_eq!(fake.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn dropping_the_last_handle_cancels_the_attempt_without_leaking() {
    let (provider, fake) = setup(plain(), vec![Script::Hang]);
    let shared = Arc::downgrade(&provider.handle.shared);
    let clone = provider.clone();

    let waiter = spawn_get(&provider);
    settle().await;
    assert_eq!(fake.live(), 1);

    waiter.abort();
    drop(provider);
    settle().await;
    assert_eq!(fake.live(), 1, "a handle is still alive");

    drop(clone);
    settle().await;
    assert_eq!(fake.live(), 0, "cancelled with the last handle");
    assert!(
        shared.upgrade().is_none(),
        "no reference cycle keeps state alive"
    );
}

#[tokio::test(start_paused = true)]
async fn shutdown_cancels_the_attempt_wakes_waiters_and_rejects_later_calls() {
    let (provider, fake) = setup(plain(), vec![Script::Hang]);

    let waiter = spawn_get(&provider);
    settle().await;
    provider.shutdown().await;

    assert_eq!(fake.live(), 0);
    // Not a resolver error: retrying this provider cannot succeed.
    assert!(matches!(
        waiter.await.unwrap(),
        Err(ClientCredentialsProviderError::ShutDown)
    ));
    assert!(matches!(
        provider.get().await,
        Err(ClientCredentialsProviderError::ShutDown)
    ));
    assert_eq!(fake.calls(), 1);
}

// --- invalidation --------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn clear_requests_refresh_of_the_prior_plugin_tag_until_it_succeeds() {
    let config = plain().with_backoff(Duration::from_secs(1), Duration::from_secs(1));
    let script = vec![
        cached_tagged(100, "a"),
        Script::Err(unavailable()),
        cached_tagged(100, "b"),
        cached(100),
    ];
    let (provider, fake) = setup(config, script);

    provider.get().await.unwrap();
    provider.clear();
    assert!(provider.get().await.is_err());
    advance(1).await;
    assert_eq!(serial(&provider.get().await.unwrap()), 2);
    advance(101).await;
    provider.get().await.unwrap();

    assert_eq!(
        fake.requests(),
        vec![
            RequestCacheControl::AllowCached,
            refresh_if(Some("a")),
            refresh_if(Some("a")),
            refresh_if(Some("b")),
        ],
        "a failed refresh keeps asking for refresh; later re-acquisitions refresh the latest tag"
    );
}

#[tokio::test(start_paused = true)]
async fn clear_without_plugin_tag_forces_unconditional_refresh() {
    let (provider, fake) = setup(plain(), vec![cached(100), cached(100)]);

    provider.get().await.unwrap();
    provider.clear();
    assert_eq!(serial(&provider.get().await.unwrap()), 2);

    assert_eq!(fake.requests()[1], refresh_if(None));
}

#[tokio::test(start_paused = true)]
async fn clear_during_initial_attempt_fences_it_and_waiters_get_refreshed_data() {
    let (stale, gated_stale) = gate();
    let (provider, fake) = setup(plain(), vec![gated_stale, cached(100)]);

    let waiter = spawn_get(&provider);
    settle().await;
    provider.clear();
    settle().await;

    assert!(
        stale.send(cached(100)).is_err(),
        "the stale attempt was cancelled"
    );
    assert_eq!(serial(&waiter.await.unwrap().unwrap()), 1);
    assert_eq!(
        fake.requests(),
        vec![RequestCacheControl::AllowCached, refresh_if(None)]
    );
    assert_eq!(serial(&provider.get().await.unwrap()), 1);
    assert_eq!(fake.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn clear_during_a_running_refresh_fences_it_and_retries() {
    let (first_refresh, gated_first) = gate();
    let (second_refresh, gated_second) = gate();
    let (provider, fake) = setup(
        plain(),
        vec![cached_tagged(100, "a"), gated_first, gated_second],
    );

    provider.get().await.unwrap();
    provider.clear();
    let waiter = spawn_get(&provider);
    settle().await;
    assert_eq!(fake.calls(), 2, "refresh running");

    // An explicit invalidation after the refresh already picked its token:
    // that token may be the rejected one, so it must not be published.
    provider.clear();
    settle().await;
    assert!(
        first_refresh.send(cached_tagged(100, "a")).is_err(),
        "the pre-clear refresh was cancelled"
    );
    assert_eq!(fake.calls(), 3);

    second_refresh.send(cached_tagged(100, "b")).ok().unwrap();
    let result = waiter.await.unwrap().unwrap();
    assert_eq!(result.cache_control.tag, Some(CacheTag::new("b")));
    assert_eq!(
        fake.requests(),
        vec![
            RequestCacheControl::AllowCached,
            refresh_if(Some("a")),
            refresh_if(Some("a")),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn clear_during_background_refresh_drops_the_entry_and_fences_the_refresh() {
    let config = plain();
    let (stale, gated) = gate();
    let (provider, fake) = setup(config, vec![cached_tagged(100, "a"), gated, no_store()]);

    provider.get().await.unwrap();
    advance(80).await;
    provider.get().await.unwrap(); // starts the Refresh("a") background refresh
    settle().await;
    provider.clear();
    settle().await;

    assert!(stale.send(cached(100)).is_err());
    assert_eq!(serial(&provider.get().await.unwrap()), 2);
    assert_eq!(
        fake.requests(),
        vec![
            RequestCacheControl::AllowCached,
            refresh_if(Some("a")),
            refresh_if(Some("a")),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_late_completion_cannot_touch_the_flight_that_replaced_it() {
    let (_stale, gated_stale) = gate();
    let (replacement, gated_replacement) = gate();
    let (provider, fake) = setup(plain(), vec![gated_stale, gated_replacement]);
    let shared = Arc::clone(&provider.handle.shared);

    let waiter = spawn_get(&provider);
    settle().await;
    let stale_id = shared.state.lock().flight.as_ref().unwrap().id;
    provider.clear();
    settle().await;
    assert_eq!(fake.calls(), 2, "the waiter joined a replacement flight");
    let current_id = shared.state.lock().flight.as_ref().unwrap().id;
    assert_ne!(current_id, stale_id);

    // Valid by the provider's own clock, so only the flight-id fence stops it.
    let (tx, _rx) = watch::channel(None);
    let late = AuthenticationResult::new(SecurityContext::anonymous()).with_cache_control(
        CacheControl::until((shared.core.wall_clock)() + Duration::from_secs(100)),
    );
    shared.complete(stale_id, Ok(late), &tx);

    {
        let state = shared.state.lock();
        assert_eq!(
            state.flight.as_ref().map(|flight| flight.id),
            Some(current_id)
        );
        assert!(state.entry.is_none());
        assert!(state.current_generation.is_none());
    }
    assert!(
        tx.borrow().is_none(),
        "nothing delivered for a fenced attempt"
    );

    replacement.send(cached(100)).ok().unwrap();
    assert_eq!(serial(&waiter.await.unwrap().unwrap()), 1);
}

#[tokio::test(start_paused = true)]
async fn clear_does_not_bypass_backoff() {
    let config = plain().with_backoff(Duration::from_secs(10), Duration::from_secs(10));
    let (provider, fake) = setup(config, vec![Script::Err(rejected()), cached(100)]);

    assert!(provider.get().await.is_err());
    for _ in 0..3 {
        provider.clear();
        assert!(provider.get().await.is_err());
    }
    assert_eq!(fake.calls(), 1);

    advance(10).await;
    assert!(provider.get().await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn invalidate_matches_only_the_latest_result_of_this_provider() {
    let (provider, fake) = setup(plain(), vec![no_store(), cached(100), cached(100)]);
    let (other, _other_fake) = setup(plain(), vec![cached(100)]);

    let first = provider.get().await.unwrap(); // no-store still gets an origin
    let foreign = other.get().await.unwrap();
    // Same slot and generation, another cache: must not match.
    assert_eq!(
        (
            foreign.origin().unwrap().slot(),
            foreign.origin().unwrap().generation()
        ),
        (
            first.origin().unwrap().slot(),
            first.origin().unwrap().generation()
        )
    );
    let raw = AuthenticationResult::new(first.security_context.clone());

    assert!(!provider.invalidate(&foreign), "another provider's result");
    assert!(
        !provider.invalidate(&raw),
        "a result not returned by a provider"
    );
    assert!(provider.invalidate(&first));
    assert!(
        !provider.invalidate(&first),
        "already cleared: a second caller with the same result is a no-op"
    );

    let second = provider.get().await.unwrap();
    assert_ne!(second.origin(), first.origin());
    assert!(!provider.invalidate(&first), "replaced result");
    assert_eq!(serial(&provider.get().await.unwrap()), 2, "entry kept");
    assert_eq!(fake.calls(), 2);
    assert_eq!(fake.requests()[1], refresh_if(None));
}

#[test]
fn invalid_config_is_rejected() {
    let base = ClientCredentialsProviderConfig::default();
    assert_eq!(base.validate(), Ok(()));
    for (config, expected) in [
        (
            base.clone().with_acquisition_timeout(Duration::ZERO),
            ClientCredentialsProviderConfigError::AcquisitionTimeout,
        ),
        (
            base.clone()
                .with_acquisition_timeout(MAX_ENTRY_LIFETIME + Duration::from_secs(1)),
            ClientCredentialsProviderConfigError::AcquisitionTimeout,
        ),
        (
            base.clone()
                .with_backoff(Duration::ZERO, Duration::from_secs(1)),
            ClientCredentialsProviderConfigError::Backoff,
        ),
        (
            base.with_backoff(Duration::from_secs(5), Duration::from_secs(1)),
            ClientCredentialsProviderConfigError::Backoff,
        ),
    ] {
        assert_eq!(config.validate(), Err(expected), "{config:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn early_refresh_gets_a_new_token_from_a_caching_plugin() {
    let config = plain();
    let script = vec![
        Script::PluginCache {
            cache_for: 100,
            tag: "gen",
        },
        Script::PluginCache {
            cache_for: 100,
            tag: "gen",
        },
    ];
    let (provider, fake) = setup(config, script);

    assert_eq!(serial(&provider.get().await.unwrap()), 1);
    advance(80).await;
    provider.get().await.unwrap();
    settle().await;

    assert_eq!(
        serial(&provider.get().await.unwrap()),
        2,
        "the refresh bypassed the plugin cache instead of re-reading token 1"
    );
    assert_eq!(fake.requests()[1], refresh_if(Some("gen")));
}

#[tokio::test(start_paused = true)]
async fn expiry_driven_reacquisition_also_refreshes() {
    let script = vec![cached_tagged(10, "a"), cached_tagged(10, "b")];
    let (provider, fake) = setup(plain(), script);

    provider.get().await.unwrap();
    advance(11).await;
    provider.get().await.unwrap();

    assert_eq!(fake.requests()[1], refresh_if(Some("a")));
}

#[tokio::test(start_paused = true)]
async fn already_expired_credential_is_a_failure_not_a_success() {
    let expired = Script::Ok {
        cache_for: None,
        expires_in: Some(0),
        tag: None,
    };
    let config = plain().with_backoff(Duration::from_secs(5), Duration::from_secs(5));
    let (provider, fake) = setup(config, vec![expired]);

    let err = provider.get().await.unwrap_err();

    assert!(matches!(
        err,
        ClientCredentialsProviderError::Resolver(AuthNResolverError::TokenAcquisitionFailed(_))
    ));
    assert!(provider.get().await.is_err(), "and it backs off");
    assert_eq!(fake.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn panicking_acquirer_fails_waiters_instead_of_wedging_them() {
    let config = plain().with_backoff(Duration::from_secs(1), Duration::from_secs(1));
    let (provider, fake) = setup(config, vec![Script::Panic, no_store()]);

    let waiters: Vec<_> = (0..3).map(|_| spawn_get(&provider)).collect();
    for waiter in waiters {
        assert!(matches!(
            waiter.await.unwrap(),
            Err(ClientCredentialsProviderError::Resolver(
                AuthNResolverError::Internal(_)
            ))
        ));
    }
    assert!(provider.handle.shared.state.lock().flight.is_none());

    advance(1).await;
    assert!(provider.get().await.is_ok(), "recovers after backoff");
    assert_eq!(fake.calls(), 2);
}

/// Barrier tests: the flight has delivered, but the waiter has not resumed
/// yet when the state changes.
async fn waiting_slot(provider: &ClientCredentialsProvider) -> DeliverySlot {
    let Step::Wait(slot) = provider.handle.shared.next_step() else {
        panic!("expected a flight");
    };
    settle().await;
    slot
}

#[tokio::test(start_paused = true)]
async fn waiter_resuming_after_clear_does_not_return_the_cleared_result() {
    let (release, gated) = gate();
    let (provider, fake) = setup(plain(), vec![gated, cached(100)]);
    let shared = Arc::clone(&provider.handle.shared);
    let slot = waiting_slot(&provider).await;

    release.send(cached(100)).ok().unwrap();
    settle().await;
    assert!(slot.borrow().is_some(), "delivered, waiter not resumed");
    provider.clear();

    assert!(
        shared.await_delivery(slot).await.is_none(),
        "must re-evaluate"
    );
    assert_eq!(serial(&provider.get().await.unwrap()), 2);
    assert_eq!(fake.requests()[1], refresh_if(None));
}

#[tokio::test(start_paused = true)]
async fn waiter_resuming_after_shutdown_gets_an_error() {
    let (release, gated) = gate();
    let (provider, _fake) = setup(plain(), vec![gated]);
    let shared = Arc::clone(&provider.handle.shared);
    let slot = waiting_slot(&provider).await;

    release.send(cached(100)).ok().unwrap();
    settle().await;
    provider.shutdown().await;

    assert!(matches!(
        shared.await_delivery(slot).await,
        Some(Err(ClientCredentialsProviderError::ShutDown))
    ));
}

#[tokio::test(start_paused = true)]
async fn delayed_waiter_never_gets_a_credential_that_expired_meanwhile() {
    let (release, gated) = gate();
    let (provider, _fake) = setup(plain(), vec![gated]);
    let shared = Arc::clone(&provider.handle.shared);
    let slot = waiting_slot(&provider).await;

    release
        .send(Script::Ok {
            cache_for: None,
            expires_in: Some(10),
            tag: None,
        })
        .ok()
        .unwrap();
    settle().await;
    let on_time = shared.accept(slot.borrow().as_ref().unwrap());
    assert!(
        matches!(on_time, Some(Ok(_))),
        "no-store fan-out while valid"
    );

    advance(11).await;
    assert!(shared.await_delivery(slot).await.is_none());
}

#[tokio::test(start_paused = true)]
async fn concurrent_and_cancelled_shutdowns_all_wait_for_the_attempt() {
    let (provider, fake) = setup(plain(), vec![Script::Hang]);
    let waiter = spawn_get(&provider);
    settle().await;
    assert_eq!(fake.live(), 1);

    // A shutdown cancelled right after it started must not lose the wait.
    {
        let cancelled = provider.shutdown();
        tokio::pin!(cancelled);
        assert!(futures_poll_once(cancelled.as_mut()).await.is_pending());
    }
    let (a, b) = tokio::join!(provider.shutdown(), provider.shutdown());
    let ((), ()) = (a, b);

    assert_eq!(fake.live(), 0);
    assert_eq!(*provider.handle.shared.running.borrow(), 0);
    assert!(waiter.await.unwrap().is_err());
}

async fn futures_poll_once<F: std::future::Future + Unpin>(fut: F) -> std::task::Poll<F::Output> {
    let mut fut = Some(fut);
    std::future::poll_fn(move |cx| {
        let poll = std::pin::Pin::new(fut.as_mut().unwrap()).poll(cx);
        std::task::Poll::Ready(poll)
    })
    .await
}

#[tokio::test(start_paused = true)]
async fn a_held_delivery_outlives_the_reuse_deadline_but_a_new_call_reacquires() {
    // The entry is reusable for 10s - 2s margin; the credential's expiry is
    // unknown, so nothing bounds the held delivery.
    let (provider, fake) = setup(
        plain().with_safety_margin(Duration::from_secs(2)),
        vec![cached(10), Script::Hang],
    );
    let slot = waiting_slot(&provider).await;
    advance(30).await;
    assert!(matches!(
        provider.handle.shared.await_delivery(slot).await,
        Some(Ok(_))
    ));
    assert!(
        matches!(provider.handle.shared.next_step(), Step::Wait(_)),
        "the expired entry is not reused"
    );
    settle().await;
    assert_eq!(fake.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_call_gives_up_after_three_discarded_acquisitions() {
    // Each `clear` cancels the pending acquisition, so the caller starts
    // another; the third discard ends the call instead of a fourth start,
    // without recording a failure for the slot.
    for clears in [2, 3] {
        let mut script: Vec<Script> = (0..clears).map(|_| Script::Hang).collect();
        script.extend([cached(100), cached(100)]);
        let (provider, fake) = setup(plain(), script);
        let caller = {
            let provider = provider.clone();
            tokio::spawn(async move { provider.get().await })
        };
        for _ in 0..clears {
            settle().await;
            provider.clear();
        }
        let outcome = caller.await.unwrap();
        if clears < 3 {
            assert!(outcome.is_ok(), "{clears} discards are retried");
        } else {
            assert!(
                matches!(
                    outcome,
                    Err(ClientCredentialsProviderError::Resolver(
                        AuthNResolverError::ServiceUnavailable(_)
                    ))
                ),
                "{clears} discards: {outcome:?}"
            );
            assert!(provider.get().await.is_ok(), "no backoff was recorded");
        }
        assert_eq!(fake.calls(), 3 + usize::from(clears == 3));
    }
}

#[tokio::test(start_paused = true)]
async fn shutdown_during_the_last_acquisition_reports_shut_down() {
    let (provider, _fake) = setup(plain(), vec![Script::Hang, Script::Hang, Script::Hang]);
    let caller = {
        let provider = provider.clone();
        tokio::spawn(async move { provider.get().await })
    };
    for _ in 0..2 {
        settle().await;
        provider.clear();
    }
    settle().await;
    provider.shutdown().await;
    assert!(matches!(
        caller.await.unwrap(),
        Err(ClientCredentialsProviderError::ShutDown)
    ));
}

#[tokio::test(start_paused = true)]
async fn non_transient_failure_fences_an_earlier_successful_delivery() {
    for error in [
        rejected(),
        AuthNResolverError::Unauthorized("revoked".into()),
    ] {
        let config = plain();
        let (provider, _) = setup(config, vec![cached(100), Script::Err(error)]);
        let slot = waiting_slot(&provider).await;
        advance(80).await;
        provider.get().await.unwrap(); // triggers on-demand refresh
        settle().await;
        assert!(provider.handle.shared.await_delivery(slot).await.is_none());
        assert!(provider.get().await.is_err());
    }
}

#[tokio::test(start_paused = true)]
async fn absolute_deadline_is_checked_on_cache_hits_and_deliveries() {
    // A forward wall jump past the reuse deadline refuses the cache hit; it
    // refuses the held delivery only when it also passes credential expiry.
    for (expires_in, delivered) in [(None, true), (Some(15), true), (Some(5), false)] {
        let offset = Arc::new(AtomicU64::new(0));
        let clock_offset = offset.clone();
        let wall = wall_clock();
        let wall =
            Arc::new(move || wall() + Duration::from_secs(clock_offset.load(Ordering::SeqCst)));
        let (provider, fake) = setup_with_wall(
            plain(),
            vec![
                Script::Ok {
                    cache_for: Some(10),
                    expires_in,
                    tag: None,
                },
                Script::Hang,
            ],
            wall,
        );
        let slot = waiting_slot(&provider).await;
        offset.store(12, Ordering::SeqCst);
        assert_eq!(
            provider.handle.shared.await_delivery(slot).await.is_some(),
            delivered,
            "expires in {expires_in:?}"
        );
        assert!(
            matches!(provider.handle.shared.next_step(), Step::Wait(_)),
            "expired cache hit must acquire"
        );
        settle().await;
        assert_eq!(fake.calls(), 2);
    }
}

#[tokio::test(start_paused = true)]
async fn backwards_wall_jump_does_not_extend_delivery_or_cache_lifetime() {
    let offset = Arc::new(AtomicU64::new(0));
    let clock_offset = offset.clone();
    let wall = wall_clock();
    let wall = Arc::new(move || wall() - Duration::from_secs(clock_offset.load(Ordering::SeqCst)));
    // Known expiry, so the delivery has a deadline whose monotonic half must hold.
    let expiring = Script::Ok {
        cache_for: Some(10),
        expires_in: Some(10),
        tag: None,
    };
    let (provider, _) = setup_with_wall(plain(), vec![expiring, Script::Hang], wall);
    let slot = waiting_slot(&provider).await;
    offset.store(100, Ordering::SeqCst);
    advance(10).await;
    assert!(provider.handle.shared.await_delivery(slot).await.is_none());
    assert!(matches!(provider.handle.shared.next_step(), Step::Wait(_)));
}

#[test]
fn overflowing_config_durations_are_rejected() {
    assert!(
        plain()
            .with_backoff(Duration::MAX, Duration::MAX)
            .validate()
            .is_err()
    );
    assert!(
        plain()
            .with_acquisition_timeout(Duration::MAX)
            .validate()
            .is_err()
    );
}

// --- review follow-ups -----------------------------------------------------

#[tokio::test(start_paused = true)]
async fn far_future_cache_deadline_is_tracked_for_at_most_a_year() {
    const CENTURY: u64 = 100 * 365 * 24 * 3600;
    let (provider, fake) = setup(plain(), vec![cached(CENTURY), cached(CENTURY)]);

    assert_eq!(serial(&provider.get().await.unwrap()), 1);
    advance(MAX_ENTRY_LIFETIME.as_secs() - 1).await;
    assert_eq!(serial(&provider.get().await.unwrap()), 1, "still cached");
    advance(1).await;
    assert_eq!(
        serial(&provider.get().await.unwrap()),
        2,
        "re-acquired at the one-year bound although cache_until is a century away"
    );
    assert_eq!(fake.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn valid_credential_with_an_elapsed_cache_deadline_is_delivered_as_no_store() {
    for expires_in in [None, Some(100)] {
        let elapsed_reuse = Script::Ok {
            cache_for: Some(0),
            expires_in,
            tag: Some("t"),
        };
        let (provider, fake) = setup(plain(), vec![elapsed_reuse, cached(100)]);

        let first = provider.get().await.unwrap();
        assert_eq!(serial(&first), 1, "a success, not TokenAcquisitionFailed");
        assert!(first.cache_control.is_no_store());
        assert_eq!(first.cache_control.tag, Some(CacheTag::new("t")));

        assert_eq!(serial(&provider.get().await.unwrap()), 2, "not stored");
        assert_eq!(
            fake.requests()[1],
            refresh_if(Some("t")),
            "the plugin cached it, so its copy must not come back"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn elapsed_cache_deadline_replaces_the_previous_entry_without_backoff() {
    let config = plain();
    let elapsed_reuse = Script::Ok {
        cache_for: Some(0),
        expires_in: Some(100),
        tag: None,
    };
    let (provider, fake) = setup(config, vec![cached(100), elapsed_reuse, cached(100)]);

    provider.get().await.unwrap();
    advance(80).await;
    provider.get().await.unwrap(); // background refresh gets the elapsed one
    settle().await;
    assert_eq!(fake.calls(), 2);

    {
        let state = provider.handle.shared.state.lock();
        assert!(state.failure.is_none(), "no backoff");
        assert_eq!(state.consecutive_failures, 0);
        assert!(
            state.entry.is_none(),
            "the uncacheable refresh replaced the entry"
        );
    }

    assert_eq!(serial(&provider.get().await.unwrap()), 3, "acquired again");
    assert_eq!(fake.calls(), 3);
}

#[tokio::test(start_paused = true)]
async fn uncached_short_delivery_is_bounded_by_credential_expiry_only() {
    // The margin exceeds both lifetimes, so nothing is stored; the reuse
    // window does not bound the attempt's own waiters, the credential does.
    for (cache_for, expires_in, bound) in [
        (10, None, None),
        (10, Some(5), Some(5)),
        (5, Some(10), Some(10)),
    ] {
        let config = plain().with_safety_margin(Duration::from_secs(20));
        let result = Script::Ok {
            cache_for: Some(cache_for),
            expires_in,
            tag: None,
        };
        let (provider, _) = setup(config, vec![result]);
        let shared = Arc::clone(&provider.handle.shared);
        let slot = waiting_slot(&provider).await;
        assert!(shared.state.lock().entry.is_none());

        let Some(bound) = bound else {
            advance(3600).await;
            assert!(
                matches!(shared.await_delivery(slot).await, Some(Ok(_))),
                "{cache_for}s: no known expiry, still deliverable"
            );
            continue;
        };
        advance(bound - 1).await;
        assert!(matches!(
            shared.accept(slot.borrow().as_ref().unwrap()),
            Some(Ok(_))
        ));
        advance(1).await;
        assert!(
            shared.await_delivery(slot).await.is_none(),
            "{cache_for}s/{expires_in:?}: rejected at {bound}s"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_newer_uncached_result_does_not_fence_an_earlier_delivery() {
    // Both are delivered without being stored: plain no-store, and a valid
    // credential whose reuse window had already closed.
    let elapsed_reuse = || Script::Ok {
        cache_for: Some(0),
        expires_in: Some(100),
        tag: Some("t"),
    };
    for (first_result, second_result) in
        [(no_store(), no_store()), (elapsed_reuse(), elapsed_reuse())]
    {
        let (provider, fake) = setup(plain(), vec![first_result, second_result]);
        let shared = Arc::clone(&provider.handle.shared);
        let first = waiting_slot(&provider).await;
        let first_tag = match first.borrow().as_ref().unwrap() {
            Delivery::Success { result, .. } => result.origin().unwrap(),
            Delivery::Failure(_) => panic!("expected a success"),
        };

        // A later caller completes its own flight before the first waiter resumes.
        let second = provider.get().await.unwrap();
        assert_eq!(serial(&second), 2);
        assert!(
            !provider.invalidate_origin(first_tag),
            "a stale invalidation is a no-op and fences nothing"
        );

        let outcome = shared.await_delivery(first).await.unwrap().unwrap();
        assert_eq!(serial(&outcome), 1, "its own result");
        assert_eq!(outcome.origin(), Some(first_tag));
        assert_eq!(fake.calls(), 2, "no extra acquisition");
        assert!(
            provider.invalidate(&second),
            "accepting the earlier delivery left the latest tag current"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_matching_conditional_clear_fences_a_pending_delivery() {
    let (provider, _) = setup(plain(), vec![no_store()]);
    let shared = Arc::clone(&provider.handle.shared);
    let slot = waiting_slot(&provider).await;
    let tag = match slot.borrow().as_ref().unwrap() {
        Delivery::Success { result, .. } => result.origin().unwrap(),
        Delivery::Failure(_) => panic!("expected a success"),
    };

    assert!(provider.invalidate_origin(tag));
    assert!(shared.await_delivery(slot).await.is_none());
}

#[test]
fn starting_an_acquisition_outside_a_runtime_panics_instead_of_deadlocking() {
    let (provider, fake) = setup(plain(), vec![no_store()]);
    let probe = provider.clone();
    let (done, finished) = std::sync::mpsc::channel();
    // A thread, so a regression shows up as a timeout instead of a hung suite.
    std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut get = std::pin::pin!(probe.get());
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            drop(get.as_mut().poll(&mut cx));
        }));
        done.send(outcome.is_err()).unwrap();
    });

    let panicked = finished
        .recv_timeout(Duration::from_secs(10))
        .expect("get() deadlocked when tokio dropped the task inside spawn");
    assert!(panicked, "tokio::spawn outside a runtime panics");
    let state = provider.handle.shared.state.lock();
    assert!(
        state.flight.is_none(),
        "the dropped task settled its flight"
    );
    assert!(state.failure.is_some());
    assert_eq!(*provider.handle.shared.running.borrow(), 0);
    assert_eq!(fake.calls(), 0);
}

#[test]
fn backoff_doubles_until_the_delay_itself_reaches_max_backoff() {
    // (min, max, failures at which the doubled delay first exceeds max)
    for (min, max, capped_at) in [
        (Duration::from_nanos(1), Duration::from_secs(60), 37),
        (Duration::from_millis(1), MAX_ENTRY_LIFETIME, 36),
    ] {
        let (provider, _) = setup(plain().with_backoff(min, max), vec![]);
        let shared = &provider.handle.shared;
        assert_eq!(shared.backoff(1), min);
        // Past the point where a u32 factor 2^(n-1) overflows.
        for failures in [32, 33, capped_at - 1] {
            let doubled = min * 2u32.pow(16) * 2u32.pow(failures - 17);
            assert_eq!(
                shared.backoff(failures),
                doubled,
                "{min:?} after {failures}"
            );
            assert!(doubled < max);
        }
        for failures in [capped_at, capped_at + 1, 64, 200, u32::MAX] {
            assert_eq!(
                shared.backoff(failures),
                max,
                "{min:?}..{max:?} after {failures}"
            );
        }
    }
}

#[test]
fn backoff_jitter_stays_between_half_the_delay_and_the_delay() {
    let config = plain().with_backoff(Duration::from_secs(1), Duration::from_secs(60));
    let most: Jitter = Arc::new(|max| max);
    let (provider, _) = setup_with(config, vec![], wall_clock(), most);
    let shared = &provider.handle.shared;

    // (failures, delay with the largest jitter): never below min_backoff.
    for (failures, secs) in [(1, 1), (2, 1), (3, 2), (4, 4), (6, 16), (7, 30), (30, 30)] {
        assert_eq!(
            shared.backoff(failures),
            Duration::from_secs(secs),
            "{failures}"
        );
    }
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn failures_and_recovery_are_logged_without_error_messages() {
    let config = plain().with_backoff(Duration::from_secs(1), Duration::from_secs(1));
    let secret_detail = AuthNResolverError::ServiceUnavailable("token=sekrit".to_owned());
    let (provider, _) = setup(
        config,
        vec![cached(100), Script::Err(secret_detail), cached(100)],
    );

    provider.get().await.unwrap();
    advance(80).await;
    provider.get().await.unwrap(); // background refresh fails, entry kept
    settle().await;
    assert!(logs_contain("S2S credential acquisition failed"));
    assert!(logs_contain("error_kind=\"service_unavailable\""));
    assert!(logs_contain("serving_previous=true"));
    assert!(!logs_contain("sekrit"));

    advance(1).await;
    provider.get().await.unwrap();
    settle().await;
    provider.get().await.unwrap();
    settle().await;
    assert!(logs_contain("S2S credential acquisition recovered"));
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn a_margin_too_large_for_the_lifetime_is_warned_once_per_episode() {
    const WARNING: &str = "lifetime is within safety_margin";
    let warnings = |expected: usize| {
        logs_assert(|lines| {
            let count = lines.iter().filter(|line| line.contains(WARNING)).count();
            if count == expected {
                Ok(())
            } else {
                Err(format!("{count} margin warnings, expected {expected}"))
            }
        });
    };
    let elapsed = Script::Ok {
        cache_for: Some(0),
        expires_in: Some(100),
        tag: None,
    };
    let (provider, fake) = setup(
        plain().with_safety_margin(Duration::from_secs(20)),
        vec![
            no_store(),
            elapsed,
            cached(10),
            cached(10),
            cached(100),
            cached(10),
        ],
    );
    provider.get().await.unwrap();
    provider.get().await.unwrap();
    warnings(0); // no-store and closed windows are not margin issues

    provider.get().await.unwrap();
    provider.get().await.unwrap();
    warnings(1); // warned when it started, not on every acquisition
    assert!(logs_contain("safety_margin=20s"));

    provider.get().await.unwrap(); // cached: the episode ends
    advance(81).await;
    provider.get().await.unwrap();
    warnings(2); // a new episode warns again
    assert_eq!(fake.calls(), 6);
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn a_recovery_does_not_hide_the_margin_warning() {
    const WARNING: &str = "lifetime is within safety_margin";
    let warnings = |expected: usize| {
        logs_assert(|lines| {
            let count = lines.iter().filter(|line| line.contains(WARNING)).count();
            if count == expected {
                Ok(())
            } else {
                Err(format!("{count} margin warnings, expected {expected}"))
            }
        });
    };
    let config = plain()
        .with_safety_margin(Duration::from_secs(20))
        .with_backoff(Duration::from_secs(1), Duration::from_secs(1));
    let (provider, _) = setup(
        config,
        vec![
            Script::Err(rejected()),
            cached(10),
            Script::Err(rejected()),
            cached(10),
            no_store(),
            cached(10),
        ],
    );
    assert!(provider.get().await.is_err());
    advance(1).await;
    provider.get().await.unwrap();
    assert!(logs_contain("S2S credential acquisition recovered"));
    warnings(1); // the recovering acquisition is also the first short one

    assert!(provider.get().await.is_err());
    advance(1).await;
    provider.get().await.unwrap();
    warnings(1); // the same run of short results, despite the recovery

    provider.get().await.unwrap(); // a non-suppressed success ends the run
    provider.get().await.unwrap();
    warnings(2);
}

#[tokio::test(start_paused = true)]
async fn a_flight_cleared_between_reservation_and_spawn_is_aborted() {
    let (provider, fake) = setup(plain(), vec![Script::Hang, Script::Hang]);
    let shared = Arc::clone(&provider.handle.shared);

    let (_step, launch) = shared.poll();
    provider.clear(); // cannot abort yet: there is no task
    shared.launch(launch.expect("a reserved flight"));
    settle().await;

    assert_eq!(fake.live(), 0, "launch aborted the orphaned task");
    assert_eq!(*shared.running.borrow(), 0);
    assert!(shared.state.lock().flight.is_none());
}

#[test]
fn starting_an_acquisition_on_a_shut_down_runtime_settles_instead_of_deadlocking() {
    let (provider, fake) = setup(plain(), vec![no_store()]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let handle = runtime.handle().clone();
    drop(runtime); // the handle now spawns onto a closed task list
    let probe = provider.clone();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _entered = handle.enter();
        let mut get = std::pin::pin!(probe.get());
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        let poll = get.as_mut().poll(&mut cx);
        done.send(matches!(
            poll,
            std::task::Poll::Ready(Err(ClientCredentialsProviderError::Resolver(
                AuthNResolverError::Internal(_)
            )))
        ))
        .unwrap();
    });

    let failed = finished
        .recv_timeout(Duration::from_secs(10))
        .expect("get() deadlocked when tokio dropped the never-polled task");
    assert!(failed, "the dropped task fails its waiters with Internal");
    let state = provider.handle.shared.state.lock();
    assert!(state.flight.is_none());
    assert!(state.failure.is_some());
    assert_eq!(*provider.handle.shared.running.borrow(), 0);
    assert_eq!(fake.calls(), 0, "never polled");
    assert_eq!(fake.live(), 0);
}

#[path = "registry_tests.rs"]
mod registry;

#[path = "builder_tests.rs"]
mod builder;
