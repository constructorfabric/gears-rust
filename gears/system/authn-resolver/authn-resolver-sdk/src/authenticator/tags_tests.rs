//! Plugin-cache refresh hints: what [`CachingAuthenticator`] asks of a plugin
//! that keeps its own authenticate cache.

use std::collections::HashMap;

use async_trait::async_trait;
use uuid::Uuid;

use super::*;
use crate::models::{CacheControl, ClientCredentialsRequest};

/// A plugin with its own per-token cache, tagged with a generation number.
///
/// It serves its cached result unless asked to refresh: `Refresh(None)`
/// always validates, `Refresh(Some(tag))` reuses only a strictly newer
/// generation. A revoked token fails validation but its stale cached result
/// would still be served to `AllowCached`.
struct CachingPlugin {
    wall: WallClock,
    /// Caller reuse the plugin grants, in seconds; `None` means no-store.
    cache_for: Option<u64>,
    state: Mutex<PluginState>,
}

#[derive(Default)]
struct PluginState {
    generation: u64,
    cache: HashMap<String, (u64, AuthenticationResult)>,
    revoked: Vec<String>,
    outage: bool,
    /// Every request's cache control, in call order.
    requests: Vec<RequestCacheControl>,
    validations: usize,
}

impl CachingPlugin {
    fn new(cache_for: Option<u64>) -> Arc<Self> {
        Arc::new(Self {
            wall: wall_clock(),
            cache_for,
            state: Mutex::default(),
        })
    }

    fn requests(&self) -> Vec<RequestCacheControl> {
        self.state.lock().requests.clone()
    }

    fn last_request(&self) -> RequestCacheControl {
        self.requests().last().cloned().expect("a request")
    }

    fn validations(&self) -> usize {
        self.state.lock().validations
    }

    fn revoke(&self, token: &str) {
        self.state.lock().revoked.push(token.to_owned());
    }

    fn set_outage(&self, outage: bool) {
        self.state.lock().outage = outage;
    }

    /// Validate `token` out of band (another consumer), caching a newer result.
    fn validate_elsewhere(&self, token: &str) {
        let mut state = self.state.lock();
        self.validate(&mut state, token).expect("valid token");
    }

    fn validate(&self, state: &mut PluginState, token: &str) -> Outcome {
        state.validations += 1;
        if state.outage {
            return Err(AuthNResolverError::ServiceUnavailable("idp down".into()));
        }
        if state.revoked.iter().any(|revoked| revoked == token) {
            state.cache.remove(token);
            return Err(AuthNResolverError::Unauthorized("revoked".into()));
        }
        state.generation += 1;
        let generation = state.generation;
        let context = SecurityContext::builder()
            .subject_id(Uuid::from_u128(u128::from(generation)))
            .subject_tenant_id(Uuid::from_u128(7))
            .build()
            .expect("valid context");
        let control = self
            .cache_for
            .map_or_else(CacheControl::no_store, |secs| {
                CacheControl::until((self.wall)() + Duration::from_secs(secs))
            })
            .with_tag(CacheTag::new(format!("g{generation}")));
        let result = AuthenticationResult::new(context).with_cache_control(control);
        state
            .cache
            .insert(token.to_owned(), (generation, result.clone()));
        Ok(result)
    }
}

fn generation_of(tag: &CacheTag) -> u64 {
    tag.as_str()[1..].parse().expect("a generation tag")
}

#[async_trait]
impl AuthNResolverClient for CachingPlugin {
    async fn authenticate(&self, request: &AuthenticationRequest) -> Outcome {
        let token = request.bearer_token.expose_secret();
        let mut state = self.state.lock();
        state.requests.push(request.cache_control.clone());
        let cached = state.cache.get(token).cloned();
        let reuse = match (&request.cache_control, &cached) {
            (_, None) | (RequestCacheControl::Refresh { replacing: None }, _) => false,
            (RequestCacheControl::AllowCached, Some(_)) => true,
            (
                RequestCacheControl::Refresh {
                    replacing: Some(tag),
                },
                Some((generation, _)),
            ) => *generation > generation_of(tag),
        };
        match cached {
            Some((_, result)) if reuse => Ok(result),
            _ => self.validate(&mut state, token),
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

fn authenticator(
    plugin: &Arc<CachingPlugin>,
    config: CachingAuthenticatorConfig,
) -> CachingAuthenticator {
    CachingAuthenticator::from_parts(plugin.clone(), config, Arc::clone(&plugin.wall)).unwrap()
}

fn config() -> CachingAuthenticatorConfig {
    CachingAuthenticatorConfig::default().with_max_ttl(Duration::from_secs(3600))
}

fn refresh_if(tag: &str) -> RequestCacheControl {
    RequestCacheControl::refresh_unless_newer_than(CacheTag::new(tag))
}

fn tag(result: &AuthenticationResult) -> &str {
    result.cache_control.tag.as_ref().expect("tagged").as_str()
}

async fn advance(secs: u64) {
    tokio::time::advance(Duration::from_secs(secs)).await;
}

fn records(auth: &CachingAuthenticator) -> (usize, usize) {
    let state = auth.handle.inner.state.lock();
    let cached = state
        .records
        .values()
        .filter(|record| record.cached.is_some())
        .count();
    (cached, state.records.len() - cached)
}

#[tokio::test(start_paused = true)]
async fn a_token_without_history_asks_for_an_unconditional_refresh() {
    let plugin = CachingPlugin::new(Some(60));
    plugin.validate_elsewhere("t"); // the plugin already caches a result
    let auth = authenticator(&plugin, config());

    let first = auth.authenticate("t").await.unwrap();
    assert_eq!(plugin.requests(), [RequestCacheControl::refresh()]);
    assert_eq!(tag(&first), "g2", "never the plugin's unseen cached result");

    auth.authenticate("t").await.unwrap();
    assert_eq!(plugin.requests().len(), 1, "a local hit asks nothing");
}

#[tokio::test(start_paused = true)]
async fn local_expiry_and_the_ttl_cap_send_the_retained_tag() {
    for (cache_for, max_ttl) in [(10, 3600), (3600, 10)] {
        let plugin = CachingPlugin::new(Some(cache_for));
        let auth = authenticator(&plugin, config().with_max_ttl(Duration::from_secs(max_ttl)));
        let first = auth.authenticate("t").await.unwrap();
        advance(10).await;

        let second = auth.authenticate("t").await.unwrap();
        assert_eq!(plugin.last_request(), refresh_if(tag(&first)));
        assert_ne!(
            tag(&second),
            tag(&first),
            "an equal tag forces revalidation"
        );
        assert_eq!(plugin.validations(), 2);
    }
}

#[tokio::test(start_paused = true)]
async fn a_provably_newer_plugin_result_is_reused() {
    let plugin = CachingPlugin::new(Some(10));
    let auth = authenticator(&plugin, config());
    let first = auth.authenticate("t").await.unwrap();
    plugin.validate_elsewhere("t"); // g2, newer than ours
    advance(10).await;

    let second = auth.authenticate("t").await.unwrap();
    assert_eq!(plugin.last_request(), refresh_if(tag(&first)));
    assert_eq!(tag(&second), "g2");
    assert_eq!(plugin.validations(), 2, "served from the plugin cache");
}

#[tokio::test(start_paused = true)]
async fn invalidation_refreshes_with_the_retained_tag_and_never_returns_the_rejected_result() {
    let plugin = CachingPlugin::new(Some(600));
    let auth = authenticator(&plugin, config());
    let rejected = auth.authenticate("t").await.unwrap();
    // A caller cannot steer the hint through the result it passes back.
    let mut forged = rejected.clone();
    forged.cache_control.tag = Some(CacheTag::new("g999"));

    assert!(auth.invalidate(&forged));
    assert_eq!(records(&auth), (0, 1), "the entry became a refresh hint");
    plugin.revoke("t");
    assert!(matches!(
        auth.authenticate("t").await,
        Err(CachingAuthenticatorError::Resolver(
            AuthNResolverError::Unauthorized(_)
        ))
    ));
    assert_eq!(plugin.last_request(), refresh_if(tag(&rejected)));
}

#[tokio::test(start_paused = true)]
async fn a_failed_refresh_keeps_the_tag_for_the_retry() {
    let plugin = CachingPlugin::new(Some(10));
    let auth = authenticator(&plugin, config());
    let first = auth.authenticate("t").await.unwrap();
    advance(10).await;

    plugin.set_outage(true);
    assert!(auth.authenticate("t").await.is_err());
    plugin.set_outage(false);
    auth.authenticate("t").await.unwrap();

    let expected = refresh_if(tag(&first));
    assert_eq!(plugin.requests()[1..], [expected.clone(), expected]);
}

#[tokio::test(start_paused = true)]
async fn clear_and_eviction_forget_tags_so_the_next_request_is_unconditional() {
    let plugin = CachingPlugin::new(Some(600));
    let auth = authenticator(&plugin, config().with_max_entries(1));

    auth.authenticate("a").await.unwrap();
    auth.authenticate("b").await.unwrap(); // evicts `a`
    auth.authenticate("a").await.unwrap();
    assert_eq!(
        plugin.last_request(),
        RequestCacheControl::refresh(),
        "evicted"
    );

    auth.clear();
    auth.authenticate("a").await.unwrap();
    assert_eq!(
        plugin.last_request(),
        RequestCacheControl::refresh(),
        "cleared"
    );
}

#[tokio::test(start_paused = true)]
async fn hints_count_toward_max_entries_and_are_evicted_before_live_entries() {
    let plugin = CachingPlugin::new(None); // tagged no-store: hints only
    let live = CachingPlugin::new(Some(600));
    let auth = authenticator(&plugin, config().with_max_entries(2));
    let mixed = authenticator(&live, config().with_max_entries(2));

    auth.authenticate("a").await.unwrap();
    auth.authenticate("b").await.unwrap();
    auth.authenticate("c").await.unwrap();
    assert_eq!(records(&auth), (0, 2), "hints are bounded by max_entries");

    let kept = mixed.authenticate("live").await.unwrap();
    mixed.authenticate("other").await.unwrap();
    assert!(mixed.invalidate(&mixed.authenticate("other").await.unwrap()));
    mixed.authenticate("third").await.unwrap(); // evicts the hint for `other`
    assert_eq!(records(&mixed), (2, 0));
    let again = mixed.authenticate("live").await.unwrap();
    assert_eq!(tag(&again), tag(&kept), "the live entry survived");
}

#[tokio::test(start_paused = true)]
async fn a_tagged_no_store_result_is_never_served_but_its_tag_guides_the_next_request() {
    let plugin = CachingPlugin::new(None);
    let auth = authenticator(&plugin, config());

    let first = auth.authenticate("t").await.unwrap();
    let second = auth.authenticate("t").await.unwrap();
    assert_eq!(
        plugin.requests(),
        [RequestCacheControl::refresh(), refresh_if(tag(&first))]
    );
    assert_ne!(tag(&second), tag(&first));
    assert!(!auth.invalidate(&second), "nothing was cached");
}

#[tokio::test(start_paused = true)]
async fn an_untagged_result_is_forgotten_once_it_expires() {
    let plugin = Arc::new(UntaggedPlugin {
        wall: wall_clock(),
        requests: Mutex::default(),
    });
    let auth = CachingAuthenticator::from_parts(plugin.clone(), config(), Arc::clone(&plugin.wall))
        .unwrap();
    auth.authenticate("t").await.unwrap();
    advance(10).await;
    auth.authenticate("t").await.unwrap();
    assert_eq!(
        *plugin.requests.lock(),
        [
            RequestCacheControl::refresh(),
            RequestCacheControl::refresh()
        ]
    );
    assert_eq!(records(&auth), (1, 0));
}

/// Cacheable for 10s, never tagged.
struct UntaggedPlugin {
    wall: WallClock,
    requests: Mutex<Vec<RequestCacheControl>>,
}

#[async_trait]
impl AuthNResolverClient for UntaggedPlugin {
    async fn authenticate(&self, request: &AuthenticationRequest) -> Outcome {
        self.requests.lock().push(request.cache_control.clone());
        let until = (self.wall)() + Duration::from_secs(10);
        Ok(AuthenticationResult::new(SecurityContext::anonymous())
            .with_cache_control(CacheControl::until(until)))
    }

    async fn exchange_client_credentials(&self, _: &ClientCredentialsRequest) -> Outcome {
        unreachable!("not used by the authenticator")
    }
}

#[tokio::test(start_paused = true)]
async fn concurrent_callers_share_one_request() {
    let plugin = CachingPlugin::new(Some(600));
    let auth = authenticator(&plugin, config());
    let callers: Vec<_> = (0..4)
        .map(|_| {
            let auth = auth.clone();
            tokio::spawn(async move { auth.authenticate("t").await })
        })
        .collect();
    for caller in callers {
        caller.await.unwrap().unwrap();
    }
    assert_eq!(plugin.requests(), [RequestCacheControl::refresh()]);
}
