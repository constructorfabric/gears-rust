//! JWKS cache and fetcher.
//!
//! Maintains an in-memory cache of JSON Web Key Sets (JWKS) keyed by issuer.
//! On a cache miss (or unknown `kid`), delegates to [`OidcDiscovery`] to resolve
//! the `jwks_uri`, then fetches and caches the JWKS.
//!
//! Cache entries are fresh until the configured TTL expires, remain usable only
//! within a bounded stale window during `IdP` outages, and the cache is bounded
//! by a configurable max-entry count (default 10 issuers).

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use jsonwebtoken::jwk::JwkSet;
use reqwest::Url;
use tracing::{debug, info, instrument, warn};

use crate::config::RetryPolicyConfig;
use crate::domain::error::AuthNError;
use crate::domain::metrics::AuthNMetrics;
use crate::domain::ports::JwksProvider;
use crate::infra::circuit_breaker::{HostCircuitBreakers, host_key};
use crate::infra::http_response::read_json_response_limited;
use crate::infra::oidc::OidcDiscovery;
use crate::infra::retry::{RetriedRequestError, send_with_retry};
use crate::infra::single_flight::{
    SingleFlightGate, release_single_flight_gate as release_gate,
    single_flight_gate_with_leadership as acquire_gate,
};
use crate::infra::ttl_cache::{Timestamped, TtlCache};

/// A cached JWKS entry.
#[derive(Debug, Clone)]
pub(crate) struct CachedJwks {
    /// The JSON Web Key Set, wrapped in `Arc` for cheap per-request cloning.
    pub key_set: Arc<JwkSet>,
    /// Time at which this entry was fetched. Used for TTL expiry.
    pub fetched_at: Instant,
}

impl Timestamped for CachedJwks {
    fn fetched_at(&self) -> Instant {
        self.fetched_at
    }
}

/// Cache and refresh policy for [`JwksFetcher`].
#[derive(Debug, Clone, Copy)]
pub struct JwksFetcherConfig {
    /// Duration for which cached keys are considered fresh.
    pub ttl: Duration,
    /// Maximum age for serving stale keys when the `IdP` is unreachable.
    pub stale_ttl: Duration,
    /// Maximum number of issuers retained in the JWKS cache.
    pub max_entries: usize,
    /// Whether an unknown `kid` triggers an out-of-band JWKS refresh.
    pub refresh_on_unknown_kid: bool,
    /// Per-issuer cooldown between unknown-`kid` refresh attempts.
    pub refresh_min_interval: Duration,
}

/// Runtime collaborators required by [`JwksFetcher`].
pub struct JwksFetcherDeps {
    /// Shared OIDC discovery cache used to resolve JWKS endpoints.
    pub discovery: Arc<OidcDiscovery>,
    /// HTTP client used to fetch JWKS documents.
    pub client: reqwest::Client,
    /// Metrics recorder for JWKS cache and fetch observations.
    pub metrics: Arc<AuthNMetrics>,
    /// Retry policy applied to JWKS endpoint requests.
    pub retry_policy: RetryPolicyConfig,
}

/// JWKS cache and fetcher backed by [`OidcDiscovery`] and optional host-scoped breakers.
///
/// Thread-safe via [`DashMap`]. Each issuer is cached separately with a configurable
/// TTL and a bounded entry count. On unknown `kid`, a force-refresh is performed
/// (with cooldown rate-limiting) before declaring the key missing.
pub struct JwksFetcher {
    cache: TtlCache<CachedJwks>,
    discovery: Arc<OidcDiscovery>,
    client: reqwest::Client,
    circuit_breakers: Option<Arc<HostCircuitBreakers>>,
    last_force_refresh: DashMap<String, Instant>,
    in_flight: DashMap<String, Arc<SingleFlightGate>>,
    stale_ttl: Duration,
    refresh_on_unknown_kid: bool,
    refresh_min_interval: Duration,
    /// Injected metrics handle for recording cache and fetch statistics.
    metrics: Arc<AuthNMetrics>,
    retry_policy: RetryPolicyConfig,
}

impl std::fmt::Debug for JwksFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwksFetcher")
            .field("cached_issuers", &self.cache.len())
            .field("has_circuit_breaker", &self.circuit_breakers.is_some())
            .field("in_flight_refreshes", &self.in_flight.len())
            .finish_non_exhaustive()
    }
}

impl JwksFetcher {
    /// Create a new `JwksFetcher`.
    #[must_use]
    pub fn new(config: JwksFetcherConfig, deps: JwksFetcherDeps) -> Self {
        let JwksFetcherConfig {
            ttl,
            stale_ttl,
            max_entries,
            refresh_on_unknown_kid,
            refresh_min_interval,
        } = config;

        Self {
            cache: TtlCache::new(ttl, max_entries),
            discovery: deps.discovery,
            client: deps.client,
            circuit_breakers: None,
            last_force_refresh: DashMap::new(),
            in_flight: DashMap::new(),
            stale_ttl,
            refresh_on_unknown_kid,
            refresh_min_interval,
            metrics: deps.metrics,
            retry_policy: deps.retry_policy,
        }
    }

    /// Increment the JWKS cache hit or miss counter.
    fn record_cache_lookup(&self, hit: bool) {
        if hit {
            self.metrics.increment_jwks_cache_hit();
        } else {
            self.metrics.increment_jwks_cache_miss();
        }
    }

    /// Update current cache entry-count gauge.
    fn record_entry_count(&self) {
        self.metrics.record_jwks_cache_entries(self.cache.len());
    }

    /// Attach host-scoped circuit breakers used to guard JWKS endpoint calls.
    #[must_use]
    pub fn with_circuit_breakers(mut self, circuit_breakers: Arc<HostCircuitBreakers>) -> Self {
        self.circuit_breakers = Some(circuit_breakers);
        self
    }

    /// Get the JWKS for the given issuer.
    ///
    /// Returns the cached JWKS when it is fresh. On stale cache or cache miss,
    /// attempts a refresh and falls back to stale keys only inside the configured
    /// stale window when the `IdP` is unreachable.
    ///
    /// # Errors
    ///
    /// Returns [`AuthNError::IdpUnreachable`] if the cache is cold and the `IdP`
    /// cannot be reached.
    #[instrument(skip(self))]
    pub async fn get_jwks(
        &self,
        issuer: &str,
        discovery_base: &Url,
    ) -> Result<Arc<JwkSet>, AuthNError> {
        if let Some(entry) = self.cache.get_fresh(issuer) {
            debug!(issuer, "JWKS cache hit (fresh)");
            self.record_cache_lookup(true);
            return Ok(entry.key_set);
        }
        debug!(issuer, "JWKS cache stale or miss, attempting refresh");
        self.record_cache_lookup(false);

        match self.fetch_and_cache(issuer, discovery_base).await {
            Ok(jwks) => Ok(jwks),
            Err(e) => {
                if let Some(entry) = self.cache.get_if_age_at_most(issuer, self.stale_ttl) {
                    warn!(
                        issuer,
                        error = %e,
                        "IdP unreachable, using stale JWKS cache as fallback"
                    );
                    return Ok(entry.key_set);
                }
                Err(e)
            }
        }
    }

    /// Force-refresh the JWKS for the given issuer (used on unknown `kid`).
    ///
    /// # Errors
    ///
    /// Returns [`AuthNError::IdpUnreachable`] if the fetch fails.
    #[instrument(skip(self))]
    pub async fn force_refresh(
        &self,
        issuer: &str,
        discovery_base: &Url,
    ) -> Result<Arc<JwkSet>, AuthNError> {
        if !self.refresh_on_unknown_kid {
            debug!(
                issuer,
                "Force-refresh on unknown kid disabled by configuration"
            );
            return Err(AuthNError::KidNotFound);
        }

        // `should_refresh` is true only for the caller that created the gate.
        // Followers wait here, then re-check cache populated by the leader.
        let (gate, should_refresh) = acquire_gate(&self.in_flight, issuer);
        let result = {
            let _single_flight_guard = gate.mutex.lock().await;

            if !should_refresh && let Some(entry) = self.cache.get_fresh(issuer) {
                debug!(
                    issuer,
                    "JWKS force-refresh joined completed in-flight refresh"
                );
                Ok(entry.key_set)
            } else if !self.reserve_force_refresh_slot(issuer) {
                debug!(
                    issuer,
                    "Force-refresh cooldown active, returning cached or error"
                );
                self.cache
                    .get_if_age_at_most(issuer, self.stale_ttl)
                    .map(|entry| entry.key_set)
                    .ok_or(AuthNError::KidNotFound)
            } else {
                info!(issuer, "Force-refreshing JWKS (unknown kid / key rotation)");
                self.fetch_and_cache(issuer, discovery_base).await
            }
        };
        release_gate(&self.in_flight, issuer, &gate);
        result
    }

    fn reserve_force_refresh_slot(&self, issuer: &str) -> bool {
        // Rate-limit force refreshes per issuer to prevent request floods with
        // novel kid values from overwhelming the Oidc JWKS endpoint. The
        // check-and-update is atomic within a single DashMap shard lock.
        let mut reserved = true;
        self.last_force_refresh
            .entry(issuer.to_owned())
            .and_modify(|last| {
                if last.elapsed() >= self.refresh_min_interval {
                    *last = Instant::now();
                } else {
                    reserved = false;
                }
            })
            .or_insert_with(Instant::now);
        reserved
    }

    /// Fetch JWKS from the `IdP` and store in the cache.
    async fn fetch_and_cache(
        &self,
        issuer: &str,
        discovery_base: &Url,
    ) -> Result<Arc<JwkSet>, AuthNError> {
        let jwks = self.fetch_remote_jwks(issuer, discovery_base).await?;

        let jwks = Arc::new(jwks);

        self.cache.insert_with_eviction(
            issuer,
            CachedJwks {
                key_set: Arc::clone(&jwks),
                fetched_at: Instant::now(),
            },
            "JWKS",
        );
        self.record_entry_count();

        Ok(jwks)
    }

    #[instrument(skip(self))]
    async fn fetch_remote_jwks(
        &self,
        issuer: &str,
        discovery_base: &Url,
    ) -> Result<JwkSet, AuthNError> {
        let started_at = Instant::now();
        let result = async {
            let oidc_config = self.discovery.get_config(discovery_base).await?;
            if oidc_config.issuer != issuer {
                warn!(
                    issuer,
                    discovery_base = %discovery_base,
                    discovery_issuer = %oidc_config.issuer,
                    "OIDC discovery issuer mismatch"
                );
                return Err(AuthNError::UntrustedIssuer);
            }
            self.fetch_jwks_uri(oidc_config.jwks_url()).await
        }
        .await;
        self.metrics
            .record_jwks_fetch_duration(started_at.elapsed());
        result
    }

    async fn fetch_jwks_uri(&self, jwks_uri: &Url) -> Result<JwkSet, AuthNError> {
        if let Some(circuit_breakers) = &self.circuit_breakers {
            let host = host_key(jwks_uri);
            return circuit_breakers
                .call(&host, || async {
                    self.fetch_jwks_uri_unchecked(jwks_uri).await
                })
                .await;
        }

        self.fetch_jwks_uri_unchecked(jwks_uri).await
    }

    async fn fetch_jwks_uri_unchecked(&self, jwks_uri: &Url) -> Result<JwkSet, AuthNError> {
        let response = send_with_retry(&self.retry_policy, || {
            self.client.get(jwks_uri.clone()).send()
        })
        .await
        .map_err(|error| {
            match error {
                RetriedRequestError::Transport(e) => {
                    warn!(url = %jwks_uri, error = %e, "JWKS fetch failed");
                }
                RetriedRequestError::Status(status) => {
                    warn!(
                        url = %jwks_uri,
                        status = %status,
                        "JWKS endpoint returned non-success status"
                    );
                }
            }
            AuthNError::IdpUnreachable
        })?;

        read_json_response_limited(response).await.map_err(|e| {
            warn!(url = %jwks_uri, error = %e, "JWKS response parse failed");
            AuthNError::IdpUnreachable
        })
    }

    /// Returns `true` if the cache has a (possibly stale) entry for the issuer.
    ///
    /// Used by the circuit-breaker fallback logic.
    #[must_use]
    pub fn has_cached_entry(&self, issuer: &str) -> bool {
        self.cache.contains_key(issuer)
    }

    /// Inject a JWKS into the cache directly — for use in unit tests only.
    #[cfg(test)]
    pub fn inject_cached_jwks(&self, issuer: &str, jwks: JwkSet) {
        self.cache.insert(
            issuer,
            CachedJwks {
                key_set: Arc::new(jwks),
                fetched_at: Instant::now(),
            },
        );
        self.record_entry_count();
    }

    /// Inject a JWKS into the cache with an explicit fetch timestamp.
    #[cfg(test)]
    fn inject_cached_jwks_at(&self, issuer: &str, jwks: JwkSet, fetched_at: Instant) {
        self.cache.insert(
            issuer,
            CachedJwks {
                key_set: Arc::new(jwks),
                fetched_at,
            },
        );
        self.record_entry_count();
    }
}

#[async_trait::async_trait]
impl JwksProvider for JwksFetcher {
    async fn get_jwks(
        &self,
        issuer: &str,
        discovery_base: &Url,
    ) -> Result<Arc<JwkSet>, AuthNError> {
        JwksFetcher::get_jwks(self, issuer, discovery_base).await
    }

    async fn force_refresh(
        &self,
        issuer: &str,
        discovery_base: &Url,
    ) -> Result<Arc<JwkSet>, AuthNError> {
        JwksFetcher::force_refresh(self, issuer, discovery_base).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::mpsc;

    use crate::circuit_breaker::{HostCircuitBreakers, STATE_CLOSED, STATE_OPEN, host_key};
    use crate::domain::metrics::test_harness::MetricsHarness;
    use crate::oidc::OidcDiscovery;
    use crate::test_support::test_fixtures::test_jwk_json;
    use httpmock::prelude::{GET, HttpMockResponse, MockServer};
    use serde_json::json;

    const TEST_ISSUER: &str = "https://oidc.example.com/realms/platform";

    fn test_issuer_url() -> Url {
        Url::parse(TEST_ISSUER).expect("test issuer URL should parse")
    }

    /// Build a test JWKS from raw JWK JSON. Panics if parsing fails (test-only).
    fn parse_jwks(json: &str) -> JwkSet {
        serde_json::from_str(json).expect("test JWK JSON should parse")
    }

    fn make_fetcher_with_deps(
        config: JwksFetcherConfig,
        discovery: Arc<OidcDiscovery>,
        metrics: Arc<AuthNMetrics>,
    ) -> JwksFetcher {
        JwksFetcher::new(
            config,
            JwksFetcherDeps {
                discovery,
                client: reqwest::Client::new(),
                metrics,
                retry_policy: crate::config::default_retry_policy_config(),
            },
        )
    }

    fn make_fetcher() -> JwksFetcher {
        let discovery = Arc::new(OidcDiscovery::new(
            3600,
            10,
            reqwest::Client::new(),
            crate::config::default_retry_policy_config(),
        ));
        make_fetcher_with_deps(
            JwksFetcherConfig {
                ttl: Duration::from_hours(1),
                stale_ttl: Duration::from_hours(24),
                max_entries: 10,
                refresh_on_unknown_kid: true,
                refresh_min_interval: Duration::from_secs(30),
            },
            discovery,
            MetricsHarness::new().metrics(),
        )
    }

    fn make_fetcher_with_policy(
        ttl_secs: u64,
        stale_ttl_secs: u64,
        refresh_on_unknown_kid: bool,
        refresh_min_interval_secs: u64,
    ) -> JwksFetcher {
        let discovery = Arc::new(OidcDiscovery::new(
            3600,
            10,
            reqwest::Client::new(),
            crate::config::default_retry_policy_config(),
        ));
        make_fetcher_with_deps(
            JwksFetcherConfig {
                ttl: Duration::from_secs(ttl_secs),
                stale_ttl: Duration::from_secs(stale_ttl_secs),
                max_entries: 10,
                refresh_on_unknown_kid,
                refresh_min_interval: Duration::from_secs(refresh_min_interval_secs),
            },
            discovery,
            MetricsHarness::new().metrics(),
        )
    }

    fn cached_jwks_with_kid(kid: &str) -> JwkSet {
        let mut jwks: JwkSet =
            serde_json::from_str(test_jwk_json()).expect("test JWKS should parse");
        jwks.keys[0].common.key_id = Some(kid.to_owned());
        jwks
    }

    fn first_kid(jwks: &JwkSet) -> Option<&str> {
        jwks.keys
            .first()
            .and_then(|jwk| jwk.common.key_id.as_deref())
    }

    #[tokio::test]
    async fn test_cache_hit_returns_without_network() {
        let fetcher = make_fetcher();
        let fake_jwks = cached_jwks_with_kid("k1");

        fetcher.inject_cached_jwks(TEST_ISSUER, fake_jwks.clone());
        let discovery_base = test_issuer_url();

        // get_jwks should return the cached entry without attempting any network call
        let result = fetcher.get_jwks(TEST_ISSUER, &discovery_base).await;
        assert!(result.is_ok(), "cache hit should succeed");
        assert_eq!(result.unwrap().keys.len(), 1);
    }

    #[tokio::test]
    async fn test_cold_cache_miss_returns_idp_unreachable() {
        let fetcher = make_fetcher(); // empty cache, no injected JWKS
        let discovery_base = test_issuer_url();

        // Without a cached entry AND with an unreachable IdP, should return IdpUnreachable
        let result = fetcher.get_jwks(TEST_ISSUER, &discovery_base).await;
        assert!(
            matches!(result, Err(AuthNError::IdpUnreachable)),
            "cold cache miss with unreachable IdP should return IdpUnreachable"
        );
    }

    #[tokio::test]
    async fn test_stale_entry_falls_back_to_cached_when_idp_down() {
        // Zero TTL -> any entry is immediately stale
        let discovery = Arc::new(OidcDiscovery::new(
            3600,
            10,
            reqwest::Client::new(),
            crate::config::default_retry_policy_config(),
        ));
        let fetcher = make_fetcher_with_deps(
            JwksFetcherConfig {
                ttl: Duration::from_secs(0),
                stale_ttl: Duration::from_hours(24),
                max_entries: 10,
                refresh_on_unknown_kid: true,
                refresh_min_interval: Duration::from_secs(30),
            },
            discovery,
            MetricsHarness::new().metrics(),
        );

        let fake_jwks = cached_jwks_with_kid("stale-k");
        fetcher.inject_cached_jwks(TEST_ISSUER, fake_jwks);
        let discovery_base = test_issuer_url();

        // With TTL=0 the entry is immediately stale, but since IdP is unreachable
        // the implementation should fall back to the stale cached entry
        let result = fetcher.get_jwks(TEST_ISSUER, &discovery_base).await;
        assert!(
            result.is_ok(),
            "stale fallback should succeed when IdP is down"
        );
    }

    #[tokio::test]
    async fn stale_entry_after_configured_window_returns_idp_unreachable() {
        let fetcher = make_fetcher_with_policy(0, 5, true, 30);
        let old = Instant::now()
            .checked_sub(Duration::from_secs(10))
            .unwrap_or_else(Instant::now);
        let fake_jwks = cached_jwks_with_kid("expired-stale");
        fetcher.inject_cached_jwks_at(TEST_ISSUER, fake_jwks, old);
        let discovery_base = test_issuer_url();

        let result = fetcher.get_jwks(TEST_ISSUER, &discovery_base).await;

        assert!(
            matches!(result, Err(AuthNError::IdpUnreachable)),
            "expired-unusable stale entry should fail closed when refresh fails: {result:?}"
        );
    }

    #[tokio::test]
    async fn stale_entry_inside_configured_window_is_served_on_idp_outage() {
        let fetcher = make_fetcher_with_policy(0, 15, true, 30);
        let old = Instant::now()
            .checked_sub(Duration::from_secs(10))
            .unwrap_or_else(Instant::now);
        let fake_jwks = cached_jwks_with_kid("usable-stale");
        fetcher.inject_cached_jwks_at(TEST_ISSUER, fake_jwks, old);
        let discovery_base = test_issuer_url();

        let result = fetcher.get_jwks(TEST_ISSUER, &discovery_base).await;

        assert!(
            result.is_ok(),
            "stale-usable entry should be served when refresh fails inside stale TTL: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_stale_entry_falls_back_to_cached_when_circuit_is_open() {
        let metrics = MetricsHarness::new().metrics();
        let breakers = Arc::new(HostCircuitBreakers::new(1, 30, Arc::clone(&metrics)));
        let discovery_base = test_issuer_url();
        let host = host_key(&discovery_base);
        drop(
            breakers
                .call(&host, || async { Err::<(), _>(AuthNError::IdpUnreachable) })
                .await,
        );
        assert_eq!(breakers.state_for_host(&host), Some(STATE_OPEN));

        let discovery = Arc::new(OidcDiscovery::new(
            3600,
            10,
            reqwest::Client::new(),
            crate::config::default_retry_policy_config(),
        ));
        let fetcher = make_fetcher_with_deps(
            JwksFetcherConfig {
                ttl: Duration::from_secs(0),
                stale_ttl: Duration::from_hours(24),
                max_entries: 10,
                refresh_on_unknown_kid: true,
                refresh_min_interval: Duration::from_secs(30),
            },
            discovery,
            metrics,
        )
        .with_circuit_breakers(breakers);

        let fake_jwks = cached_jwks_with_kid("stale-open");
        fetcher.inject_cached_jwks(TEST_ISSUER, fake_jwks);

        let result = fetcher.get_jwks(TEST_ISSUER, &discovery_base).await;
        assert!(result.is_ok(), "open-circuit stale fallback should succeed");
    }

    #[tokio::test]
    async fn jwks_failure_opens_jwks_uri_breaker_not_discovery_breaker() -> anyhow::Result<()> {
        let jwks_server = MockServer::start();
        let jwks_uri_raw = jwks_server.url("/keys");
        let jwks_uri = Url::parse(&jwks_uri_raw)?;
        jwks_server.mock(|when, then| {
            when.method(GET).path("/keys");
            then.status(500)
                .header("content-type", "application/json")
                .json_body(json!({ "error": "down" }));
        });

        let discovery_server = MockServer::start();
        let discovery_base_raw = discovery_server.url("/realms/platform");
        let discovery_base = Url::parse(&discovery_base_raw)?;
        let discovery_body = json!({
            "issuer": discovery_base_raw.clone(),
            "jwks_uri": jwks_uri_raw.clone(),
        });
        discovery_server.mock(move |when, then| {
            when.method(GET)
                .path("/realms/platform/.well-known/openid-configuration");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(discovery_body);
        });

        let metrics = MetricsHarness::new().metrics();
        let breakers = Arc::new(HostCircuitBreakers::new(1, 30, Arc::clone(&metrics)));
        let discovery = Arc::new(
            OidcDiscovery::new_allowing_insecure_http_for_tests(
                3600,
                10,
                reqwest::Client::new(),
                crate::config::default_retry_policy_config(),
            )
            .with_circuit_breakers(Arc::clone(&breakers)),
        );
        let fetcher = JwksFetcher::new(
            JwksFetcherConfig {
                ttl: Duration::from_hours(1),
                stale_ttl: Duration::from_hours(24),
                max_entries: 10,
                refresh_on_unknown_kid: true,
                refresh_min_interval: Duration::from_secs(30),
            },
            JwksFetcherDeps {
                discovery,
                client: reqwest::Client::new(),
                metrics,
                retry_policy: crate::config::default_retry_policy_config(),
            },
        )
        .with_circuit_breakers(Arc::clone(&breakers));

        let result = fetcher
            .get_jwks(discovery_base.as_str(), &discovery_base)
            .await;

        assert!(matches!(result, Err(AuthNError::IdpUnreachable)));
        assert_eq!(
            breakers.state_for_host(&host_key(&jwks_uri)),
            Some(STATE_OPEN),
            "JWKS endpoint failure should open the breaker keyed by jwks_uri"
        );
        assert_eq!(
            breakers.state_for_host(&host_key(&discovery_base)),
            Some(STATE_CLOSED),
            "successful discovery must not be opened by a later JWKS endpoint failure"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_has_cached_entry_false_initially() {
        let fetcher = make_fetcher();
        assert!(!fetcher.has_cached_entry(TEST_ISSUER));
    }

    #[tokio::test]
    async fn test_has_cached_entry_true_after_injection() {
        let fetcher = make_fetcher();
        let fake_jwks = parse_jwks(r#"{"keys":[]}"#);
        fetcher.inject_cached_jwks(TEST_ISSUER, fake_jwks);
        assert!(fetcher.has_cached_entry(TEST_ISSUER));
    }

    #[tokio::test]
    async fn test_force_refresh_fails_without_reachable_idp() {
        let fetcher = make_fetcher();
        let discovery_base = test_issuer_url();
        let result = fetcher.force_refresh(TEST_ISSUER, &discovery_base).await;
        assert!(
            matches!(result, Err(AuthNError::IdpUnreachable)),
            "force_refresh with unreachable IdP should return IdpUnreachable"
        );
    }

    #[tokio::test]
    async fn force_refresh_respects_disabled_unknown_kid_refresh() {
        let fetcher = make_fetcher_with_policy(3600, 86_400, false, 30);
        let discovery_base = test_issuer_url();

        let result = fetcher.force_refresh(TEST_ISSUER, &discovery_base).await;

        assert!(
            matches!(result, Err(AuthNError::KidNotFound)),
            "disabled unknown-kid refresh should not call the IdP: {result:?}"
        );
    }

    #[tokio::test]
    async fn concurrent_force_refresh_waits_for_in_flight_refresh() -> anyhow::Result<()> {
        let server = MockServer::start();
        let discovery_base_raw = server.url("/realms/platform");
        let discovery_base = Url::parse(&discovery_base_raw)?;
        let jwks_uri_raw = server.url("/keys");
        let fresh_jwks = cached_jwks_with_kid("fresh-kid");
        let fresh_jwks_body = serde_json::to_string(&fresh_jwks)?;
        let jwks_calls = Arc::new(AtomicUsize::new(0));
        let (request_started_tx, request_started_rx) = mpsc::channel();
        let (release_response_tx, release_response_rx) = mpsc::channel();
        let release_response_rx = Arc::new(std::sync::Mutex::new(Some(release_response_rx)));

        let discovery_body = json!({
            "issuer": discovery_base_raw.clone(),
            "jwks_uri": jwks_uri_raw,
        });
        server.mock(move |when, then| {
            when.method(GET)
                .path("/realms/platform/.well-known/openid-configuration");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(discovery_body);
        });

        let jwks_calls_for_mock = Arc::clone(&jwks_calls);
        let release_response_for_mock = Arc::clone(&release_response_rx);
        server.mock(move |when, then| {
            when.method(GET).path("/keys");
            let fresh_jwks_body = fresh_jwks_body.clone();
            let request_started_tx = request_started_tx.clone();
            then.respond_with(move |_| {
                if jwks_calls_for_mock.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
                    match request_started_tx.send(()) {
                        Ok(()) | Err(_) => {}
                    }
                    if let Some(release_response_rx) =
                        release_response_for_mock.lock().unwrap().take()
                    {
                        match release_response_rx.recv() {
                            Ok(()) | Err(_) => {}
                        }
                    }
                }
                HttpMockResponse::builder()
                    .status(200)
                    .header("content-type", "application/json")
                    .body(fresh_jwks_body.clone())
                    .build()
            });
        });

        let discovery = Arc::new(OidcDiscovery::new_allowing_insecure_http_for_tests(
            3600,
            10,
            reqwest::Client::new(),
            crate::config::default_retry_policy_config(),
        ));
        let fetcher = Arc::new(make_fetcher_with_deps(
            JwksFetcherConfig {
                ttl: Duration::from_hours(1),
                stale_ttl: Duration::from_hours(24),
                max_entries: 10,
                refresh_on_unknown_kid: true,
                refresh_min_interval: Duration::from_secs(30),
            },
            discovery,
            MetricsHarness::new().metrics(),
        ));
        fetcher.inject_cached_jwks(discovery_base.as_str(), cached_jwks_with_kid("old-kid"));

        let first_fetcher = Arc::clone(&fetcher);
        let first_discovery_base = discovery_base.clone();
        let first = tokio::spawn(async move {
            first_fetcher
                .force_refresh(first_discovery_base.as_str(), &first_discovery_base)
                .await
        });
        tokio::task::spawn_blocking(move || {
            request_started_rx.recv_timeout(Duration::from_secs(2))
        })
        .await??;

        let second_fetcher = Arc::clone(&fetcher);
        let second_discovery_base = discovery_base.clone();
        let mut second = tokio::spawn(async move {
            second_fetcher
                .force_refresh(second_discovery_base.as_str(), &second_discovery_base)
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut second)
                .await
                .is_err(),
            "concurrent force_refresh should wait for the in-flight refresh"
        );

        release_response_tx.send(())?;
        let first_jwks = first.await??;
        let second_jwks = second.await??;

        assert_eq!(first_kid(&first_jwks), Some("fresh-kid"));
        assert_eq!(
            first_kid(&second_jwks),
            Some("fresh-kid"),
            "waiter should observe the refreshed JWKS, not the stale cached key"
        );
        assert_eq!(
            jwks_calls.load(AtomicOrdering::SeqCst),
            1,
            "concurrent force refreshes should share one JWKS endpoint call"
        );
        Ok(())
    }

    #[tokio::test]
    async fn zero_refresh_min_interval_allows_immediate_retry() {
        let fetcher = make_fetcher_with_policy(3600, 86_400, true, 0);
        let discovery_base = test_issuer_url();

        let first = fetcher.force_refresh(TEST_ISSUER, &discovery_base).await;
        let second = fetcher.force_refresh(TEST_ISSUER, &discovery_base).await;

        assert!(matches!(first, Err(AuthNError::IdpUnreachable)));
        assert!(
            matches!(second, Err(AuthNError::IdpUnreachable)),
            "zero refresh interval should not trip the cooldown path: {second:?}"
        );
    }

    #[tokio::test]
    async fn test_cache_respects_max_entries() {
        let discovery = Arc::new(OidcDiscovery::new(
            3600,
            10,
            reqwest::Client::new(),
            crate::config::default_retry_policy_config(),
        ));
        let fetcher = make_fetcher_with_deps(
            JwksFetcherConfig {
                ttl: Duration::from_hours(1),
                stale_ttl: Duration::from_hours(24),
                max_entries: 2,
                refresh_on_unknown_kid: true,
                refresh_min_interval: Duration::from_secs(30),
            },
            discovery,
            MetricsHarness::new().metrics(),
        );

        let jwks = parse_jwks(r#"{"keys":[]}"#);
        fetcher.inject_cached_jwks("https://issuer1.example.com/realm", jwks.clone());
        fetcher.inject_cached_jwks("https://issuer2.example.com/realm", jwks);

        // Both entries should exist
        assert!(fetcher.has_cached_entry("https://issuer1.example.com/realm"));
        assert!(fetcher.has_cached_entry("https://issuer2.example.com/realm"));
        // Total at capacity
        assert_eq!(fetcher.cache.len(), 2);
    }
}
