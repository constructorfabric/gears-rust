//! Configuration for the REST `usage-collector-client` module.

use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use tracing::warn;
use url::{Host, Url};
use usage_emitter::UsageEmitterConfig;

/// `OAuth2` client credentials and scope configuration.
#[derive(Clone, Deserialize, modkit_macros::ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct OAuthConfig {
    /// `OAuth2` client identifier for s2s authentication.
    pub client_id: String,

    /// `OAuth2` client secret for s2s authentication.
    #[expand_vars]
    pub client_secret: SecretString,

    /// `OAuth2` scopes to request (empty = `IdP` default scopes).
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl fmt::Debug for OAuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthConfig")
            .field("client_id", &"[REDACTED]")
            .field("client_secret", &"[REDACTED]")
            .field("scopes", &self.scopes)
            .finish()
    }
}

/// Module configuration.
#[derive(Clone, Deserialize, modkit_macros::ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct UsageCollectorRestClientConfig {
    /// URL of the usage-collector REST service.
    pub collector_url: Url,

    /// `OAuth2` credentials and scope configuration.
    #[expand_vars]
    pub oauth: OAuthConfig,

    /// Outbox/authorization tuning for the embedded usage emitter.
    #[serde(default)]
    pub emitter: UsageEmitterConfig,
}

impl fmt::Debug for UsageCollectorRestClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UsageCollectorRestClientConfig")
            .field("collector_url", &self.collector_url.as_str())
            .field("oauth", &self.oauth)
            .field("emitter", &self.emitter)
            .finish()
    }
}

impl UsageCollectorRestClientConfig {
    /// Validates the configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if `collector_url` is not a hierarchical URL, or if
    /// `client_id` / `client_secret` are empty or whitespace-only.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.collector_url.cannot_be_a_base() {
            anyhow::bail!(
                "collector_url must be a hierarchical URL with a host \
                 (e.g. https://host:port), got: {}",
                self.collector_url
            );
        }
        if self.oauth.client_id.trim().is_empty() {
            anyhow::bail!("client_id must not be empty");
        }
        if self.oauth.client_secret.expose_secret().trim().is_empty() {
            anyhow::bail!("client_secret must not be empty");
        }
        // @cpt-dod:cpt-cf-dod-rest-ingest-tls-config:p1
        // @cpt-begin:cpt-cf-dod-rest-ingest-tls-config:p1:inst-tls-check
        if is_insecure_non_loopback_http(&self.collector_url) {
            warn!(
                %self.collector_url,
                "collector_url uses http:// with a non-localhost host \u{2014} use https:// in production for secure transport",
            );
        }
        // @cpt-end:cpt-cf-dod-rest-ingest-tls-config:p1:inst-tls-check
        Ok(())
    }
}

/// Returns `true` when `url` uses the `http://` scheme with a host that
/// is **not** a loopback address.
///
/// Loopback hosts recognised here:
/// - any IPv4 in `127.0.0.0/8` and the IPv6 `::1`
/// - `localhost` (case-insensitive), with or without a trailing dot
/// - any subdomain of `localhost` per RFC 6761 § 6.3 (e.g. `foo.localhost`)
///
/// This is used by the module initialisation to decide whether to emit a
/// `WARN`-level log message about insecure transport configuration
/// (`cpt-cf-dod-rest-ingest-tls-config`).
fn is_insecure_non_loopback_http(url: &Url) -> bool {
    if url.scheme() != "http" {
        return false;
    }
    match url.host() {
        Some(Host::Ipv4(address)) => !address.is_loopback(),
        Some(Host::Ipv6(address)) => !address.is_loopback(),
        Some(Host::Domain(domain)) => !is_localhost_domain(domain),
        None => true,
    }
}

/// `true` when `domain` is `localhost` or a subdomain of it, per RFC 6761 § 6.3.
/// Comparison is case-insensitive and tolerates a single trailing dot (FQDN form).
fn is_localhost_domain(domain: &str) -> bool {
    let trimmed = domain.strip_suffix('.').unwrap_or(domain);
    trimmed.eq_ignore_ascii_case("localhost")
        || trimmed
            .rsplit_once('.')
            .is_some_and(|(_, tld)| tld.eq_ignore_ascii_case("localhost"))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
