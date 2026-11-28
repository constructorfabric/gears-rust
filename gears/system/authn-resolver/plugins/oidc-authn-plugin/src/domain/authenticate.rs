//! Authentication orchestrator — JWT-only local validation.
//!
//! ```text
//! authenticate(token)
//!   ├── opaque (non-JWT) -> Err(UnsupportedTokenFormat)  [no network]
//!   └── JWT              -> JwtValidator (local, cached JWKS)
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverError, AuthNResolverPluginClient, AuthNResolverPluginSpecV1,
    AuthenticationRequest, AuthenticationResult, CacheControl, ClientCredentialsRequest,
};
use secrecy::ExposeSecret as _;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit_macros::domain_model;
use tracing::{debug, error, instrument};

use crate::config::{
    INSTANCE_SUFFIX, IssuerTrustConfig, JwtValidationConfig, MAX_AUTHENTICATE_RESULT_CACHE_TTL,
};
use crate::domain::claim_mapper::{
    ClaimMapperConfig, ClaimMapperOptions, Claims, default_config as default_claim_mapper_config,
};
use crate::domain::error::AuthNError;
use crate::domain::metrics::AuthNMetrics;
use crate::domain::ports::{ClientCredentialsExchanger, ExchangedToken, JwksProvider};
use crate::domain::token_type::{TokenType, detect_token_type};
use crate::domain::validator::{JwtClaims, JwtValidator};

/// Plugin authentication orchestrator (JWT-only).
///
/// Debug output intentionally omits the validator internals and only shows
/// operational state (registration status and issuer-trust summary).
#[domain_model]
pub struct OidcAuthNPlugin {
    jwt_validator: JwtValidator,
    jwt_config: JwtValidationConfig,
    /// Runtime issuer trust config (compiled regexes in regex mode, `HashSet` in exact mode).
    issuer_trust: IssuerTrustConfig,
    claim_mapper_config: ClaimMapperConfig,
    s2s_claim_mapper_config: ClaimMapperConfig,
    claim_mapper_options: ClaimMapperOptions,
    s2s_default_subject_type: String,
    /// Caller reuse allowed for bearer-token results; zero means no-store.
    authenticate_result_cache_ttl: Duration,
    /// Injected metrics handle shared across all plugin components.
    metrics: Arc<AuthNMetrics>,
    /// S2S token exchange port.
    token_exchanger: Arc<dyn ClientCredentialsExchanger>,
    is_registered: AtomicBool,
}

impl std::fmt::Debug for OidcAuthNPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcAuthNPlugin")
            .field("is_registered", &self.is_registered())
            .field("issuer_trust", &self.issuer_trust)
            .finish_non_exhaustive()
    }
}

/// Builder for [`OidcAuthNPlugin`] runtime wiring.
#[domain_model]
pub struct OidcAuthNPluginBuilder {
    jwt_config: JwtValidationConfig,
    issuer_trust: IssuerTrustConfig,
    claim_mapper_config: ClaimMapperConfig,
    s2s_claim_mapper_config: ClaimMapperConfig,
    claim_mapper_options: ClaimMapperOptions,
    s2s_default_subject_type: String,
    authenticate_result_cache_ttl: Duration,
}

impl OidcAuthNPluginBuilder {
    /// Start a plugin builder with production defaults for optional runtime settings.
    #[must_use]
    pub fn new(
        jwt_config: JwtValidationConfig,
        issuer_trust: IssuerTrustConfig,
        s2s_default_subject_type: String,
    ) -> Self {
        Self {
            jwt_config,
            issuer_trust,
            claim_mapper_config: default_claim_mapper_config(),
            s2s_claim_mapper_config: default_claim_mapper_config(),
            claim_mapper_options: ClaimMapperOptions::default(),
            s2s_default_subject_type,
            authenticate_result_cache_ttl: Duration::ZERO,
        }
    }

    /// Permit caller reuse from validation start until the earlier of `ttl` and verified
    /// `exp`. Zero disables reuse (default). This builder caps `ttl` at
    /// [`MAX_AUTHENTICATE_RESULT_CACHE_TTL`] (300s). File configuration rejects larger values.
    #[must_use]
    pub fn authenticate_result_cache_ttl(mut self, ttl: Duration) -> Self {
        self.authenticate_result_cache_ttl = ttl.min(MAX_AUTHENTICATE_RESULT_CACHE_TTL);
        self
    }

    /// Set the claim mapper used for bearer-token authentication.
    #[must_use]
    pub fn claim_mapper_config(mut self, config: ClaimMapperConfig) -> Self {
        self.claim_mapper_config = config;
        self
    }

    /// Set the same claim mapper for bearer-token and S2S authentication.
    #[must_use]
    pub fn shared_claim_mapper_config(mut self, config: ClaimMapperConfig) -> Self {
        self.claim_mapper_config = config.clone();
        self.s2s_claim_mapper_config = config;
        self
    }

    /// Set the claim mapper used for S2S client-credentials tokens.
    #[must_use]
    pub fn s2s_claim_mapper_config(mut self, config: ClaimMapperConfig) -> Self {
        self.s2s_claim_mapper_config = config;
        self
    }

    /// Set the claim options shared by bearer-token and S2S authentication.
    #[must_use]
    pub fn claim_mapper_options(mut self, options: ClaimMapperOptions) -> Self {
        self.claim_mapper_options = options;
        self
    }

    /// Build a plugin instance.
    ///
    /// Metrics registration is performed in the gear's `init()` hook, which always
    /// runs before plugin construction; building the plugin does not register metrics itself.
    #[must_use]
    pub fn build(
        self,
        jwks_provider: Arc<dyn JwksProvider>,
        token_exchanger: Arc<dyn ClientCredentialsExchanger>,
        metrics: Arc<AuthNMetrics>,
    ) -> OidcAuthNPlugin {
        OidcAuthNPlugin {
            jwt_validator: JwtValidator::new(jwks_provider, Arc::clone(&metrics)),
            jwt_config: self.jwt_config,
            issuer_trust: self.issuer_trust,
            claim_mapper_config: self.claim_mapper_config,
            s2s_claim_mapper_config: self.s2s_claim_mapper_config,
            claim_mapper_options: self.claim_mapper_options,
            s2s_default_subject_type: self.s2s_default_subject_type,
            authenticate_result_cache_ttl: self.authenticate_result_cache_ttl,
            metrics,
            token_exchanger,
            is_registered: AtomicBool::new(false),
        }
    }

    /// Build and register the plugin after `ClientHub` is available.
    ///
    /// This ensures registration happens eagerly at startup instead of lazily
    /// on first authentication request.
    ///
    /// # Errors
    ///
    /// Returns [`AuthNResolverError`] if plugin registration fails.
    pub fn build_registered(
        self,
        hub: &ClientHub,
        jwks_provider: Arc<dyn JwksProvider>,
        token_exchanger: Arc<dyn ClientCredentialsExchanger>,
        metrics: Arc<AuthNMetrics>,
    ) -> Result<Arc<OidcAuthNPlugin>, AuthNResolverError> {
        let plugin = Arc::new(self.build(jwks_provider, token_exchanger, metrics));
        plugin.register(hub)?;
        Ok(plugin)
    }
}

impl OidcAuthNPlugin {
    /// Register this plugin instance in `ClientHub` under the canonical
    /// AuthN-resolver GTS instance-id scope.
    ///
    /// Registration is guarded to prevent duplicate registration of the same plugin
    /// instance. Calling this method twice on the same `Arc<Self>` returns an error.
    ///
    /// # Errors
    ///
    /// Returns [`AuthNResolverError::Internal`] if the plugin is already registered.
    pub fn register(self: &Arc<Self>, hub: &ClientHub) -> Result<ClientScope, AuthNResolverError> {
        if self.is_registered.swap(true, Ordering::AcqRel) {
            return Err(AuthNResolverError::Internal(
                "oidc plugin instance is already registered".to_owned(),
            ));
        }

        // Convert this concrete plugin into the trait object required by the resolver.
        let client: Arc<dyn AuthNResolverPluginClient> = self.clone();
        // AuthN resolver resolves plugins by GTS instance id and then loads the
        // scoped client by that same id. Keep this scoped registration in sync
        // with gear-level GTS instance metadata.
        let scope = Self::register_instance_client_scope(hub, client);
        Ok(scope)
    }

    /// Return whether this plugin instance was already registered.
    #[must_use]
    pub fn is_registered(&self) -> bool {
        self.is_registered.load(Ordering::Acquire)
    }

    /// Register plugin under the canonical GTS instance-id scope used by
    /// `authn-resolver` runtime service lookup.
    fn register_instance_client_scope(
        hub: &ClientHub,
        client: Arc<dyn AuthNResolverPluginClient>,
    ) -> ClientScope {
        let instance_id = AuthNResolverPluginSpecV1::gts_make_instance_id(INSTANCE_SUFFIX);
        let scope = ClientScope::gts_id(instance_id.as_ref());
        hub.register_scoped::<dyn AuthNResolverPluginClient>(scope.clone(), client);
        scope
    }

    /// Authenticate a bearer token (JWT-only; opaque tokens are rejected).
    ///
    /// **Note:** request-level success and failure metrics are recorded by the
    /// [`AuthNResolverPluginClient`] trait implementation, which is the
    /// production entry point. Direct callers should account for this if they
    /// need request-level observability.
    ///
    /// # Errors
    ///
    /// Returns [`AuthNError`] on validation failure (opaque token, expired,
    /// untrusted issuer, bad signature, etc.).
    #[instrument(skip(self, token), fields(token_type = tracing::field::Empty))]
    pub async fn authenticate(&self, token: &str) -> Result<JwtClaims, AuthNError> {
        let token_type = detect_token_type(token);
        tracing::Span::current().record("token_type", tracing::field::debug(&token_type));
        // JWT-only policy: opaque tokens are always rejected fail-closed.
        if matches!(token_type, TokenType::Opaque) {
            debug!("Opaque token detected - Unauthorized (JWT-only policy)");
            return Err(AuthNError::UnsupportedTokenFormat);
        }

        let claims = self
            .jwt_validator
            .validate(token, &self.jwt_config, &self.issuer_trust)
            .await?;
        Ok(claims)
    }
}

#[async_trait]
impl AuthNResolverPluginClient for OidcAuthNPlugin {
    /// Validate the JWT on every call, regardless of the requested cache policy.
    async fn authenticate(
        &self,
        request: &AuthenticationRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        let bearer_token = request.bearer_token.expose_secret();
        let start = Instant::now();
        // Reuse is authorized relative to the start: slow validation must not
        // extend it.
        let validation_started = time::OffsetDateTime::now_utc();
        let result = async {
            let jwt_claims = OidcAuthNPlugin::authenticate(self, bearer_token)
                .await
                .map_err(AuthNResolverError::from)?;

            let claims = jwt_claims_to_map(jwt_claims);
            let security_context = crate::domain::claim_mapper::map_with_bearer_with_options(
                &claims,
                &self.claim_mapper_config,
                &self.claim_mapper_options,
                Some(bearer_token),
                &self.metrics,
            )?;

            #[cfg(feature = "e2e-diagnostics")]
            tracing::info!(
                marker = "E2E_AUTHN_RESULT",
                subject_id = %security_context.subject_id(),
                subject_tenant_id = %security_context.subject_tenant_id(),
                subject_type = ?security_context.subject_type(),
                token_scopes = ?security_context.token_scopes(),
                "authentication completed"
            );

            let cache_control = authenticate_cache_control(
                self.authenticate_result_cache_ttl,
                validation_started,
                security_context.expires_at(),
                time::OffsetDateTime::now_utc(),
            );
            Ok(AuthenticationResult::new(security_context).with_cache_control(cache_control))
        }
        .await;

        match &result {
            Ok(_) => self
                .metrics
                .record_request_success_duration(start.elapsed()),
            Err(error) => {
                let label = authn_resolver_error_label(error);
                self.metrics.increment_error(label);
                self.metrics.increment_request_failure(label);
            }
        }

        result
    }

    #[instrument(skip(self, request), fields(client_id = %request.client_id))]
    async fn exchange_client_credentials(
        &self,
        request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        let start = std::time::Instant::now();
        self.metrics.increment_s2s_exchange();

        let result = async {
            // Exchange credentials for an access token (JWT).
            let exchanged = self
                .token_exchanger
                .exchange(request, &self.issuer_trust)
                .await
                .map_err(AuthNResolverError::from)?;

            // Validate the obtained JWT through the existing validation pipeline.
            let jwt_claims =
                OidcAuthNPlugin::authenticate(self, exchanged.access_token.expose_secret())
                    .await
                    .map_err(AuthNResolverError::from)?;

            // Map claims to SecurityContext with the obtained token as bearer.
            // S2S service-account tokens skip first-party ratio tracking to
            // avoid skewing the interactive-user gauge.
            let claims = jwt_claims_to_map(jwt_claims);
            let (builder, jwt_expiry) = crate::domain::claim_mapper::prepare_with_bearer(
                &claims,
                &self.s2s_claim_mapper_config,
                &self.claim_mapper_options,
                Some(exchanged.access_token.expose_secret()),
                &self.metrics,
                Some(crate::domain::claim_mapper::S2sMapping {
                    default_subject_type: &self.s2s_default_subject_type,
                }),
            )?;

            // Decided after mapping, so a credential expiring meanwhile is caught.
            let wall = time::OffsetDateTime::now_utc();
            let (expiry, cache_control) =
                exchange_deadlines(&exchanged, jwt_expiry, Instant::now(), wall)?;
            let security_context = crate::domain::claim_mapper::build(match expiry {
                Some(expiry) => builder.expires_at(expiry),
                None => builder,
            })?;
            Ok(AuthenticationResult::new(security_context).with_cache_control(cache_control))
        }
        .await;

        self.metrics.record_s2s_exchange_duration(start.elapsed());

        if let Err(error) = &result {
            let label = authn_resolver_error_label(error);
            self.metrics.increment_error(label);
            self.metrics.increment_s2s_exchange_error(label);
        }

        result
    }
}

/// Convert `AuthNResolverError` variants into stable metric labels.
fn authn_resolver_error_label(error: &AuthNResolverError) -> &'static str {
    match error {
        AuthNResolverError::Unauthorized(_) => "unauthorized",
        AuthNResolverError::NoPluginAvailable => "no_plugin_available",
        AuthNResolverError::ServiceUnavailable(_) => "service_unavailable",
        AuthNResolverError::TokenAcquisitionFailed(_) => "token_acquisition_failed",
        AuthNResolverError::Internal(_) => "internal",
    }
}

/// Convert typed JWT claims into a generic claims map for the claim mapper.
///
/// Delegates to serde serialization so that new fields added to [`JwtClaims`]
/// are automatically included.  `Option` fields annotated with
/// `#[serde(skip_serializing_if = "Option::is_none")]` are omitted when `None`.
fn jwt_claims_to_map(jwt_claims: JwtClaims) -> Claims {
    match serde_json::to_value(jwt_claims) {
        Ok(serde_json::Value::Object(map)) => map,
        other => {
            error!(
                result = ?other,
                "JwtClaims serialization produced unexpected result"
            );
            serde_json::Map::new()
        }
    }
}

/// Bound credential expiry by both JWT and OAuth deadlines, then cap reuse at expiry.
/// Read `wall` before `now` and project both exchange deadlines onto that pair so
/// a backward wall-clock jump cannot extend either lifetime.
///
/// # Errors
///
/// `TokenAcquisitionFailed` when the credential has already expired: a zero
/// `expires_in`, or a short one spent on the exchange.
fn exchange_deadlines(
    exchanged: &ExchangedToken,
    jwt_expiry: Option<time::OffsetDateTime>,
    now: Instant,
    wall: time::OffsetDateTime,
) -> Result<(Option<time::OffsetDateTime>, CacheControl), AuthNResolverError> {
    let oauth_expiry = exchanged
        .expires_at
        .map(|deadline| exchanged.rebase(deadline, now, wall));
    let expiry = match (jwt_expiry, oauth_expiry) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    if expiry.is_some_and(|expiry| expiry <= wall) {
        return Err(AuthNResolverError::TokenAcquisitionFailed(
            "acquired credential is already expired".to_owned(),
        ));
    }
    let cache_until = exchanged.rebase(exchanged.cache_until, now, wall);
    let cache_until = expiry.map_or(cache_until, |expiry| expiry.min(cache_until));
    // A disabled or elapsed reuse policy does not expire the credential.
    let cache_control = if cache_until > wall {
        CacheControl::until(cache_until)
    } else {
        CacheControl::no_store()
    };
    Ok((expiry, cache_control))
}

/// Permit reuse until the earlier of verified expiry and validation start plus `ttl`.
/// Zero TTL or a missing, elapsed or unrepresentable deadline yields no-store.
fn authenticate_cache_control(
    ttl: Duration,
    validation_started: time::OffsetDateTime,
    expires_at: Option<time::OffsetDateTime>,
    now: time::OffsetDateTime,
) -> CacheControl {
    if ttl.is_zero() {
        return CacheControl::no_store();
    }
    let Some(expires_at) = expires_at else {
        return CacheControl::no_store();
    };
    let Some(until) = time::Duration::try_from(ttl)
        .ok()
        .and_then(|ttl| validation_started.checked_add(ttl))
    else {
        return CacheControl::no_store();
    };
    let until = until.min(expires_at);
    if until <= now {
        return CacheControl::no_store();
    }
    CacheControl::until(until)
}

#[cfg(test)]
#[path = "authenticate_tests.rs"]
mod authenticate_tests;
