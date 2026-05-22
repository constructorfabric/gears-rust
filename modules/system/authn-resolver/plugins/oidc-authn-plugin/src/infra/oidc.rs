//! OIDC Discovery component.
//!
//! Fetches and caches the `OpenID` Connect discovery document from
//! `{issuer}/.well-known/openid-configuration` to resolve the `jwks_uri`.
//!
//! The discovery document is cached in memory with a configurable TTL
//! (default 1 hour) for up to a configurable number of issuers (default 10).

use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::Url;
use serde::Deserialize;
use tracing::{debug, info, instrument, warn};

use crate::config::RetryPolicyConfig;
use crate::domain::error::AuthNError;
use crate::infra::circuit_breaker::{HostCircuitBreakers, host_key};
use crate::infra::http_response::read_json_response_limited;
use crate::infra::retry::{RetriedRequestError, send_with_retry};
use crate::infra::ttl_cache::{Timestamped, TtlCache};
use crate::infra::url_policy::UrlSecurityPolicy;

/// The subset of the OIDC Discovery document we care about.
#[derive(Debug, Deserialize)]
struct OidcDiscoveryDocument {
    /// The issuer identifier from the discovery document.
    issuer: String,
    /// URI pointing to the JWKS (JSON Web Key Set) endpoint.
    jwks_uri: String,
    /// URI of the `OAuth2` token endpoint (used for S2S client credentials exchange).
    token_endpoint: Option<String>,
}

/// The subset of the OIDC Discovery document we care about.
#[derive(Debug, Clone)]
pub struct OidcConfig {
    /// The issuer identifier from the discovery document.
    pub issuer: String,
    /// Parsed JWKS endpoint, validated once when discovery metadata is accepted.
    jwks_url: Url,
    /// Parsed token endpoint, validated once when discovery metadata is accepted.
    token_endpoint_url: Option<Url>,
}

impl OidcConfig {
    /// Return the validated JWKS endpoint URL.
    #[must_use]
    pub fn jwks_url(&self) -> &Url {
        &self.jwks_url
    }

    /// Return the validated token endpoint URL when discovery metadata provides one.
    #[must_use]
    pub fn token_endpoint_url(&self) -> Option<&Url> {
        self.token_endpoint_url.as_ref()
    }
}

/// A cached OIDC Discovery entry.
#[derive(Debug, Clone)]
pub(crate) struct CachedDiscovery {
    pub config: OidcConfig,
    pub fetched_at: Instant,
}

impl Timestamped for CachedDiscovery {
    fn fetched_at(&self) -> Instant {
        self.fetched_at
    }
}

/// In-memory OIDC Discovery cache with TTL and max-entry eviction.
///
/// Thread-safe via [`TtlCache`]. Fetches are performed lazily on cache miss.
pub struct OidcDiscovery {
    // Debug is implemented manually below to show cache size without contents.
    cache: TtlCache<CachedDiscovery>,
    client: reqwest::Client,
    retry_policy: RetryPolicyConfig,
    circuit_breakers: Option<Arc<HostCircuitBreakers>>,
    url_policy: UrlSecurityPolicy,
}

impl std::fmt::Debug for OidcDiscovery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcDiscovery")
            .field("cached_issuers", &self.cache.len())
            .finish_non_exhaustive()
    }
}

impl OidcDiscovery {
    /// Create a new `OidcDiscovery` with the given TTL and max entry count.
    #[must_use]
    pub fn new(
        ttl_secs: u64,
        max_entries: usize,
        client: reqwest::Client,
        retry_policy: RetryPolicyConfig,
    ) -> Self {
        Self::new_with_url_policy(
            ttl_secs,
            max_entries,
            client,
            retry_policy,
            UrlSecurityPolicy::STRICT,
        )
    }

    pub(crate) fn new_with_url_policy(
        ttl_secs: u64,
        max_entries: usize,
        client: reqwest::Client,
        retry_policy: RetryPolicyConfig,
        url_policy: UrlSecurityPolicy,
    ) -> Self {
        Self {
            cache: TtlCache::new(Duration::from_secs(ttl_secs), max_entries),
            client,
            retry_policy,
            circuit_breakers: None,
            url_policy,
        }
    }

    /// Create a new `OidcDiscovery` that permits plain HTTP IdP URLs.
    #[doc(hidden)]
    #[must_use]
    pub fn new_allowing_insecure_http_for_tests(
        ttl_secs: u64,
        max_entries: usize,
        client: reqwest::Client,
        retry_policy: RetryPolicyConfig,
    ) -> Self {
        Self::new_with_url_policy(
            ttl_secs,
            max_entries,
            client,
            retry_policy,
            UrlSecurityPolicy::allow_insecure_http_for_tests(),
        )
    }

    /// Attach host-scoped circuit breakers for discovery network calls.
    #[must_use]
    pub fn with_circuit_breakers(mut self, circuit_breakers: Arc<HostCircuitBreakers>) -> Self {
        self.circuit_breakers = Some(circuit_breakers);
        self
    }

    /// Fetch the OIDC configuration for the given discovery base URL.
    ///
    /// Returns the cached config if available and not expired. Otherwise
    /// fetches from `{discovery_base}/.well-known/openid-configuration`.
    ///
    /// # Errors
    ///
    /// Returns [`AuthNError::IdpUnreachable`] if the HTTP request fails.
    #[instrument(skip(self))]
    pub async fn get_config(&self, discovery_base: &Url) -> Result<OidcConfig, AuthNError> {
        let discovery_base_key = discovery_base.as_str();
        if let Some(entry) = self.cache.get_fresh(discovery_base_key) {
            debug!(discovery_base = %discovery_base, "OIDC discovery cache hit");
            return Ok(entry.config);
        }

        info!(discovery_base = %discovery_base, "OIDC discovery cache miss or stale, fetching");

        let discovery_url = self.discovery_document_url(discovery_base)?;

        if let Some(circuit_breakers) = &self.circuit_breakers {
            let host = host_key(&discovery_url);
            circuit_breakers
                .call(&host, || async {
                    self.fetch_and_cache(discovery_base_key, &discovery_url)
                        .await
                })
                .await
        } else {
            self.fetch_and_cache(discovery_base_key, &discovery_url)
                .await
        }
    }

    /// Unconditionally fetch and cache the OIDC config for the given discovery base.
    async fn fetch_and_cache(
        &self,
        discovery_base_key: &str,
        discovery_url: &Url,
    ) -> Result<OidcConfig, AuthNError> {
        let response = send_with_retry(&self.retry_policy, || {
            self.client.get(discovery_url.clone()).send()
        })
        .await
        .map_err(|error| {
            match error {
                RetriedRequestError::Transport(e) => {
                    warn!(url = %discovery_url, error = %e, "OIDC discovery fetch failed");
                }
                RetriedRequestError::Status(status) => {
                    warn!(
                        url = %discovery_url,
                        status = %status,
                        "OIDC discovery returned non-success status"
                    );
                }
            }
            AuthNError::IdpUnreachable
        })?;

        let config: OidcDiscoveryDocument =
            read_json_response_limited(response).await.map_err(|e| {
                warn!(url = %discovery_url, error = %e, "OIDC discovery response parse failed");
                AuthNError::IdpUnreachable
            })?;
        let config = self.validate_config(config, discovery_url)?;

        self.cache.insert_with_eviction(
            discovery_base_key,
            CachedDiscovery {
                config: config.clone(),
                fetched_at: Instant::now(),
            },
            "OIDC discovery",
        );

        Ok(config)
    }

    fn discovery_document_url(&self, discovery_base: &Url) -> Result<Url, AuthNError> {
        self.url_policy
            .discovery_document_url(discovery_base, "OIDC discovery URL")
            .map_err(|error| {
                warn!(
                    discovery_base = %discovery_base,
                    error,
                    "OIDC discovery URL rejected by HTTPS policy"
                );
                AuthNError::IdpUnreachable
            })
    }

    fn validate_config(
        &self,
        config: OidcDiscoveryDocument,
        discovery_url: &Url,
    ) -> Result<OidcConfig, AuthNError> {
        let jwks_url = self
            .url_policy
            .validate_url(&config.jwks_uri, "OIDC jwks_uri")
            .map_err(|error| {
                warn!(
                    url = %discovery_url,
                    jwks_uri = %config.jwks_uri,
                    error,
                    "OIDC discovery jwks_uri rejected by HTTPS policy"
                );
                AuthNError::IdpUnreachable
            })?;

        let token_endpoint_url = if let Some(token_endpoint) = &config.token_endpoint {
            Some(
                self.url_policy
                    .validate_url(token_endpoint, "OIDC token_endpoint")
                    .map_err(|error| {
                        warn!(
                            url = %discovery_url,
                            token_endpoint,
                            error,
                            "OIDC discovery token_endpoint rejected by HTTPS policy"
                        );
                        AuthNError::IdpUnreachable
                    })?,
            )
        } else {
            None
        };

        Ok(OidcConfig {
            issuer: config.issuer,
            jwks_url,
            token_endpoint_url,
        })
    }

    /// Inject a discovery entry directly into the cache (for testing).
    #[cfg(test)]
    fn inject_cached_config(&self, issuer: &str, config: OidcConfig) {
        self.cache.insert(
            issuer,
            CachedDiscovery {
                config,
                fetched_at: Instant::now(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::metrics::test_harness::MetricsHarness;
    use crate::infra::circuit_breaker::{HostCircuitBreakers, STATE_OPEN, host_key};

    const TEST_ISSUER: &str = "http://127.0.0.1:19090/realms/platform";

    fn test_issuer_url() -> Url {
        Url::parse(TEST_ISSUER).expect("test issuer URL should parse")
    }

    fn make_discovery(max_entries: usize, ttl_secs: u64) -> OidcDiscovery {
        OidcDiscovery::new_allowing_insecure_http_for_tests(
            ttl_secs,
            max_entries,
            reqwest::Client::new(),
            crate::config::default_retry_policy_config(),
        )
    }

    fn fake_config(issuer: &str) -> OidcConfig {
        let discovery = make_discovery(10, 3600);
        let discovery_url = Url::parse("https://oidc.example.com/.well-known/openid-configuration")
            .expect("test discovery URL should parse");
        discovery
            .validate_config(
                OidcDiscoveryDocument {
                    issuer: issuer.to_owned(),
                    jwks_uri: format!("{issuer}/protocol/openid-connect/certs"),
                    token_endpoint: None,
                },
                &discovery_url,
            )
            .expect("fake config should satisfy test URL policy")
    }

    #[tokio::test]
    async fn cache_hit_returns_without_network() {
        let discovery = make_discovery(10, 3600);
        discovery.inject_cached_config(TEST_ISSUER, fake_config(TEST_ISSUER));
        let issuer = test_issuer_url();

        let result = discovery.get_config(&issuer).await;
        assert!(result.is_ok(), "cache hit should succeed");
        assert_eq!(result.unwrap().issuer, TEST_ISSUER);
    }

    #[tokio::test]
    async fn cold_cache_miss_returns_idp_unreachable() {
        let discovery = make_discovery(10, 3600);
        let issuer = test_issuer_url();

        let result = discovery.get_config(&issuer).await;
        assert!(
            matches!(result, Err(AuthNError::IdpUnreachable)),
            "cold cache miss with unreachable IdP should return IdpUnreachable: {result:?}"
        );
    }

    #[tokio::test]
    async fn strict_policy_rejects_http_discovery_url_before_network() {
        let discovery = OidcDiscovery::new(
            3600,
            10,
            reqwest::Client::new(),
            crate::config::default_retry_policy_config(),
        );
        let issuer = test_issuer_url();

        let result = discovery.get_config(&issuer).await;

        assert!(
            matches!(result, Err(AuthNError::IdpUnreachable)),
            "HTTP discovery URL should be rejected by strict policy: {result:?}"
        );
    }

    #[test]
    fn strict_policy_rejects_http_jwks_uri_from_discovery_metadata() {
        let discovery = OidcDiscovery::new(
            3600,
            10,
            reqwest::Client::new(),
            crate::config::default_retry_policy_config(),
        );
        let metadata = OidcDiscoveryDocument {
            issuer: "https://oidc.example.com/realms/platform".to_owned(),
            jwks_uri: "http://127.0.0.1:1/keys".to_owned(),
            token_endpoint: None,
        };
        let discovery_url = Url::parse("https://oidc.example.com/.well-known/openid-configuration")
            .expect("test discovery URL should parse");

        let result = discovery.validate_config(metadata, &discovery_url);

        assert!(
            matches!(result, Err(AuthNError::IdpUnreachable)),
            "HTTP JWKS URI should be rejected while accepting discovery metadata: {result:?}"
        );
    }

    #[tokio::test]
    async fn discovery_failure_opens_only_that_host_breaker() {
        let breakers = Arc::new(HostCircuitBreakers::new(
            1,
            30,
            MetricsHarness::new().metrics(),
        ));
        let discovery = OidcDiscovery::new_allowing_insecure_http_for_tests(
            3600,
            10,
            reqwest::Client::new(),
            crate::config::default_retry_policy_config(),
        )
        .with_circuit_breakers(Arc::clone(&breakers));
        let issuer = test_issuer_url();

        let result = discovery.get_config(&issuer).await;

        assert!(matches!(result, Err(AuthNError::IdpUnreachable)));
        assert_eq!(
            breakers.state_for_host(&host_key(&issuer)),
            Some(STATE_OPEN)
        );
        assert_eq!(
            breakers.state_for_host("unrelated.example.com"),
            None,
            "failing one discovery host must not create or open unrelated host breakers"
        );
    }

    #[tokio::test]
    async fn expired_entry_fails_closed_when_idp_unreachable() {
        let discovery = OidcDiscovery::new(
            0,
            10,
            reqwest::Client::new(),
            crate::config::default_retry_policy_config(),
        );
        discovery.inject_cached_config(TEST_ISSUER, fake_config(TEST_ISSUER));
        let issuer = test_issuer_url();

        let result = discovery.get_config(&issuer).await;
        assert!(
            matches!(result, Err(AuthNError::IdpUnreachable)),
            "expired entry with unreachable IdP should fail closed: {result:?}"
        );
    }

    #[tokio::test]
    async fn cold_cache_miss_with_no_stale_entry_returns_idp_unreachable() {
        let discovery = OidcDiscovery::new(
            0,
            10,
            reqwest::Client::new(),
            crate::config::default_retry_policy_config(),
        );
        let issuer = test_issuer_url();

        let result = discovery.get_config(&issuer).await;
        assert!(
            matches!(result, Err(AuthNError::IdpUnreachable)),
            "cold cache with unreachable IdP and no stale entry should return IdpUnreachable: {result:?}"
        );
    }
}
