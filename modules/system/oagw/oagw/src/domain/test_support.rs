//! Test utilities for CP and DP integration tests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::TokenCacheConfig;
use crate::domain::services::{
    ControlPlaneService, ControlPlaneServiceImpl, DataPlaneService, EndpointSelector,
    ServiceGatewayClientV1Facade,
};
use crate::infra::proxy::DataPlaneServiceImpl;
use crate::infra::storage::{InMemoryRouteRepo, InMemoryUpstreamRepo};
use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverClient, AuthZResolverError, EvaluationRequest, EvaluationResponse,
    EvaluationResponseContext, PolicyEnforcer,
};
use credstore_sdk::{
    CredStoreClientV1, CredStoreError, GetSecretResponse, SecretRef, SecretValue, SharingMode,
    TenantId as CredstoreTenantId,
};
use modkit::client_hub::ClientHub;
use modkit_security::SecurityContext;
use oagw_sdk::api::ServiceGatewayClientV1;
use tenant_resolver_sdk::{
    GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
    GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo, TenantRef, TenantResolverClient,
    TenantResolverError, TenantStatus,
};

/// Build an allow-all `PolicyEnforcer` for tests.
pub fn allow_all_enforcer() -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(MockAuthZResolverClient))
}

/// Mock AuthZ resolver that always allows access for testing.
struct MockAuthZResolverClient;

/// Always returns `Allow` so tests that do not care about authorization pass by default.
#[async_trait]
impl AuthZResolverClient for MockAuthZResolverClient {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: Vec::new(),
                deny_reason: None,
            },
        })
    }
}

/// Mock AuthZ resolver that always denies access for testing.
pub struct DenyingAuthZResolverClient;

#[async_trait]
impl AuthZResolverClient for DenyingAuthZResolverClient {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext {
                constraints: Vec::new(),
                deny_reason: None,
            },
        })
    }
}

/// Records all evaluation requests for post-hoc inspection.
/// Configurable decision (default: allow).
pub struct CapturingAuthZResolverClient {
    pub requests: Arc<Mutex<Vec<EvaluationRequest>>>,
    decision: bool,
}

impl CapturingAuthZResolverClient {
    /// Create a new allowing [`CapturingAuthZResolverClient`].
    pub fn new() -> Self {
        Self {
            requests: Arc::new(Mutex::new(vec![])),
            decision: true,
        }
    }

    /// Create a denying variant that records requests and returns `Deny`.
    pub fn denying() -> Self {
        Self {
            decision: false,
            ..Self::new()
        }
    }

    /// Return a snapshot of all recorded evaluation requests.
    pub fn recorded(&self) -> Vec<EvaluationRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Default for CapturingAuthZResolverClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AuthZResolverClient for CapturingAuthZResolverClient {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        self.requests.lock().unwrap().push(request);
        Ok(EvaluationResponse {
            decision: self.decision,
            context: EvaluationResponseContext {
                constraints: Vec::new(),
                deny_reason: None,
            },
        })
    }
}

/// Mock `CredStoreClientV1` for tests. Stores secrets in memory keyed by
/// the bare secret name (without `cred://` prefix).
pub struct MockCredStoreClient {
    store: HashMap<String, Vec<u8>>,
}

impl MockCredStoreClient {
    /// Create a mock pre-loaded with secrets.
    ///
    /// Keys may optionally include the `cred://` prefix — it is stripped
    /// automatically so lookups work regardless of the prefix convention.
    pub fn with_secrets(creds: Vec<(String, String)>) -> Self {
        let store = creds
            .into_iter()
            .map(|(k, v)| {
                let key = k.strip_prefix("cred://").unwrap_or(k.as_str()).to_string();
                (key, v.into_bytes())
            })
            .collect();
        Self { store }
    }

    /// Create an empty mock (all lookups return `Ok(None)`).
    pub fn empty() -> Self {
        Self {
            store: HashMap::new(),
        }
    }
}

#[async_trait]
impl CredStoreClientV1 for MockCredStoreClient {
    async fn get(
        &self,
        _ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<GetSecretResponse>, CredStoreError> {
        Ok(self.store.get(key.as_ref()).map(|v| GetSecretResponse {
            value: SecretValue::new(v.clone()),
            owner_tenant_id: CredstoreTenantId::nil(),
            sharing: SharingMode::default(),
            is_inherited: false,
        }))
    }
}

/// Mock `CredStoreClientV1` that always returns `CredStoreError::Internal`.
/// Useful for testing error-handling paths.
#[cfg(test)]
pub struct FailingCredStoreClient;

#[cfg(test)]
#[async_trait]
impl CredStoreClientV1 for FailingCredStoreClient {
    async fn get(
        &self,
        _ctx: &SecurityContext,
        _key: &SecretRef,
    ) -> Result<Option<GetSecretResponse>, CredStoreError> {
        Err(CredStoreError::Internal("backend failure".into()))
    }
}

/// Re-export for tests that need a `CredStoreClientV1` mock.
pub use MockCredStoreClient as TestCredStoreClient;

/// Mock `TenantResolverClient` for tests.
///
/// By default operates in single-tenant mode: every tenant is a root with no
/// ancestors and no descendants.  Use [`MockTenantResolverClient::with_hierarchy`]
/// to configure a parent→child chain for hierarchy tests.
pub struct MockTenantResolverClient {
    /// Map from tenant_id → (TenantInfo, ordered ancestors [parent..root]).
    tenants: HashMap<TenantId, (TenantInfo, Vec<TenantRef>)>,
}

impl MockTenantResolverClient {
    /// Create a single-tenant resolver: any tenant_id is treated as a root
    /// tenant with no ancestors.
    pub fn single_tenant() -> Self {
        Self {
            tenants: HashMap::new(),
        }
    }

    /// Create a resolver with an explicit hierarchy.
    ///
    /// `chain` is ordered root-first: `[root, parent, child]`.  Each entry
    /// gets ancestors derived automatically from its position in the chain.
    pub fn with_hierarchy(chain: Vec<TenantId>) -> Self {
        let mut tenants = HashMap::new();
        for (i, &id) in chain.iter().enumerate() {
            let parent_id = if i == 0 { None } else { Some(chain[i - 1]) };
            let info = TenantInfo {
                id,
                name: format!("tenant-{}", &id.to_string()[..8]),
                status: TenantStatus::Active,
                tenant_type: None,
                parent_id,
                self_managed: false,
            };
            // Ancestors for this tenant: walk backwards from parent to root.
            let ancestors: Vec<TenantRef> = (0..i)
                .rev()
                .map(|j| {
                    let anc_id = chain[j];
                    let anc_parent = if j == 0 { None } else { Some(chain[j - 1]) };
                    TenantRef {
                        id: anc_id,
                        status: TenantStatus::Active,
                        tenant_type: None,
                        parent_id: anc_parent,
                        self_managed: false,
                    }
                })
                .collect();
            tenants.insert(id, (info, ancestors));
        }
        Self { tenants }
    }
}

#[async_trait]
impl TenantResolverClient for MockTenantResolverClient {
    async fn get_tenant(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        if let Some((info, _)) = self.tenants.get(&id) {
            return Ok(info.clone());
        }
        // Single-tenant fallback: synthesize a root tenant.
        Ok(TenantInfo {
            id,
            name: format!("tenant-{}", &id.to_string()[..8]),
            status: TenantStatus::Active,
            tenant_type: None,
            parent_id: None,
            self_managed: false,
        })
    }

    async fn get_tenants(
        &self,
        ctx: &SecurityContext,
        ids: &[TenantId],
        _options: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        let mut result = Vec::new();
        for &id in ids {
            result.push(self.get_tenant(ctx, id).await?);
        }
        Ok(result)
    }

    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
        _options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        if let Some((info, ancestors)) = self.tenants.get(&id) {
            return Ok(GetAncestorsResponse {
                tenant: TenantRef::from(info.clone()),
                ancestors: ancestors.clone(),
            });
        }
        // Single-tenant fallback: root tenant with no ancestors.
        Ok(GetAncestorsResponse {
            tenant: TenantRef {
                id,
                status: TenantStatus::Active,
                tenant_type: None,
                parent_id: None,
                self_managed: false,
            },
            ancestors: vec![],
        })
    }

    async fn get_descendants(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
        _options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        let tenant_ref = if let Some((info, _)) = self.tenants.get(&id) {
            TenantRef::from(info.clone())
        } else {
            TenantRef {
                id,
                status: TenantStatus::Active,
                tenant_type: None,
                parent_id: None,
                self_managed: false,
            }
        };
        // Collect children from the hierarchy map.
        let descendants: Vec<TenantRef> = self
            .tenants
            .values()
            .filter(|(info, _)| info.parent_id == Some(id))
            .map(|(info, _)| TenantRef::from(info.clone()))
            .collect();
        Ok(GetDescendantsResponse {
            tenant: tenant_ref,
            descendants,
        })
    }

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        ancestor_id: TenantId,
        descendant_id: TenantId,
        _options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        if ancestor_id == descendant_id {
            return Ok(false);
        }
        if let Some((_, ancestors)) = self.tenants.get(&descendant_id) {
            return Ok(ancestors.iter().any(|a| a.id == ancestor_id));
        }
        Ok(false)
    }
}

/// Re-export plugin ID constants for test configurations.
pub use crate::domain::gts_helpers::{
    APIKEY_AUTH_PLUGIN_ID, OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID,
    OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID,
};

/// Builder for a fully-wired Control Plane test environment.
pub struct TestCpBuilder {
    credentials: Vec<(String, String)>,
    tenant_resolver: Option<MockTenantResolverClient>,
}

impl TestCpBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            credentials: Vec::new(),
            tenant_resolver: None,
        }
    }

    /// Pre-load credentials into the mock credstore client.
    #[must_use]
    pub fn with_credentials(mut self, creds: Vec<(String, String)>) -> Self {
        self.credentials = creds;
        self
    }

    /// Override the tenant resolver (for hierarchy tests).
    #[must_use]
    pub fn with_tenant_resolver(mut self, resolver: MockTenantResolverClient) -> Self {
        self.tenant_resolver = Some(resolver);
        self
    }

    /// Create repos, service, and mock credstore, register them in the
    /// provided `ClientHub`, and return the CP service trait object.
    pub(crate) fn build_and_register(self, hub: &ClientHub) -> Arc<dyn ControlPlaneService> {
        let upstream_repo = Arc::new(InMemoryUpstreamRepo::new());
        let route_repo = Arc::new(InMemoryRouteRepo::new());
        let tenant_resolver: Arc<dyn TenantResolverClient> = Arc::new(
            self.tenant_resolver
                .unwrap_or_else(MockTenantResolverClient::single_tenant),
        );
        let credstore: Arc<dyn CredStoreClientV1> =
            Arc::new(MockCredStoreClient::with_secrets(self.credentials));
        hub.register::<dyn CredStoreClientV1>(credstore.clone());

        let cp: Arc<dyn ControlPlaneService> = Arc::new(ControlPlaneServiceImpl::new(
            upstream_repo,
            route_repo,
            tenant_resolver,
            allow_all_enforcer(),
            credstore,
        ));

        cp
    }
}

impl Default for TestCpBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for a fully-wired Data Plane test environment.
///
/// Requires that a `CredStoreClientV1` is already registered in the
/// `ClientHub` (e.g., via `TestCpBuilder`).
pub struct TestDpBuilder {
    request_timeout: Option<Duration>,
    authz_client: Option<Arc<dyn AuthZResolverClient>>,
    backend_selector: Option<Arc<dyn EndpointSelector>>,
    max_body_size: Option<usize>,
    skip_upstream_tls_verify: bool,
    token_http_config: Option<modkit_http::HttpClientConfig>,
    token_cache_config: TokenCacheConfig,
}

impl TestDpBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            request_timeout: None,
            authz_client: None,
            backend_selector: None,
            max_body_size: None,
            skip_upstream_tls_verify: false,
            token_http_config: None,
            token_cache_config: TokenCacheConfig::default(),
        }
    }

    /// Override the request timeout (useful for timeout tests).
    #[must_use]
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    /// Override the AuthZ client (useful for authorization tests).
    #[must_use]
    pub fn with_authz_client(mut self, client: Arc<dyn AuthZResolverClient>) -> Self {
        self.authz_client = Some(client);
        self
    }

    /// Override the maximum request body size (useful for body-limit tests).
    #[must_use]
    pub fn with_max_body_size(mut self, size: usize) -> Self {
        self.max_body_size = Some(size);
        self
    }

    /// Skip upstream TLS certificate verification. **Test use only.**
    #[must_use]
    pub fn with_skip_upstream_tls_verify(mut self, allow: bool) -> Self {
        self.skip_upstream_tls_verify = allow;
        self
    }

    /// Inject a shared `EndpointSelector` so callers can hold the same
    /// instance that the DP service uses (e.g. for `invalidate()` calls).
    #[must_use]
    pub(crate) fn with_backend_selector(mut self, selector: Arc<dyn EndpointSelector>) -> Self {
        self.backend_selector = Some(selector);
        self
    }

    /// Override the HTTP client config for OAuth2 token endpoints.
    /// Pass `HttpClientConfig::for_testing()` to allow plain HTTP in tests.
    #[must_use]
    pub fn with_token_http_config(mut self, config: modkit_http::HttpClientConfig) -> Self {
        self.token_http_config = Some(config);
        self
    }

    /// Override the token cache configuration.
    #[must_use]
    pub fn with_token_cache_config(mut self, config: TokenCacheConfig) -> Self {
        self.token_cache_config = config;
        self
    }

    /// Fetch `CredStoreClientV1` from the hub, create a DP service with
    /// the given CP, and return the trait object.
    pub(crate) fn build_and_register(
        self,
        hub: &ClientHub,
        cp: Arc<dyn ControlPlaneService>,
    ) -> Arc<dyn DataPlaneService> {
        let credstore = hub
            .get::<dyn CredStoreClientV1>()
            .expect("CredStoreClientV1 must be registered before building DP");

        let authz_client = self
            .authz_client
            .unwrap_or_else(|| Arc::new(MockAuthZResolverClient));
        let policy_enforcer = PolicyEnforcer::new(authz_client);

        let server_conf = Arc::new(pingora_core::server::configuration::ServerConf::default());
        let pingora_proxy = crate::infra::proxy::pingora_proxy::PingoraProxy::new(
            Duration::from_secs(10),
            Duration::from_secs(30),
        )
        .with_skip_upstream_tls_verify(self.skip_upstream_tls_verify);
        let proxy = Arc::new(crate::infra::proxy::pingora_proxy::new_http_proxy(
            &server_conf,
            pingora_proxy,
        ));

        let backend_selector: Arc<dyn EndpointSelector> =
            self.backend_selector.unwrap_or_else(|| {
                Arc::new(crate::infra::proxy::pingora_proxy::PingoraEndpointSelector::new())
            });

        let mut svc = DataPlaneServiceImpl::new(
            cp,
            credstore,
            policy_enforcer,
            self.token_http_config,
            self.token_cache_config,
            backend_selector,
            proxy,
        )
        .with_allow_http_upstream(true);
        if let Some(timeout) = self.request_timeout {
            svc = svc.with_request_timeout(timeout);
        }
        if let Some(size) = self.max_body_size {
            svc = svc.with_max_body_size(size);
        }

        Arc::new(svc)
    }
}

impl Default for TestDpBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Test harness providing both an `AppState` (for REST handlers) and a
/// `ServiceGatewayClientV1` facade (for programmatic data setup in tests).
pub struct TestAppState {
    pub state: crate::module::AppState,
    pub facade: Arc<dyn ServiceGatewayClientV1>,
}

/// Build an `AppState` and facade for integration tests.
///
/// Use `result.state` when constructing an axum test router and
/// `result.facade` when you need to create data programmatically
/// (e.g. `facade.create_upstream(…)`).
pub fn build_test_app_state(
    hub: &ClientHub,
    cp_builder: TestCpBuilder,
    dp_builder: TestDpBuilder,
) -> TestAppState {
    let backend_selector: Arc<dyn EndpointSelector> =
        Arc::new(crate::infra::proxy::pingora_proxy::PingoraEndpointSelector::new());
    let cp = cp_builder.build_and_register(hub);
    let dp = dp_builder
        .with_backend_selector(backend_selector.clone())
        .build_and_register(hub, cp.clone());
    let facade: Arc<dyn ServiceGatewayClientV1> =
        Arc::new(ServiceGatewayClientV1Facade::new(cp.clone(), dp.clone()));
    hub.register::<dyn ServiceGatewayClientV1>(facade.clone());
    TestAppState {
        state: crate::module::AppState {
            cp,
            dp,
            backend_selector,
            config: crate::config::RuntimeConfig {
                max_body_size_bytes: 100 * 1024 * 1024, // 100 MB default for tests
            },
        },
        facade,
    }
}

/// Build a fully wired `ServiceGatewayClientV1` facade for integration tests.
/// Returns the facade registered in `client_hub`.
pub fn build_test_gateway(
    hub: &ClientHub,
    cp_builder: TestCpBuilder,
    dp_builder: TestDpBuilder,
) -> Arc<dyn ServiceGatewayClientV1> {
    let cp = cp_builder.build_and_register(hub);
    let dp = dp_builder.build_and_register(hub, cp.clone());
    let oagw: Arc<dyn ServiceGatewayClientV1> = Arc::new(ServiceGatewayClientV1Facade::new(cp, dp));
    hub.register::<dyn ServiceGatewayClientV1>(oagw.clone());
    oagw
}
