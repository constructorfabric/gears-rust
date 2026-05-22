//! Authentication orchestrator — JWT-only local validation.
//!
//! ```text
//! authenticate(token)
//!   ├── opaque (non-JWT) -> Err(UnsupportedTokenFormat)  [no network]
//!   └── JWT              -> JwtValidator (local, cached JWKS)
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverError, AuthNResolverPluginClient, AuthNResolverPluginSpecV1, AuthenticationResult,
    ClientCredentialsRequest,
};
use modkit::client_hub::{ClientHub, ClientScope};
use modkit_macros::domain_model;
use tracing::{debug, error, instrument};

use crate::config::{INSTANCE_SUFFIX, IssuerTrustConfig, JwtValidationConfig};
use crate::domain::claim_mapper::{
    ClaimMapperConfig, ClaimMapperOptions, Claims, default_config as default_claim_mapper_config,
};
use crate::domain::error::AuthNError;
use crate::domain::metrics::AuthNMetrics;
use crate::domain::ports::{ClientCredentialsExchanger, JwksProvider};
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
        }
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
    /// Metrics registration is performed in the module's `init()` hook, which always
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
        // with module-level GTS instance metadata.
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
    async fn authenticate(
        &self,
        bearer_token: &str,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        let start = Instant::now();
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

            Ok(AuthenticationResult { security_context })
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
            let access_token = self
                .token_exchanger
                .exchange(request, &self.issuer_trust)
                .await
                .map_err(AuthNResolverError::from)?;

            // Validate the obtained JWT through the existing validation pipeline.
            let jwt_claims = OidcAuthNPlugin::authenticate(self, &access_token)
                .await
                .map_err(AuthNResolverError::from)?;

            // Map claims to SecurityContext with the obtained token as bearer.
            // S2S service-account tokens skip first-party ratio tracking to
            // avoid skewing the interactive-user gauge.
            let claims = jwt_claims_to_map(jwt_claims);
            let security_context = crate::domain::claim_mapper::map_with_bearer_opts(
                &claims,
                &self.s2s_claim_mapper_config,
                &self.claim_mapper_options,
                Some(&access_token),
                &self.metrics,
                false,
                Some(self.s2s_default_subject_type.as_str()),
            )?;

            Ok(AuthenticationResult { security_context })
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

#[cfg(test)]
#[path = "authenticate_tests.rs"]
mod authenticate_tests;
