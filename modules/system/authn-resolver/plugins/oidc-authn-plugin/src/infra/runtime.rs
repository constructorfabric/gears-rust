//! Runtime wiring for HTTP-backed OIDC plugin infrastructure.

use std::sync::Arc;
use std::time::Duration;

use crate::config::{IssuerTrustConfig, JwtValidationConfig, OidcPluginConfig};
use crate::domain::authenticate::{OidcAuthNPlugin, OidcAuthNPluginBuilder};
use crate::domain::metrics::AuthNMetrics;
use crate::infra::circuit_breaker::HostCircuitBreakers;
use crate::infra::jwks::{JwksFetcher, JwksFetcherConfig, JwksFetcherDeps};
use crate::infra::oidc::OidcDiscovery;
use crate::infra::token_client::TokenClient;
use crate::infra::url_policy::UrlSecurityPolicy;

/// Build the domain plugin with HTTP-backed infrastructure adapters.
#[must_use]
pub fn build_oidc_authn_plugin(
    jwt_config: JwtValidationConfig,
    issuer_trust: IssuerTrustConfig,
    plugin_config: OidcPluginConfig,
    http_client: reqwest::Client,
    metrics: Arc<AuthNMetrics>,
) -> OidcAuthNPlugin {
    build_oidc_authn_plugin_with_url_policy(
        jwt_config,
        issuer_trust,
        plugin_config,
        http_client,
        metrics,
        UrlSecurityPolicy::STRICT,
    )
}

/// Build the domain plugin while permitting plain HTTP IdP URLs.
#[doc(hidden)]
#[must_use]
pub fn build_oidc_authn_plugin_allowing_insecure_http_for_tests(
    jwt_config: JwtValidationConfig,
    issuer_trust: IssuerTrustConfig,
    plugin_config: OidcPluginConfig,
    http_client: reqwest::Client,
    metrics: Arc<AuthNMetrics>,
) -> OidcAuthNPlugin {
    build_oidc_authn_plugin_with_url_policy(
        jwt_config,
        issuer_trust,
        plugin_config,
        http_client,
        metrics,
        UrlSecurityPolicy::allow_insecure_http_for_tests(),
    )
}

fn build_oidc_authn_plugin_with_url_policy(
    jwt_config: JwtValidationConfig,
    issuer_trust: IssuerTrustConfig,
    plugin_config: OidcPluginConfig,
    http_client: reqwest::Client,
    metrics: Arc<AuthNMetrics>,
    url_policy: UrlSecurityPolicy,
) -> OidcAuthNPlugin {
    let circuit_breakers = plugin_config.circuit_breaker.as_ref().map(|config| {
        Arc::new(HostCircuitBreakers::new(
            config.failure_threshold,
            config.reset_timeout_secs,
            Arc::clone(&metrics),
        ))
    });

    let retry_policy = plugin_config.retry_policy;
    let mut discovery = OidcDiscovery::new_with_url_policy(
        jwt_config.discovery_cache_ttl_secs,
        jwt_config.discovery_max_entries,
        http_client.clone(),
        retry_policy.clone(),
        url_policy,
    );
    if let Some(breakers) = &circuit_breakers {
        discovery = discovery.with_circuit_breakers(Arc::clone(breakers));
    }
    let discovery = Arc::new(discovery);

    let mut token_client = TokenClient::new(
        http_client.clone(),
        Arc::clone(&discovery),
        plugin_config.s2s,
        retry_policy.clone(),
    );
    if let Some(breakers) = &circuit_breakers {
        token_client = token_client.with_circuit_breakers(Arc::clone(breakers));
    }

    let jwks_config = JwksFetcherConfig {
        ttl: Duration::from_secs(jwt_config.jwks_cache_ttl_secs),
        stale_ttl: Duration::from_secs(jwt_config.jwks_stale_ttl_secs),
        max_entries: jwt_config.jwks_max_entries,
        refresh_on_unknown_kid: jwt_config.jwks_refresh_on_unknown_kid,
        refresh_min_interval: Duration::from_secs(jwt_config.jwks_refresh_min_interval_secs),
    };
    let jwks_deps = JwksFetcherDeps {
        discovery,
        client: http_client,
        metrics: Arc::clone(&metrics),
        retry_policy,
    };
    let mut fetcher = JwksFetcher::new(jwks_config, jwks_deps);
    if let Some(breakers) = circuit_breakers {
        fetcher = fetcher.with_circuit_breakers(breakers);
    }

    let jwks_provider = Arc::new(fetcher);
    let token_exchanger = Arc::new(token_client);

    OidcAuthNPluginBuilder::new(
        jwt_config,
        issuer_trust,
        plugin_config.s2s_default_subject_type,
    )
    .claim_mapper_config(plugin_config.claim_mapper)
    .s2s_claim_mapper_config(plugin_config.s2s_claim_mapper)
    .claim_mapper_options(plugin_config.claim_mapper_options)
    .build(jwks_provider, token_exchanger, metrics)
}
