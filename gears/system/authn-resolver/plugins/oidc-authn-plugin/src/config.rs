//! Configuration types for the Oidc `AuthN` plugin.

use std::time::Duration;

use anyhow::{Result, anyhow};
use jsonwebtoken::Algorithm;
use regex::Regex;
use reqwest::Url;
use serde::Deserialize;

use crate::domain::claim_mapper::{ClaimMapperConfig, ClaimMapperOptions};
use crate::infra::url_policy::UrlSecurityPolicy;

/// Stable plugin instance suffix used by `AuthN` resolver plugin selection.
pub const INSTANCE_SUFFIX: &str = "cf.builtin.oidc_authn_resolver.plugin.v1";

/// The plugin gear configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcAuthNGearConfig {
    /// Vendor key under which this plugin is discoverable.
    #[serde(default = "default_vendor")]
    pub vendor: String,
    /// Plugin priority used for vendor-level selection.
    #[serde(default = "default_priority")]
    pub priority: u32,
    /// JWT validation and claim-mapping settings.
    pub jwt: JwtConfigInput,
    /// OIDC discovery document cache settings.
    ///
    /// Controls cache TTL and maximum issuer entries.
    #[serde(default)]
    pub discovery_cache: DiscoveryCacheInput,
    /// JWKS cache settings.
    ///
    /// Controls key TTL/stale TTL, rotation refresh behavior, and cache capacity.
    #[serde(default)]
    pub jwks_cache: JwksCacheInput,
    /// Shared outbound HTTP client settings.
    ///
    /// Applies to discovery, JWKS, and S2S token exchange requests.
    #[serde(default)]
    pub http_client: HttpClientInput,
    /// Outbound retry behavior.
    ///
    /// Configures max retries, exponential backoff, and optional jitter.
    #[serde(default)]
    pub retry_policy: RetryPolicyInput,
    /// Outbound circuit-breaker settings.
    ///
    /// Tracks failures per host and controls open/half-open recovery timing.
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerInput,
    /// Service-to-service client-credentials exchange settings.
    ///
    /// Configures token endpoint discovery, claim mapping fallback, and token cache.
    pub s2s_oauth: S2sOauthInput,
}

fn default_vendor() -> String {
    "constructorfabric".to_owned()
}

fn default_priority() -> u32 {
    100
}

impl OidcAuthNGearConfig {
    /// Resolve raw gear input config into validated runtime config.
    ///
    /// # Errors
    ///
    /// Returns an error when duration strings are invalid, required
    /// audience configuration is missing, or retry/backoff bounds are invalid.
    #[allow(clippy::cognitive_complexity)]
    pub fn resolve(self) -> Result<ResolvedGearConfig> {
        self.resolve_with_url_policy(UrlSecurityPolicy::STRICT)
    }

    /// Resolve raw gear input config while permitting HTTP IdP URLs.
    #[doc(hidden)]
    pub fn resolve_allowing_insecure_http_for_tests(self) -> Result<ResolvedGearConfig> {
        self.resolve_with_url_policy(UrlSecurityPolicy::allow_insecure_http_for_tests())
    }

    fn resolve_with_url_policy(self, url_policy: UrlSecurityPolicy) -> Result<ResolvedGearConfig> {
        let Self {
            mut vendor,
            priority,
            jwt,
            discovery_cache,
            jwks_cache,
            circuit_breaker,
            retry_policy,
            mut s2s_oauth,
            http_client,
        } = self;

        vendor = vendor.trim().into();

        if vendor.is_empty() {
            return Err(anyhow!("`vendor` must not be empty"));
        }

        s2s_oauth.discovery_url = s2s_oauth.discovery_url.trim().into();
        s2s_oauth.default_subject_type = s2s_oauth.default_subject_type.trim().into();

        if s2s_oauth.discovery_url.is_empty() {
            return Err(anyhow!("`s2s_oauth.discovery_url` must not be empty"));
        }
        let s2s_discovery_url = url_policy
            .validate_oidc_base(&s2s_oauth.discovery_url, "`s2s_oauth.discovery_url`")
            .map_err(anyhow::Error::msg)?;

        if s2s_oauth.default_subject_type.is_empty() {
            return Err(anyhow!(
                "`s2s_oauth.default_subject_type` must not be empty"
            ));
        }

        let issuer_trust =
            IssuerTrustConfig::from_inputs_with_url_policy(jwt.trusted_issuers, url_policy)
                .map_err(|e| anyhow!("invalid `trusted_issuers` config: {e}"))?;

        let expected_audience = jwt
            .expected_audience
            .iter()
            .enumerate()
            .map(|(index, pattern)| MatcherCompiled::from_wildcard_pattern(pattern, index))
            .collect::<Result<Vec<_>>>()
            .map_err(|e| anyhow!("invalid `jwt.expected_audience`: {e}"))?;
        if expected_audience.is_empty() && !jwt.require_audience {
            tracing::warn!(
                "jwt.expected_audience is empty and jwt.require_audience is false; tokens with no audience claim will be accepted"
            );
        }
        let supported_algorithms = parse_supported_algorithms(&jwt.supported_algorithms)?;
        let clock_skew_leeway_secs = parse_duration_secs(&jwt.clock_skew_leeway)?;
        if clock_skew_leeway_secs > 300 {
            return Err(anyhow!("`jwt.clock_skew_leeway` must be <= 300s"));
        }

        let mut jwt_claim_mapping = jwt.claim_mapping;
        trim_subject_tenant_id(
            &mut jwt_claim_mapping.subject_tenant_id,
            "`jwt.claim_mapping.subject_tenant_id`",
        )?;
        let claim_mapper_options = ClaimMapperOptions {
            required_claims: jwt.required_claims,
            first_party_clients: jwt.first_party_clients,
        };
        let claim_mapper = ClaimMapperConfig {
            subject_id: jwt_claim_mapping.subject_id.clone(),
            subject_tenant_id: jwt_claim_mapping.subject_tenant_id.clone(),
            subject_type: jwt_claim_mapping.subject_type.clone(),
            token_scopes: jwt_claim_mapping.token_scopes.clone(),
        };

        let discovery_cache_ttl_secs = parse_duration_secs(&discovery_cache.ttl)?;
        let discovery_max_entries = discovery_cache.max_entries.max(1);

        let jwks_cache_ttl_secs = parse_duration_secs(&jwks_cache.ttl)?;
        let jwks_stale_ttl_secs = parse_duration_secs(&jwks_cache.stale_ttl)?;
        if jwks_stale_ttl_secs != 0 && jwks_stale_ttl_secs < jwks_cache_ttl_secs {
            return Err(anyhow!(
                "`jwks_cache.stale_ttl` must be 0s or >= `jwks_cache.ttl`"
            ));
        }
        let jwks_max_entries = jwks_cache.max_entries.max(1);
        let jwks_refresh_min_interval_secs = parse_duration_secs(&jwks_cache.refresh_min_interval)?;

        if circuit_breaker.failure_threshold == 0 {
            return Err(anyhow!("`circuit_breaker.failure_threshold` must be >= 1"));
        }
        let circuit_breaker_reset_timeout_secs =
            parse_duration_secs(&circuit_breaker.reset_timeout)?;
        if circuit_breaker_reset_timeout_secs == 0 {
            return Err(anyhow!("`circuit_breaker.reset_timeout` must be >= 1s"));
        }

        let circuit_breaker_config = if circuit_breaker.enabled {
            Some(CircuitBreakerConfig {
                failure_threshold: circuit_breaker.failure_threshold,
                reset_timeout_secs: circuit_breaker_reset_timeout_secs,
            })
        } else {
            None
        };

        let retry_policy_config = RetryPolicyConfig {
            max_attempts: retry_policy.max_attempts,
            initial_backoff_ms: parse_duration_millis(&retry_policy.initial_backoff)?,
            max_backoff_ms: parse_duration_millis(&retry_policy.max_backoff)?,
            jitter: retry_policy.jitter,
        };

        if retry_policy_config.initial_backoff_ms == 0
            || retry_policy_config.initial_backoff_ms > retry_policy_config.max_backoff_ms
        {
            return Err(anyhow!(
                "`retry_policy.initial_backoff` must be > 0 and <= `retry_policy.max_backoff`"
            ));
        }

        let mut s2s_claim_mapping = s2s_oauth.claim_mapping.unwrap_or(jwt_claim_mapping);
        trim_subject_tenant_id(
            &mut s2s_claim_mapping.subject_tenant_id,
            "`s2s_oauth.claim_mapping.subject_tenant_id`",
        )?;
        let s2s_claim_mapper = ClaimMapperConfig {
            subject_id: s2s_claim_mapping.subject_id,
            subject_tenant_id: s2s_claim_mapping.subject_tenant_id,
            subject_type: s2s_claim_mapping.subject_type,
            token_scopes: s2s_claim_mapping.token_scopes,
        };

        let s2s = S2sConfig {
            discovery_url: s2s_discovery_url,
            token_cache_ttl_secs: parse_duration_secs(&s2s_oauth.token_cache.ttl)?,
            token_cache_max_entries: s2s_oauth.token_cache.max_entries.max(1),
        };

        let HttpClientInput {
            request_timeout,
            custom_ca_certificate_paths,
        } = http_client;
        let request_timeout = humantime::parse_duration(&request_timeout)
            .map_err(|e| anyhow!("invalid http_client.request_timeout: {e}"))?;
        if request_timeout.is_zero() {
            return Err(anyhow!("http_client.request_timeout must be positive"));
        }
        let custom_ca_certificate_paths = custom_ca_certificate_paths
            .into_iter()
            .enumerate()
            .map(|(index, path)| {
                let path = path.trim().to_owned();
                if path.is_empty() {
                    return Err(anyhow!(
                        "`http_client.custom_ca_certificate_paths[{index}]` must not be empty"
                    ));
                }
                Ok(path)
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(ResolvedGearConfig {
            jwt_validation: JwtValidationConfig {
                supported_algorithms,
                clock_skew_leeway_secs,
                require_audience: jwt.require_audience,
                expected_audience,
                jwks_cache_ttl_secs,
                jwks_stale_ttl_secs,
                jwks_max_entries,
                jwks_refresh_on_unknown_kid: jwks_cache.refresh_on_unknown_kid,
                jwks_refresh_min_interval_secs,
                discovery_cache_ttl_secs,
                discovery_max_entries,
            },
            plugin: OidcPluginConfig {
                vendor,
                priority,
                claim_mapper,
                s2s_claim_mapper,
                claim_mapper_options,
                s2s_default_subject_type: s2s_oauth.default_subject_type,
                circuit_breaker: circuit_breaker_config,
                retry_policy: retry_policy_config,
                s2s,
            },
            issuer_trust,
            request_timeout,
            custom_ca_certificate_paths,
        })
    }
}

fn trim_subject_tenant_id(subject_tenant_id: &mut String, field_name: &str) -> Result<()> {
    *subject_tenant_id = subject_tenant_id.trim().to_owned();

    if subject_tenant_id.is_empty() {
        return Err(anyhow!("{field_name} must not be empty"));
    }

    Ok(())
}

#[derive(Debug, Clone)]
pub struct JwtValidationConfig {
    pub supported_algorithms: Vec<Algorithm>,
    pub clock_skew_leeway_secs: u64,
    pub require_audience: bool,
    pub expected_audience: Vec<MatcherCompiled>,
    pub jwks_cache_ttl_secs: u64,
    pub jwks_stale_ttl_secs: u64,
    pub jwks_max_entries: usize,
    pub jwks_refresh_on_unknown_kid: bool,
    pub jwks_refresh_min_interval_secs: u64,
    pub discovery_cache_ttl_secs: u64,
    pub discovery_max_entries: usize,
}

#[derive(Debug, Clone)]
struct IssuerRuleCompiled {
    matcher: MatcherCompiled,
    discovery_url: Option<DiscoveryUrlOverride>,
    url_policy: UrlSecurityPolicy,
    /// Per-issuer compiled audience matchers (empty ⇒ fall back to global).
    expected_audience: Vec<MatcherCompiled>,
    /// Required JOSE `typ` header, normalized to lowercase for case-insensitive
    /// comparison (`None` ⇒ `typ` not checked).
    jose_typ: Option<String>,
    /// Per-issuer clock-skew leeway in seconds (`None` ⇒ use global leeway).
    clock_skew_leeway_secs: Option<u64>,
}

#[derive(Debug, Clone)]
enum DiscoveryUrlOverride {
    Static(Url),
    Template(String),
}

#[derive(Debug, Clone)]
pub enum MatcherCompiled {
    Exact(String),
    Regex(Regex),
}

impl MatcherCompiled {
    pub(crate) fn from_wildcard_pattern(mut pattern_raw: &str, index: usize) -> Result<Self> {
        pattern_raw = pattern_raw.trim();

        if pattern_raw.is_empty() {
            return Err(anyhow!(
                "expected audience entry at index {index} has an empty value"
            ));
        }

        if pattern_raw.contains("**") {
            return Err(anyhow!(
                "expected audience entry at index {index} uses unsupported '**' wildcard"
            ));
        }

        if !pattern_raw.contains('*') {
            return Ok(Self::Exact(pattern_raw.to_owned()));
        }

        let escaped_parts = pattern_raw
            .split('*')
            .map(regex::escape)
            .collect::<Vec<_>>()
            .join(".*");

        Self::anchored_regex(&escaped_parts)
            .map_err(|e| anyhow!("invalid expected audience pattern at index {index}: {e}"))
    }

    pub(crate) fn anchored_regex(pattern: &str) -> Result<Self> {
        let anchored = format!("^{pattern}$");

        Regex::new(&anchored).map_err(Into::into).map(Self::Regex)
    }

    #[must_use]
    pub(crate) fn is_match(&self, issuer: &str) -> bool {
        match self {
            Self::Exact(expected) => expected == issuer,
            Self::Regex(pattern) => pattern.is_match(issuer),
        }
    }
}

/// Resolved per-issuer validation context for the first matching trusted-issuer
/// rule.
///
/// Carries the discovery base used to fetch JWKS plus the per-issuer validation
/// overrides (audience, JOSE `typ`, clock-skew leeway). Empty/`None` overrides
/// signal that the validator should fall back to the global configuration.
#[derive(Debug, Clone)]
pub struct ResolvedIssuer {
    /// Discovery base URL used to fetch this issuer's JWKS.
    pub discovery_base: Url,
    /// Per-issuer audience matchers (empty ⇒ use global `expected_audience`).
    pub expected_audience: Vec<MatcherCompiled>,
    /// Required JOSE `typ` header, lowercased (`None` ⇒ `typ` not checked).
    pub jose_typ: Option<String>,
    /// Per-issuer clock-skew leeway in seconds (`None` ⇒ use global leeway).
    pub clock_skew_leeway_secs: Option<u64>,
}

/// Outcome of evaluating one trusted-issuer rule against a token `iss`.
///
/// A matched rule is **terminal** (first match wins): the caller must not fall
/// through to later rules when a rule matched but its discovery URL is invalid.
enum IssuerMatch {
    /// The rule's matcher did not match `iss`.
    NoMatch,
    /// The matcher matched, but the discovery URL failed to resolve/validate.
    MatchedInvalid,
    /// The matcher matched and produced a usable per-issuer context.
    Matched(ResolvedIssuer),
}

impl IssuerRuleCompiled {
    fn from_input(
        TrustedIssuerInput {
            entry,
            discovery_url,
            expected_audience,
            jose_typ,
            clock_skew_leeway_secs,
        }: TrustedIssuerInput,
        index: usize,
        url_policy: UrlSecurityPolicy,
    ) -> Result<Self> {
        let matcher = match entry {
            TrustedIssuerEntry::Issuer(issuer) => {
                let exact = issuer.trim();

                if exact.is_empty() {
                    return Err(anyhow!(
                        "`trusted_issuers` entry at index {index} has an empty `issuer`"
                    ));
                }
                url_policy
                    .validate_oidc_base(
                        exact,
                        &format!("`trusted_issuers` entry at index {index} `issuer`"),
                    )
                    .map_err(anyhow::Error::msg)?;

                MatcherCompiled::Exact(exact.to_owned())
            }
            TrustedIssuerEntry::IssuerPattern(pattern_raw) => {
                let pattern = pattern_raw.trim();

                if pattern.is_empty() {
                    return Err(anyhow!(
                        "`trusted_issuers` entry at index {index} has an empty `issuer_pattern`"
                    ));
                }

                MatcherCompiled::anchored_regex(pattern).map_err(|e| {
                    anyhow!(
                        "invalid `issuer_pattern` in `trusted_issuers` entry at index {index}: {e}"
                    )
                })?
            }
        };

        let discovery_url = discovery_url
            .map(|value| {
                let value = value.trim().to_owned();
                if value.is_empty() {
                    return Err(anyhow!(
                        "`trusted_issuers` entry at index {index} has an empty `discovery_url`"
                    ));
                }
                if value.contains("{issuer}") {
                    Ok(DiscoveryUrlOverride::Template(value))
                } else {
                    let value = url_policy
                        .validate_oidc_base(
                            &value,
                            &format!("`trusted_issuers` entry at index {index} `discovery_url`"),
                        )
                        .map_err(anyhow::Error::msg)?;
                    Ok(DiscoveryUrlOverride::Static(value))
                }
            })
            .transpose()?;

        let expected_audience = expected_audience
            .iter()
            .enumerate()
            .map(|(audience_index, pattern)| {
                MatcherCompiled::from_wildcard_pattern(pattern, audience_index)
            })
            .collect::<Result<Vec<_>>>()
            .map_err(|e| {
                anyhow!(
                    "invalid `expected_audience` in `trusted_issuers` entry at index {index}: {e}"
                )
            })?;

        let jose_typ = jose_typ
            .map(|value| {
                let value = value.trim();
                if value.is_empty() {
                    return Err(anyhow!(
                        "`trusted_issuers` entry at index {index} has an empty `jose_typ`"
                    ));
                }
                Ok(value.to_ascii_lowercase())
            })
            .transpose()?;

        if let Some(leeway) = clock_skew_leeway_secs
            && leeway > 300
        {
            return Err(anyhow!(
                "`trusted_issuers` entry at index {index} `clock_skew_leeway_secs` must be <= 300"
            ));
        }

        Ok(Self {
            matcher,
            discovery_url,
            url_policy,
            expected_audience,
            jose_typ,
            clock_skew_leeway_secs,
        })
    }

    /// Resolve the discovery base URL for an already-matched rule. Returns
    /// `None` when the configured override (or the issuer itself) fails URL
    /// validation.
    fn resolve_discovery_base(&self, issuer: &str) -> Option<Url> {
        match &self.discovery_url {
            Some(DiscoveryUrlOverride::Static(url)) => Some(url.clone()),
            Some(DiscoveryUrlOverride::Template(value)) => {
                let discovery_url = value.replace("{issuer}", issuer);
                self.url_policy
                    .validate_oidc_base(&discovery_url, "resolved OIDC discovery URL")
                    .ok()
            }
            None => self
                .url_policy
                .validate_oidc_base(issuer, "resolved OIDC discovery URL")
                .ok(),
        }
    }

    /// Evaluate this rule against `issuer`. A matched rule is terminal: when its
    /// discovery URL fails validation the result is [`IssuerMatch::MatchedInvalid`]
    /// so the caller stops instead of falling through to later rules.
    fn resolve(&self, issuer: &str) -> IssuerMatch {
        if !self.matcher.is_match(issuer) {
            return IssuerMatch::NoMatch;
        }
        match self.resolve_discovery_base(issuer) {
            Some(discovery_base) => IssuerMatch::Matched(ResolvedIssuer {
                discovery_base,
                expected_audience: self.expected_audience.clone(),
                jose_typ: self.jose_typ.clone(),
                clock_skew_leeway_secs: self.clock_skew_leeway_secs,
            }),
            None => IssuerMatch::MatchedInvalid,
        }
    }
}

#[derive(Clone)]
pub struct IssuerTrustConfig {
    rule: IssuerRuleCompiled,
    remaining_rules: Vec<IssuerRuleCompiled>,
}

impl std::fmt::Debug for IssuerTrustConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuerTrustConfig")
            .field("rule_count", &self.rule_count())
            .finish()
    }
}

impl IssuerTrustConfig {
    /// Build runtime trust config from ordered trusted-issuer input entries.
    ///
    /// Rules are evaluated in declaration order and first match wins.
    ///
    /// # Errors
    ///
    /// Returns an error when the list is empty, an entry is empty, or a regex
    /// pattern cannot be compiled.
    pub fn from_inputs(inputs: impl IntoIterator<Item = TrustedIssuerInput>) -> Result<Self> {
        Self::from_inputs_with_url_policy(inputs, UrlSecurityPolicy::STRICT)
    }

    /// Build runtime trust config while permitting HTTP issuer/discovery URLs.
    #[doc(hidden)]
    pub fn from_inputs_allowing_insecure_http_for_tests(
        inputs: impl IntoIterator<Item = TrustedIssuerInput>,
    ) -> Result<Self> {
        Self::from_inputs_with_url_policy(
            inputs,
            UrlSecurityPolicy::allow_insecure_http_for_tests(),
        )
    }

    fn from_inputs_with_url_policy(
        inputs: impl IntoIterator<Item = TrustedIssuerInput>,
        url_policy: UrlSecurityPolicy,
    ) -> Result<Self> {
        let mut inputs = inputs.into_iter();

        let Some(rule) = inputs.next() else {
            anyhow::bail!("expected at least one trusted issuer");
        };

        Ok(Self {
            rule: IssuerRuleCompiled::from_input(rule, 0, url_policy)?,
            remaining_rules: inputs
                .enumerate()
                .map(|(index, input)| IssuerRuleCompiled::from_input(input, index + 1, url_policy))
                .collect::<Result<_, _>>()?,
        })
    }

    /// Build test trust config from exact issuer strings.
    ///
    /// This keeps unit tests concise while runtime callers use the gear input
    /// schema through [`Self::from_inputs`].
    ///
    /// # Errors
    ///
    /// Returns an error when an issuer string is empty.
    #[cfg(test)]
    pub fn from_exact_issuers(issuers: impl IntoIterator<Item = String>) -> Result<Self> {
        Self::from_inputs(issuers.into_iter().map(|issuer| TrustedIssuerInput {
            entry: TrustedIssuerEntry::Issuer(issuer),
            discovery_url: None,
            expected_audience: Vec::new(),
            jose_typ: None,
            clock_skew_leeway_secs: None,
        }))
    }

    /// Resolve the per-issuer validation context for the first trusted-issuer
    /// rule matching `issuer`.
    ///
    /// Returns the discovery base plus any per-issuer overrides (audience, JOSE
    /// `typ`, clock-skew leeway).
    #[must_use]
    pub fn resolve_issuer(&self, issuer: &str) -> Option<ResolvedIssuer> {
        // First match wins and is terminal: a rule that matches `issuer` but whose
        // discovery URL is invalid fails closed (returns `None`) rather than
        // falling through to a later — possibly more permissive — rule.
        for rule in std::iter::once(&self.rule).chain(&self.remaining_rules) {
            match rule.resolve(issuer) {
                IssuerMatch::NoMatch => {}
                IssuerMatch::MatchedInvalid => return None,
                IssuerMatch::Matched(resolved) => return Some(resolved),
            }
        }
        None
    }

    /// Resolve the discovery URL for the first trusted-issuer rule matching `issuer`.
    #[must_use]
    pub fn resolve(&self, issuer: &str) -> Option<Url> {
        self.resolve_issuer(issuer)
            .map(|resolved| resolved.discovery_base)
    }

    fn rule_count(&self) -> usize {
        1 + self.remaining_rules.len()
    }

    /// Returns `true` if the given issuer string is trusted.
    #[must_use]
    pub fn is_trusted(&self, issuer: &str) -> bool {
        self.resolve_issuer(issuer).is_some()
    }
}

#[derive(Debug, Clone)]
pub struct S2sConfig {
    pub discovery_url: Url,
    pub token_cache_ttl_secs: u64,
    pub token_cache_max_entries: usize,
}

/// Circuit-breaker tuning knobs for `IdP` calls.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive `IdP` failures before opening the circuit.
    pub failure_threshold: u32,
    /// Seconds the circuit remains open before allowing a single probe.
    pub reset_timeout_secs: u64,
}

#[derive(Debug, Clone)]
pub struct RetryPolicyConfig {
    pub max_attempts: u32,
    pub initial_backoff_ms: u64,
    pub max_backoff_ms: u64,
    pub jitter: bool,
}

#[cfg(test)]
#[must_use]
pub(crate) fn default_retry_policy_config() -> RetryPolicyConfig {
    RetryPolicyConfig {
        max_attempts: 3,
        initial_backoff_ms: 100,
        max_backoff_ms: 2_000,
        jitter: true,
    }
}

impl RetryPolicyConfig {
    #[must_use]
    pub fn initial_backoff(&self) -> Duration {
        Duration::from_millis(self.initial_backoff_ms)
    }

    #[must_use]
    pub fn max_backoff(&self) -> Duration {
        Duration::from_millis(self.max_backoff_ms)
    }
}

/// Runtime configuration used for `ClientHub` registration and claim mapping.
///
/// This config is intentionally focused on plugin registration concerns:
/// - `priority` controls plugin ordering when multiple implementations are available
/// - `claim_mapper` provides mapping behavior for normalized `SecurityContext` output
#[derive(Debug, Clone)]
pub struct OidcPluginConfig {
    /// Vendor key under which this plugin is discoverable.
    pub vendor: String,

    /// Plugin priority used for vendor-level selection.
    ///
    /// Higher values take precedence in this plugin's in-memory registration index.
    pub priority: u32,

    /// Claim mapper behavior used by the `AuthN` plugin boundary.
    pub claim_mapper: ClaimMapperConfig,

    /// Claim mapper behavior used for S2S client-credentials tokens.
    pub s2s_claim_mapper: ClaimMapperConfig,

    /// Claim options shared by bearer and S2S JWT mapping.
    pub claim_mapper_options: ClaimMapperOptions,

    /// S2S subject-type fallback applied after claim mapping.
    pub s2s_default_subject_type: String,

    /// Circuit-breaker behavior used for Oidc network operations.
    pub circuit_breaker: Option<CircuitBreakerConfig>,

    /// Retry behavior used for transient Oidc network failures.
    pub retry_policy: RetryPolicyConfig,

    /// S2S client credentials exchange configuration.
    pub s2s: S2sConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JwtConfigInput {
    /// Supported JWT signing algorithms for token verification.
    ///
    /// The `none` algorithm is always rejected.
    #[serde(default = "default_supported_algorithms")]
    pub supported_algorithms: Vec<String>,
    /// Optional clock skew allowance for time-based claims (`exp`, `iat`).
    ///
    /// Parsed as [`humantime`] duration.
    ///
    /// Must be less than or equal to `300s`.
    #[serde(default = "default_clock_skew_leeway")]
    pub clock_skew_leeway: String,
    /// Enables audience (`aud`) claim validation.
    ///
    /// When `true`, tokens without an `aud` claim are rejected.
    #[serde(default)]
    pub require_audience: bool,
    /// Allowed audience values used when an `aud` claim is present.
    ///
    /// Missing `aud` is accepted unless `require_audience` is enabled.
    /// Supports `*` as a substring wildcard.
    #[serde(default)]
    pub expected_audience: Vec<String>,
    /// Ordered issuer allowlist used to validate token `iss` values.
    ///
    /// First matching entry wins.
    #[serde(default)]
    pub trusted_issuers: Vec<TrustedIssuerInput>,
    /// JWT claim-name mapping for [`SecurityContext`](toolkit_security::SecurityContext) fields.
    pub claim_mapping: ClaimMappingInput,
    /// Client IDs treated as first-party and granted wildcard scopes (`["*"]`).
    #[serde(default)]
    pub first_party_clients: Vec<String>,
    /// Additional claims that must be present on top of mandatory defaults.
    #[serde(default)]
    pub required_claims: Vec<String>,
}

fn default_supported_algorithms() -> Vec<String> {
    ["RS256", "ES256"]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect()
}

fn default_clock_skew_leeway() -> String {
    "60s".to_owned()
}

fn parse_supported_algorithms(inputs: &[String]) -> Result<Vec<Algorithm>> {
    if inputs.is_empty() {
        return Err(anyhow!(
            "jwt.supported_algorithms must contain at least one algorithm"
        ));
    }

    inputs
        .iter()
        .enumerate()
        .map(|(index, algorithm)| match algorithm.as_str() {
            "RS256" => Ok(Algorithm::RS256),
            "ES256" => Ok(Algorithm::ES256),
            "none" | "NONE" | "None" => Err(anyhow!(
                "jwt.supported_algorithms[{index}] must not be none"
            )),
            other => Err(anyhow!(
                "unsupported jwt.supported_algorithms[{index}]: {other}; supported values are RS256 and ES256"
            )),
        })
        .collect()
}

/// One `jwt.trusted_issuers` list item from gear input config.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedIssuerInput {
    /// Issuer matcher (`issuer` or `issuer_pattern`).
    #[serde(flatten)]
    pub entry: TrustedIssuerEntry,
    /// Optional discovery base override for this matcher.
    ///
    /// When omitted, the matched JWT `iss` value is used.
    ///
    /// When provided, `"{issuer}"` placeholder is replaced with the matched issuer.
    #[serde(default)]
    pub discovery_url: Option<String>,
    /// Per-issuer audience override applied instead of the global
    /// `jwt.expected_audience` when non-empty.
    ///
    /// When empty, the global `jwt.expected_audience` behavior is used.
    /// Supports `*` as a substring wildcard (same syntax as the global list).
    #[serde(default)]
    pub expected_audience: Vec<String>,
    /// Required JOSE `typ` header value for tokens from this issuer.
    ///
    /// Matched case-insensitively (e.g. `"obo+jwt"`). When `None`, the `typ`
    /// header is not inspected, preserving default behavior for issuers that do
    /// not pin a token type.
    #[serde(default)]
    pub jose_typ: Option<String>,
    /// Per-issuer clock-skew leeway in seconds applied instead of the global
    /// `jwt.clock_skew_leeway` when present.
    ///
    /// When `None`, the global leeway is used.
    #[serde(default)]
    pub clock_skew_leeway_secs: Option<u64>,
}

/// Issuer matcher kind for a trusted-issuer entry.
///
/// - `{ issuer: "https://idp.example.com" }` for exact `iss` match
/// - `{ issuer_pattern: "^https://idp\\.example\\.com/realms/[^/]+$" }` for regex
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedIssuerEntry {
    Issuer(String),
    IssuerPattern(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimMappingInput {
    #[serde(default = "default_subject_id")]
    pub subject_id: String,
    #[serde(default)]
    pub subject_type: Option<String>,
    pub subject_tenant_id: String,
    #[serde(default = "default_token_scopes")]
    pub token_scopes: String,
}

fn default_token_scopes() -> String {
    "scope".to_owned()
}

fn default_subject_id() -> String {
    "sub".to_owned()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiscoveryCacheInput {
    /// Discovery cache TTL in [`humantime`] format.
    ///
    /// This controls how long OIDC discovery metadata remains fresh in memory.
    pub ttl: String,
    /// Maximum number of cached discovery entries (issuers).
    ///
    /// On overflow, least-recently-used (`LRU`) entries are evicted.
    pub max_entries: usize,
}

impl Default for DiscoveryCacheInput {
    fn default() -> Self {
        Self {
            ttl: "1h".to_owned(),
            max_entries: 10,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JwksCacheInput {
    /// JWKS cache fresh TTL in [`humantime`] format.
    ///
    /// Within this window, cached keys are considered fresh and used directly.
    pub ttl: String,
    /// Maximum number of cached JWKS entries (issuers).
    ///
    /// On overflow, least-recently-used (`LRU`) entries are evicted.
    pub max_entries: usize,
    /// Stale TTL in [`humantime`] format.
    ///
    /// Expired JWKS entries may still be served while the `IdP` is unreachable.
    /// Set to `0s` to disable stale-while-revalidate behavior.
    pub stale_ttl: String,
    /// Refresh JWKS immediately when token `kid` is not found in cache.
    ///
    /// This helps key-rotation convergence by forcing an out-of-band refresh.
    pub refresh_on_unknown_kid: bool,
    /// Minimum interval in [`humantime`] format between refresh attempts
    /// for the same issuer.
    ///
    /// Acts as a per-issuer `DoS` guard to avoid refresh storms on repeated
    /// unknown-`kid` tokens.
    pub refresh_min_interval: String,
}

impl Default for JwksCacheInput {
    fn default() -> Self {
        Self {
            ttl: "1h".to_owned(),
            max_entries: 10,
            stale_ttl: "24h".to_owned(),
            refresh_on_unknown_kid: true,
            refresh_min_interval: "30s".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpClientInput {
    /// Per-attempt timeout for outbound HTTP calls in [`humantime`] format.
    ///
    /// Applies to OIDC discovery, JWKS fetch/refresh, and S2S token exchange requests.
    pub request_timeout: String,
    /// PEM-encoded root CA certificate bundle file paths.
    ///
    /// Each file may contain one or more X.509 CA certificates. The certificates
    /// are added to the shared outbound HTTP client trust store in addition to
    /// the platform/default roots and apply to OIDC discovery, JWKS fetch/refresh,
    /// and S2S token exchange requests.
    pub custom_ca_certificate_paths: Vec<String>,
}

impl Default for HttpClientInput {
    fn default() -> Self {
        Self {
            request_timeout: "5s".to_owned(),
            custom_ca_certificate_paths: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetryPolicyInput {
    /// Maximum number of retries after the initial outbound attempt.
    ///
    /// `0` disables retries.
    pub max_attempts: u32,
    /// Initial retry delay in [`humantime`] format.
    ///
    /// Backoff grows exponentially (doubling each retry) from this value and is
    /// capped by `max_backoff`.
    pub initial_backoff: String,
    /// Upper bound for computed retry backoff in
    /// [`humantime`] format.
    pub max_backoff: String,
    /// Enables full jitter for retry delays.
    ///
    /// When `true`, each delay is randomized in `[0, computed_backoff]`.
    ///
    /// When `false`, the computed backoff is used as-is.
    pub jitter: bool,
}

impl Default for RetryPolicyInput {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: "100ms".to_owned(),
            max_backoff: "2s".to_owned(),
            jitter: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CircuitBreakerInput {
    /// Global circuit-breaker toggle for outbound HTTP calls.
    ///
    /// When `false`, breaker transitions are disabled (pass-through mode) while
    /// timeout and retry policy still apply.
    pub enabled: bool,
    /// Consecutive failures per host required to open that host's circuit.
    ///
    /// Must be greater than or equal to `1`.
    pub failure_threshold: u32,
    /// Duration in [`humantime`] format before an open
    /// host circuit transitions to half-open.
    pub reset_timeout: String,
}

impl Default for CircuitBreakerInput {
    fn default() -> Self {
        Self {
            enabled: true,
            failure_threshold: 5,
            reset_timeout: "30s".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S2sOauthInput {
    /// OIDC discovery base URL used to resolve `token_endpoint` for S2S exchange.
    ///
    /// The plugin fetches `{discovery_url}/.well-known/openid-configuration` (or
    /// uses `discovery_url` as-is when it already points to the discovery
    /// document). The returned `issuer` must be allowed by `jwt.trusted_issuers`,
    /// otherwise token endpoint resolution fails before credentials are posted.
    pub discovery_url: String,
    /// Optional claim mapping override for tokens obtained via client-credentials.
    ///
    /// When omitted, top-level `jwt.claim_mapping` is reused.
    #[serde(default)]
    pub claim_mapping: Option<ClaimMappingInput>,
    /// Fallback subject type for S2S tokens that do not include a subject-type claim.
    ///
    /// Applied only when mapped `subject_type` is absent after claim extraction.
    pub default_subject_type: String,
    /// Cache policy for validated S2S exchange results.
    #[serde(default)]
    pub token_cache: TokenCacheInput,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TokenCacheInput {
    /// Maximum cache TTL in [`humantime`] format.
    ///
    /// Effective entry TTL is `min(token.expires_in, ttl)`.
    pub ttl: String,
    /// Maximum number of cached S2S entries.
    ///
    /// On overflow, least-recently-used (`LRU`) entries are evicted.
    pub max_entries: usize,
}

impl Default for TokenCacheInput {
    fn default() -> Self {
        Self {
            ttl: "300s".to_owned(),
            max_entries: 100,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedGearConfig {
    /// JWT validation parameters (trusted issuers, cache sizes, audience).
    pub jwt_validation: JwtValidationConfig,
    /// Registration/claim-mapping/circuit-breaker plugin parameters.
    pub plugin: OidcPluginConfig,
    /// Ordered trusted-issuer matcher used by validator.
    pub issuer_trust: IssuerTrustConfig,
    /// Outbound request timeout per attempt.
    pub request_timeout: Duration,
    /// PEM-encoded root CA certificate bundle file paths.
    pub custom_ca_certificate_paths: Vec<String>,
}

fn parse_duration_secs(input: &str) -> Result<u64> {
    humantime::parse_duration(input)
        .map(|duration| duration.as_secs())
        .map_err(|error| anyhow!("invalid duration {input:?}: {error}"))
}

fn parse_duration_millis(input: &str) -> Result<u64> {
    let duration = humantime::parse_duration(input)
        .map_err(|error| anyhow!("invalid duration {input:?}: {error}"))?;
    u64::try_from(duration.as_millis())
        .map_err(|_| anyhow!("duration too large in milliseconds: {input:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_audience_exact_matcher_matches_literal_only() {
        let audience = MatcherCompiled::from_wildcard_pattern("cyber-fabric-api", 0)
            .expect("exact audience should compile");

        assert!(audience.is_match("cyber-fabric-api"));
        assert!(!audience.is_match("other-api"));
    }

    #[test]
    fn expected_audience_wildcard_matcher_uses_anchored_regex() {
        let audience = MatcherCompiled::from_wildcard_pattern("https://*.example.com/api", 0)
            .expect("wildcard audience should compile");

        assert!(audience.is_match("https://tenant.example.com/api"));
        assert!(!audience.is_match("prefix:https://tenant.example.com/api"));
        assert!(!audience.is_match("https://tenant.example.com/api:suffix"));
    }

    #[test]
    fn expected_audience_wildcard_escapes_regex_metacharacters() {
        let audience = MatcherCompiled::from_wildcard_pattern("audience?.*", 0)
            .expect("wildcard audience should compile");

        assert!(audience.is_match("audience?.prod"));
        assert!(!audience.is_match("audiences.prod"));
    }

    #[test]
    fn expected_audience_rejects_double_star_wildcard() {
        let result = MatcherCompiled::from_wildcard_pattern("api-**", 0);

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unsupported '**' wildcard"),
            "error should describe unsupported wildcard"
        );
    }
}

#[cfg(test)]
mod issuer_trust_config_tests {
    use super::*;

    fn exact_input(issuer: &str) -> TrustedIssuerInput {
        TrustedIssuerInput {
            entry: TrustedIssuerEntry::Issuer(issuer.to_owned()),
            discovery_url: None,
            expected_audience: Vec::new(),
            jose_typ: None,
            clock_skew_leeway_secs: None,
        }
    }

    fn pattern_input(pattern: &str) -> TrustedIssuerInput {
        TrustedIssuerInput {
            entry: TrustedIssuerEntry::IssuerPattern(pattern.to_owned()),
            discovery_url: None,
            expected_audience: Vec::new(),
            jose_typ: None,
            clock_skew_leeway_secs: None,
        }
    }

    #[test]
    fn exact_mode_trusts_literal_match() {
        let trust = IssuerTrustConfig::from_inputs(vec![exact_input(
            "https://oidc.example.com/realms/prod",
        )])
        .unwrap();
        assert!(trust.is_trusted("https://oidc.example.com/realms/prod"));
    }

    #[test]
    fn exact_mode_rejects_mismatch() {
        let trust = IssuerTrustConfig::from_inputs(vec![exact_input(
            "https://oidc.example.com/realms/prod",
        )])
        .unwrap();
        assert!(!trust.is_trusted("https://evil.example.com/realms/prod"));
    }

    #[test]
    fn regex_mode_full_match_pattern() {
        let trust = IssuerTrustConfig::from_inputs(vec![pattern_input(
            r"https://oidc\..*\.example\.com/realms/prod",
        )])
        .unwrap();
        assert!(trust.is_trusted("https://oidc.eu.example.com/realms/prod"));
        assert!(trust.is_trusted("https://oidc.us.example.com/realms/prod"));
    }

    #[test]
    fn regex_mode_rejects_non_matching_issuer() {
        let trust = IssuerTrustConfig::from_inputs(vec![pattern_input(
            r"https://oidc\..*\.example\.com/realms/prod",
        )])
        .unwrap();
        assert!(!trust.is_trusted("https://evil.example.com/realms/prod"));
    }

    #[test]
    fn regex_mode_matches_second_pattern() {
        let trust = IssuerTrustConfig::from_inputs(vec![
            pattern_input(r"https://first\.example\.com/realms/.*"),
            pattern_input(r"https://second\.example\.com/realms/.*"),
        ])
        .unwrap();
        assert!(trust.is_trusted("https://second.example.com/realms/prod"));
    }

    #[test]
    fn regex_mode_invalid_pattern_fails() {
        let result = IssuerTrustConfig::from_inputs(vec![pattern_input("(unclosed")]);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid `issuer_pattern`"),
            "error should describe the bad pattern"
        );
    }

    #[test]
    fn empty_issuers_fail_fast() {
        let error = IssuerTrustConfig::from_inputs(vec![])
            .expect_err("empty trusted issuer config should fail");

        assert!(
            error
                .to_string()
                .contains("expected at least one trusted issuer"),
            "error should describe the missing issuer trust root: {error}"
        );
    }

    #[test]
    fn discovery_url_placeholder_uses_matched_issuer() {
        let trust = IssuerTrustConfig::from_inputs(vec![TrustedIssuerInput {
            entry: TrustedIssuerEntry::IssuerPattern(
                r"https://idp\.example\.com/realms/[^/]+".to_owned(),
            ),
            discovery_url: Some("{issuer}".to_owned()),
            expected_audience: Vec::new(),
            jose_typ: None,
            clock_skew_leeway_secs: None,
        }])
        .unwrap();

        let discovery_base = trust
            .resolve("https://idp.example.com/realms/platform")
            .unwrap();
        assert_eq!(
            discovery_base.as_str(),
            "https://idp.example.com/realms/platform"
        );
    }

    #[test]
    fn discovery_url_override_is_trimmed_during_resolution() {
        let trust = IssuerTrustConfig::from_inputs(vec![TrustedIssuerInput {
            entry: TrustedIssuerEntry::Issuer("https://issuer.example.com".to_owned()),
            discovery_url: Some("  https://discovery.example.com  ".to_owned()),
            expected_audience: Vec::new(),
            jose_typ: None,
            clock_skew_leeway_secs: None,
        }])
        .unwrap();

        assert_eq!(
            trust
                .resolve("https://issuer.example.com")
                .unwrap()
                .as_str(),
            "https://discovery.example.com/"
        );
    }

    #[test]
    fn blank_discovery_url_override_fails_fast() {
        let error = IssuerTrustConfig::from_inputs(vec![TrustedIssuerInput {
            entry: TrustedIssuerEntry::Issuer("https://issuer.example.com".to_owned()),
            discovery_url: Some("   ".to_owned()),
            expected_audience: Vec::new(),
            jose_typ: None,
            clock_skew_leeway_secs: None,
        }])
        .expect_err("blank discovery URL override should fail");

        assert!(
            error
                .to_string()
                .contains("`trusted_issuers` entry at index 0 has an empty `discovery_url`"),
            "error should identify blank discovery URL override: {error}"
        );
    }

    #[test]
    fn trusted_issuer_rejects_http_url_by_default() {
        let error = IssuerTrustConfig::from_inputs(vec![TrustedIssuerInput {
            entry: TrustedIssuerEntry::Issuer("http://issuer.example.com".to_owned()),
            discovery_url: None,
            expected_audience: Vec::new(),
            jose_typ: None,
            clock_skew_leeway_secs: None,
        }])
        .expect_err("HTTP issuer should fail under the default URL policy");

        assert!(
            error.to_string().contains("must use https"),
            "error should identify HTTPS policy failure: {error}"
        );
    }

    #[test]
    fn resolve_issuer_carries_per_issuer_overrides() {
        let trust = IssuerTrustConfig::from_inputs(vec![TrustedIssuerInput {
            entry: TrustedIssuerEntry::Issuer("https://issuer.example.com".to_owned()),
            discovery_url: None,
            expected_audience: vec!["public-api".to_owned(), "https://*.api".to_owned()],
            jose_typ: Some("OBO+JWT".to_owned()),
            clock_skew_leeway_secs: Some(30),
        }])
        .unwrap();

        let resolved = trust
            .resolve_issuer("https://issuer.example.com")
            .expect("issuer should resolve");

        assert_eq!(resolved.clock_skew_leeway_secs, Some(30));
        // jose_typ is normalized to lowercase for case-insensitive comparison.
        assert_eq!(resolved.jose_typ.as_deref(), Some("obo+jwt"));
        assert_eq!(resolved.expected_audience.len(), 2);
        assert!(resolved.expected_audience[0].is_match("public-api"));
        assert!(resolved.expected_audience[1].is_match("https://tenant.api"));
    }

    #[test]
    fn resolve_issuer_first_match_is_terminal_when_discovery_invalid() {
        // Rule 0 matches the issuer but its discovery template resolves to a URL
        // with a query string (invalid). A later catch-all rule that *would*
        // resolve must NOT be consulted — first match wins, so resolution fails
        // closed instead of silently adopting the later rule's policy.
        let trust = IssuerTrustConfig::from_inputs(vec![
            TrustedIssuerInput {
                entry: TrustedIssuerEntry::Issuer("https://issuer.example.com".to_owned()),
                discovery_url: Some("{issuer}?leak=1".to_owned()),
                expected_audience: Vec::new(),
                jose_typ: None,
                clock_skew_leeway_secs: None,
            },
            TrustedIssuerInput {
                entry: TrustedIssuerEntry::IssuerPattern("^https://.*$".to_owned()),
                discovery_url: None,
                expected_audience: Vec::new(),
                jose_typ: None,
                clock_skew_leeway_secs: None,
            },
        ])
        .expect("config builds");

        assert!(
            trust.resolve_issuer("https://issuer.example.com").is_none(),
            "a matched-but-invalid rule must be terminal, not fall through to later rules"
        );
    }

    #[test]
    fn resolve_issuer_defaults_overrides_to_empty_and_none() {
        let trust = IssuerTrustConfig::from_inputs(vec![exact_input("https://issuer.example.com")])
            .unwrap();

        let resolved = trust
            .resolve_issuer("https://issuer.example.com")
            .expect("issuer should resolve");

        assert!(resolved.expected_audience.is_empty());
        assert!(resolved.jose_typ.is_none());
        assert!(resolved.clock_skew_leeway_secs.is_none());
    }

    #[test]
    fn blank_jose_typ_override_fails_fast() {
        let error = IssuerTrustConfig::from_inputs(vec![TrustedIssuerInput {
            entry: TrustedIssuerEntry::Issuer("https://issuer.example.com".to_owned()),
            discovery_url: None,
            expected_audience: Vec::new(),
            jose_typ: Some("   ".to_owned()),
            clock_skew_leeway_secs: None,
        }])
        .expect_err("blank jose_typ override should fail");

        assert!(
            error
                .to_string()
                .contains("`trusted_issuers` entry at index 0 has an empty `jose_typ`"),
            "error should identify blank jose_typ override: {error}"
        );
    }

    #[test]
    fn excessive_per_issuer_leeway_fails_fast() {
        let error = IssuerTrustConfig::from_inputs(vec![TrustedIssuerInput {
            entry: TrustedIssuerEntry::Issuer("https://issuer.example.com".to_owned()),
            discovery_url: None,
            expected_audience: Vec::new(),
            jose_typ: None,
            clock_skew_leeway_secs: Some(301),
        }])
        .expect_err("per-issuer leeway over 300s should fail");

        assert!(
            error
                .to_string()
                .contains("`clock_skew_leeway_secs` must be <= 300"),
            "error should identify per-issuer leeway bound: {error}"
        );
    }

    #[test]
    fn invalid_per_issuer_audience_pattern_fails_fast() {
        let error = IssuerTrustConfig::from_inputs(vec![TrustedIssuerInput {
            entry: TrustedIssuerEntry::Issuer("https://issuer.example.com".to_owned()),
            discovery_url: None,
            expected_audience: vec!["api-**".to_owned()],
            jose_typ: None,
            clock_skew_leeway_secs: None,
        }])
        .expect_err("invalid per-issuer audience pattern should fail");

        assert!(
            error.to_string().contains("invalid `expected_audience`"),
            "error should identify invalid per-issuer audience: {error}"
        );
    }

    #[test]
    fn hidden_test_trusted_issuer_builder_allows_http_url() {
        let trust = IssuerTrustConfig::from_inputs_allowing_insecure_http_for_tests(vec![
            TrustedIssuerInput {
                entry: TrustedIssuerEntry::Issuer("http://issuer.example.com".to_owned()),
                discovery_url: None,
                expected_audience: Vec::new(),
                jose_typ: None,
                clock_skew_leeway_secs: None,
            },
        ])
        .expect("hidden test builder should allow HTTP issuer URLs");

        assert!(trust.is_trusted("http://issuer.example.com"));
    }
}

#[cfg(test)]
mod gear_input_tests {
    use super::*;

    #[test]
    #[allow(clippy::cognitive_complexity)]
    fn gear_config_deserializes_documented_schema() {
        let json = r#"{
            "vendor": "custom-vendor",
            "priority": 250,
            "jwt": {
                "require_audience": true,
                "expected_audience": ["cyber-fabric-api"],
                "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                "claim_mapping": { "subject_tenant_id": "tenant_id", "subject_type": "sub_type" },
                "first_party_clients": ["platform-portal"],
                "required_claims": ["tenant_id"]
            },
            "discovery_cache": { "ttl": "30m", "max_entries": 7 },
            "jwks_cache": {
                "ttl": "45m",
                "stale_ttl": "48h",
                "max_entries": 11,
                "refresh_on_unknown_kid": false,
                "refresh_min_interval": "45s"
            },
            "http_client": {
                "request_timeout": "5s",
                "custom_ca_certificate_paths": ["custom-root-ca.pem"]
            },
            "retry_policy": { "max_attempts": 5, "initial_backoff": "250ms", "max_backoff": "3s", "jitter": false },
            "circuit_breaker": { "failure_threshold": 10, "reset_timeout": "60s" },
            "s2s_oauth": {
                "discovery_url": "https://oidc/realms/platform",
                "token_cache": { "ttl": "600s", "max_entries": 120 },
                "default_subject_type": "gts.cf.core.security.subject_user.v1~"
            }
        }"#;
        let config: OidcAuthNGearConfig =
            serde_json::from_str(json).expect("documented config should deserialize");
        let resolved = config
            .resolve()
            .expect("documented config should resolve to runtime config");
        assert!(
            resolved
                .issuer_trust
                .is_trusted("https://oidc/realms/platform")
        );

        assert_eq!(resolved.jwt_validation.expected_audience.len(), 1);
        assert_eq!(resolved.plugin.vendor, "custom-vendor");
        assert_eq!(resolved.plugin.priority, 250);
        assert!(resolved.jwt_validation.expected_audience[0].is_match("cyber-fabric-api"));
        assert_eq!(resolved.jwt_validation.jwks_cache_ttl_secs, 2700);
        assert_eq!(resolved.jwt_validation.jwks_stale_ttl_secs, 172_800);
        assert!(!resolved.jwt_validation.jwks_refresh_on_unknown_kid);
        assert_eq!(resolved.jwt_validation.jwks_refresh_min_interval_secs, 45);
        assert_eq!(resolved.jwt_validation.discovery_cache_ttl_secs, 1800);
        assert_eq!(resolved.jwt_validation.discovery_max_entries, 7);
        assert_eq!(resolved.jwt_validation.jwks_max_entries, 11);
        let circuit_breaker = resolved
            .plugin
            .circuit_breaker
            .as_ref()
            .expect("circuit breaker should be enabled");
        assert_eq!(circuit_breaker.failure_threshold, 10);
        assert_eq!(circuit_breaker.reset_timeout_secs, 60);
        assert_eq!(resolved.plugin.retry_policy.max_attempts, 5);
        assert_eq!(resolved.plugin.retry_policy.initial_backoff_ms, 250);
        assert_eq!(resolved.plugin.retry_policy.max_backoff_ms, 3000);
        assert!(!resolved.plugin.retry_policy.jitter);
        assert_eq!(resolved.plugin.claim_mapper.subject_tenant_id, "tenant_id");
        assert_eq!(
            resolved.plugin.claim_mapper_options.first_party_clients,
            vec!["platform-portal".to_owned()]
        );
        assert_eq!(
            resolved.plugin.claim_mapper_options.required_claims,
            vec!["tenant_id".to_owned()]
        );
        assert_eq!(
            resolved.plugin.s2s.discovery_url.as_str(),
            "https://oidc/realms/platform"
        );
        assert_eq!(
            resolved.plugin.s2s_default_subject_type,
            "gts.cf.core.security.subject_user.v1~"
        );
        assert_eq!(resolved.plugin.s2s.token_cache_ttl_secs, 600);
        assert_eq!(resolved.plugin.s2s.token_cache_max_entries, 120);
        assert_eq!(resolved.request_timeout, Duration::from_secs(5));
        assert_eq!(
            resolved.custom_ca_certificate_paths,
            vec!["custom-root-ca.pem".to_owned()]
        );
    }

    #[test]
    fn gear_config_deserializes_per_issuer_overrides() {
        let json = r#"{
            "jwt": {
                "expected_audience": ["global-api"],
                "trusted_issuers": [
                    {
                        "issuer": "https://obo.example.com",
                        "expected_audience": ["public-api"],
                        "jose_typ": "obo+jwt",
                        "clock_skew_leeway_secs": 30
                    },
                    { "issuer": "https://kc.example.com/realms/platform" }
                ],
                "claim_mapping": { "subject_tenant_id": "tenant_id", "subject_type": "sub_type" }
            },
            "s2s_oauth": {
                "discovery_url": "https://obo.example.com",
                "default_subject_type": "gts.cf.core.security.subject_user.v1~"
            }
        }"#;
        let config: OidcAuthNGearConfig =
            serde_json::from_str(json).expect("per-issuer override config should deserialize");
        let resolved = config
            .resolve()
            .expect("per-issuer override config should resolve");

        // OBO issuer carries its own overrides.
        let obo = resolved
            .issuer_trust
            .resolve_issuer("https://obo.example.com")
            .expect("OBO issuer should resolve");
        assert_eq!(obo.jose_typ.as_deref(), Some("obo+jwt"));
        assert_eq!(obo.clock_skew_leeway_secs, Some(30));
        assert_eq!(obo.expected_audience.len(), 1);
        assert!(obo.expected_audience[0].is_match("public-api"));

        // KC issuer has no overrides (preserves global behavior).
        let kc = resolved
            .issuer_trust
            .resolve_issuer("https://kc.example.com/realms/platform")
            .expect("KC issuer should resolve");
        assert!(kc.jose_typ.is_none());
        assert!(kc.clock_skew_leeway_secs.is_none());
        assert!(kc.expected_audience.is_empty());
    }

    #[test]
    fn gear_config_requires_s2s_default_subject_type() {
        let json = r#"{
            "jwt": {
                "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                "claim_mapping": { "subject_tenant_id": "tenant_id", "subject_type": "sub_type" }
            },
            "s2s_oauth": {
                "discovery_url": "https://oidc/realms/platform"
            }
        }"#;
        let error = serde_json::from_str::<OidcAuthNGearConfig>(json)
            .expect_err("missing S2S default subject type should fail deserialization");

        assert!(
            error
                .to_string()
                .contains("missing field `default_subject_type`"),
            "error should identify missing S2S default subject type: {error}"
        );
    }

    #[test]
    fn gear_config_rejects_blank_s2s_default_subject_type() {
        let json = r#"{
            "jwt": {
                "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                "claim_mapping": { "subject_tenant_id": "tenant_id", "subject_type": "sub_type" }
            },
            "s2s_oauth": {
                "discovery_url": "https://oidc/realms/platform",
                "default_subject_type": "   "
            }
        }"#;
        let config: OidcAuthNGearConfig =
            serde_json::from_str(json).expect("blank string should deserialize before validation");
        let error = config
            .resolve()
            .expect_err("blank S2S default subject type should fail resolution");

        assert!(
            error
                .to_string()
                .contains("`s2s_oauth.default_subject_type` must not be empty"),
            "error should identify blank S2S default subject type: {error}"
        );
    }

    #[test]
    fn gear_config_rejects_blank_custom_ca_certificate_path() {
        let json = r#"{
            "jwt": {
                "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                "claim_mapping": { "subject_tenant_id": "tenant_id", "subject_type": "sub_type" }
            },
            "http_client": { "custom_ca_certificate_paths": ["   "] },
            "s2s_oauth": {
                "discovery_url": "https://oidc/realms/platform",
                "default_subject_type": "gts.cf.core.security.subject_user.v1~"
            }
        }"#;
        let config: OidcAuthNGearConfig =
            serde_json::from_str(json).expect("blank CA path should deserialize before validation");
        let error = config
            .resolve()
            .expect_err("blank CA path should fail resolution");

        assert!(
            error
                .to_string()
                .contains("`http_client.custom_ca_certificate_paths[0]` must not be empty"),
            "error should identify blank custom CA path: {error}"
        );
    }

    #[test]
    fn gear_config_rejects_zero_http_client_request_timeout() {
        let json = r#"{
            "jwt": {
                "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                "claim_mapping": { "subject_tenant_id": "tenant_id", "subject_type": "sub_type" }
            },
            "http_client": { "request_timeout": "0s" },
            "s2s_oauth": {
                "discovery_url": "https://oidc/realms/platform",
                "default_subject_type": "gts.cf.core.security.subject_user.v1~"
            }
        }"#;
        let config: OidcAuthNGearConfig =
            serde_json::from_str(json).expect("zero request timeout should deserialize");
        let error = config
            .resolve()
            .expect_err("zero request timeout should fail resolution");

        assert!(
            error
                .to_string()
                .contains("http_client.request_timeout must be positive"),
            "error should identify zero request timeout: {error}"
        );
    }

    #[test]
    fn gear_config_rejects_blank_s2s_discovery_url() {
        let json = r#"{
            "jwt": {
                "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                "claim_mapping": { "subject_tenant_id": "tenant_id", "subject_type": "sub_type" }
            },
            "s2s_oauth": {
                "discovery_url": "   ",
                "default_subject_type": "gts.cf.core.security.subject_user.v1~"
            }
        }"#;
        let config: OidcAuthNGearConfig =
            serde_json::from_str(json).expect("blank string should deserialize before validation");
        let error = config
            .resolve()
            .expect_err("blank S2S discovery URL should fail resolution");

        assert!(
            error
                .to_string()
                .contains("`s2s_oauth.discovery_url` must not be empty"),
            "error should identify blank S2S discovery URL: {error}"
        );
    }

    #[test]
    fn gear_config_rejects_http_s2s_discovery_url_by_default() {
        let json = r#"{
            "jwt": {
                "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                "claim_mapping": { "subject_tenant_id": "tenant_id", "subject_type": "sub_type" }
            },
            "s2s_oauth": {
                "discovery_url": "http://oidc/realms/platform",
                "default_subject_type": "gts.cf.core.security.subject_user.v1~"
            }
        }"#;
        let error = serde_json::from_str::<OidcAuthNGearConfig>(json)
            .expect("config JSON should parse")
            .resolve()
            .expect_err("HTTP S2S discovery URL should fail under default URL policy");

        assert!(
            error.to_string().contains("must use https"),
            "error should identify HTTPS policy failure: {error}"
        );
    }

    #[test]
    fn gear_config_trims_s2s_discovery_url() {
        let json = r#"{
            "jwt": {
                "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                "claim_mapping": { "subject_tenant_id": "tenant_id", "subject_type": "sub_type" }
            },
            "s2s_oauth": {
                "discovery_url": "  https://oidc/realms/platform  ",
                "default_subject_type": "gts.cf.core.security.subject_user.v1~"
            }
        }"#;
        let config: OidcAuthNGearConfig =
            serde_json::from_str(json).expect("padded discovery URL should deserialize");
        let resolved = config
            .resolve()
            .expect("padded discovery URL should resolve");

        assert_eq!(
            resolved.plugin.s2s.discovery_url.as_str(),
            "https://oidc/realms/platform"
        );
    }

    #[test]
    fn gear_config_rejects_blank_claim_mapping_subject_tenant_id() {
        let json = r#"{
            "jwt": {
                "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                "claim_mapping": { "subject_tenant_id": "   ", "subject_type": "sub_type" }
            },
            "s2s_oauth": {
                "discovery_url": "https://oidc/realms/platform",
                "default_subject_type": "gts.cf.core.security.subject_user.v1~"
            }
        }"#;
        let config: OidcAuthNGearConfig = serde_json::from_str(json)
            .expect("blank subject tenant ID should deserialize before validation");
        let error = config
            .resolve()
            .expect_err("blank subject tenant ID should fail resolution");

        assert!(
            error
                .to_string()
                .contains("`jwt.claim_mapping.subject_tenant_id` must not be empty"),
            "error should identify blank subject tenant ID: {error}"
        );
    }

    #[test]
    fn gear_config_trims_claim_mapping_subject_tenant_id() {
        let json = r#"{
            "jwt": {
                "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                "claim_mapping": { "subject_tenant_id": "  tenant_id  ", "subject_type": "sub_type" }
            },
            "s2s_oauth": {
                "discovery_url": "https://oidc/realms/platform",
                "claim_mapping": { "subject_tenant_id": "  s2s_tenant_id  " },
                "default_subject_type": "gts.cf.core.security.subject_user.v1~"
            }
        }"#;
        let config: OidcAuthNGearConfig =
            serde_json::from_str(json).expect("padded subject tenant IDs should deserialize");
        let resolved = config
            .resolve()
            .expect("padded subject tenant IDs should resolve");

        assert_eq!(resolved.plugin.claim_mapper.subject_tenant_id, "tenant_id");
        assert_eq!(
            resolved.plugin.s2s_claim_mapper.subject_tenant_id,
            "s2s_tenant_id"
        );
    }

    #[test]
    fn gear_config_keeps_expected_audience_when_audience_is_not_required() {
        let json = r#"{
            "jwt": {
                "require_audience": false,
                "expected_audience": ["https://*.api.example.com"],
                "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                "claim_mapping": { "subject_tenant_id": "tenant_id", "subject_type": "sub_type" }
            },
            "s2s_oauth": {
                "discovery_url": "https://oidc/realms/platform",
                "default_subject_type": "gts.cf.core.security.subject_user.v1~"
            }
        }"#;
        let config: OidcAuthNGearConfig =
            serde_json::from_str(json).expect("optional audience config should deserialize");
        let resolved = config
            .resolve()
            .expect("optional audience config should resolve");

        assert_eq!(resolved.jwt_validation.expected_audience.len(), 1);
        assert!(!resolved.jwt_validation.require_audience);
        assert!(
            resolved.jwt_validation.expected_audience[0].is_match("https://tenant.api.example.com")
        );
    }

    #[test]
    fn gear_config_resolves_configured_algorithm_subset_and_clock_skew() {
        let json = r#"{
            "jwt": {
                "supported_algorithms": ["RS256"],
                "clock_skew_leeway": "120s",
                "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                "claim_mapping": { "subject_tenant_id": "tenant_id", "subject_type": "sub_type" }
            },
            "s2s_oauth": {
                "discovery_url": "https://oidc/realms/platform",
                "default_subject_type": "gts.cf.core.security.subject_user.v1~"
            }
        }"#;
        let config: OidcAuthNGearConfig =
            serde_json::from_str(json).expect("algorithm subset config should deserialize");
        let resolved = config
            .resolve()
            .expect("algorithm subset config should resolve");

        assert_eq!(
            resolved.jwt_validation.supported_algorithms,
            vec![Algorithm::RS256]
        );
        assert_eq!(resolved.jwt_validation.clock_skew_leeway_secs, 120);
    }

    #[test]
    fn gear_config_rejects_unsupported_algorithm() {
        let json = r#"{
            "jwt": {
                "supported_algorithms": ["HS256"],
                "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                "claim_mapping": { "subject_tenant_id": "tenant_id", "subject_type": "sub_type" }
            },
            "s2s_oauth": {
                "discovery_url": "https://oidc/realms/platform",
                "default_subject_type": "gts.cf.core.security.subject_user.v1~"
            }
        }"#;
        let config: OidcAuthNGearConfig =
            serde_json::from_str(json).expect("unsupported algorithm config should deserialize");
        let error = config
            .resolve()
            .expect_err("unsupported algorithm should fail resolution");

        assert!(
            error
                .to_string()
                .contains("unsupported jwt.supported_algorithms[0]: HS256"),
            "error should identify unsupported algorithm: {error}"
        );
    }

    #[test]
    fn gear_config_rejects_excessive_clock_skew() {
        let json = r#"{
            "jwt": {
                "clock_skew_leeway": "301s",
                "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                "claim_mapping": { "subject_tenant_id": "tenant_id", "subject_type": "sub_type" }
            },
            "s2s_oauth": {
                "discovery_url": "https://oidc/realms/platform",
                "default_subject_type": "gts.cf.core.security.subject_user.v1~"
            }
        }"#;
        let config: OidcAuthNGearConfig =
            serde_json::from_str(json).expect("clock skew config should deserialize");
        let error = config
            .resolve()
            .expect_err("clock skew over 300s should fail resolution");

        assert!(
            error
                .to_string()
                .contains("`jwt.clock_skew_leeway` must be <= 300s"),
            "error should identify clock skew bound: {error}"
        );
    }

    #[test]
    fn gear_config_rejects_stale_ttl_shorter_than_fresh_ttl() {
        let json = r#"{
            "jwt": {
                "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                "claim_mapping": { "subject_tenant_id": "tenant_id", "subject_type": "sub_type" }
            },
            "jwks_cache": { "ttl": "10m", "stale_ttl": "5m" },
            "s2s_oauth": {
                "discovery_url": "https://oidc/realms/platform",
                "default_subject_type": "gts.cf.core.security.subject_user.v1~"
            }
        }"#;
        let config: OidcAuthNGearConfig =
            serde_json::from_str(json).expect("stale ttl config should deserialize");
        let error = config
            .resolve()
            .expect_err("stale ttl shorter than fresh ttl should fail resolution");

        assert!(
            error
                .to_string()
                .contains("`jwks_cache.stale_ttl` must be 0s or >= `jwks_cache.ttl`"),
            "error should identify stale ttl bound: {error}"
        );
    }

    #[test]
    fn gear_config_rejects_zero_circuit_breaker_reset_timeout() {
        for reset_timeout in ["0s", "500ms"] {
            let json = format!(
                r#"{{
                    "jwt": {{
                        "trusted_issuers": [{{ "issuer": "https://oidc/realms/platform" }}],
                        "claim_mapping": {{ "subject_tenant_id": "tenant_id", "subject_type": "sub_type" }}
                    }},
                    "circuit_breaker": {{ "reset_timeout": "{reset_timeout}" }},
                    "s2s_oauth": {{
                        "discovery_url": "https://oidc/realms/platform",
                        "default_subject_type": "gts.cf.core.security.subject_user.v1~"
                    }}
                }}"#
            );
            let config: OidcAuthNGearConfig =
                serde_json::from_str(&json).expect("reset timeout config should deserialize");
            let error = config
                .resolve()
                .expect_err("zero reset timeout should fail resolution");

            assert!(
                error
                    .to_string()
                    .contains("`circuit_breaker.reset_timeout` must be >= 1s"),
                "error should identify reset timeout bound for {reset_timeout}: {error}"
            );
        }
    }

    #[test]
    fn gear_config_allows_zero_stale_ttl_to_disable_stale_fallback() {
        let json = r#"{
            "jwt": {
                "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                "claim_mapping": { "subject_tenant_id": "tenant_id", "subject_type": "sub_type" }
            },
            "jwks_cache": { "ttl": "10m", "stale_ttl": "0s" },
            "s2s_oauth": {
                "discovery_url": "https://oidc/realms/platform",
                "default_subject_type": "gts.cf.core.security.subject_user.v1~"
            }
        }"#;
        let config: OidcAuthNGearConfig =
            serde_json::from_str(json).expect("zero stale ttl config should deserialize");
        let resolved = config
            .resolve()
            .expect("zero stale ttl should disable stale fallback");

        assert_eq!(resolved.jwt_validation.jwks_stale_ttl_secs, 0);
    }
}
