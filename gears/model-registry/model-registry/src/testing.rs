//! Test doubles for the gear's dependency seams: Tenant Resolver, PDP, the
//! provider type schema source, and the `CredStore` secret reference source. Used by the unit and integration tests.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, EqPredicate, Predicate};
use authz_resolver_sdk::{
    AuthZResolverApi, EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use model_registry_sdk::{UpdateModelRequestV1, UpdateProviderRequestV1};
use tenant_resolver_sdk::{
    BarrierMode, GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions,
    GetDescendantsResponse, GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo, TenantRef,
    TenantResolverClient, TenantResolverError, TenantStatus,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use types_registry_sdk::{LifecycleStatus, TypeSchema};
use uuid::Uuid;

use crate::domain::credential_check::{OwnRowState, SecretRefSource};
use crate::domain::error::DomainError;
use crate::domain::gts_validator::SchemaSource;

/// A caller in `tenant` with a fresh subject id.
#[must_use]
pub fn security_context(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant)
        .build()
        .unwrap_or_else(|e| panic!("security context: {e}"))
}

/// A caller in `tenant` acting as `subject`.
#[must_use]
pub fn security_context_for(tenant: Uuid, subject: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .build()
        .unwrap_or_else(|e| panic!("security context: {e}"))
}

fn tenant_ref(id: Uuid, parent: Option<Uuid>) -> TenantRef {
    TenantRef {
        id: TenantId(id),
        status: TenantStatus::Active,
        tenant_type: None,
        parent_id: parent.map(TenantId),
        self_managed: false,
    }
}

type ErrorFactory = Box<dyn Fn() -> TenantResolverError + Send + Sync>;

/// Tenant Resolver double: configured ancestor chains, an optional failure,
/// an optional delay, and call accounting.
#[derive(Default)]
pub struct StaticTenantResolver {
    chains: HashMap<Uuid, Vec<Uuid>>,
    self_managed: HashSet<Uuid>,
    on_call: Option<Box<dyn Fn() + Send + Sync>>,
    failure: Option<ErrorFactory>,
    delay: Option<Duration>,
    calls: AtomicUsize,
    last_barrier_mode: Mutex<Option<BarrierMode>>,
}

impl StaticTenantResolver {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `ancestors` is parent first, root last (the tenant itself excluded).
    #[must_use]
    pub fn with_chain(mut self, tenant: Uuid, ancestors: Vec<Uuid>) -> Self {
        self.chains.insert(tenant, ancestors);
        self
    }

    /// Marks a tenant self-managed: under `BarrierMode::Respect` its own
    /// chain is empty and the chain of a tenant below it ends at it.
    #[must_use]
    pub fn with_self_managed(mut self, tenant: Uuid) -> Self {
        self.self_managed.insert(tenant);
        self
    }

    /// Runs `hook` during every `get_ancestors` call.
    #[must_use]
    pub fn on_call(mut self, hook: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_call = Some(Box::new(hook));
        self
    }

    #[must_use]
    pub fn failing_with(
        mut self,
        factory: impl Fn() -> TenantResolverError + Send + Sync + 'static,
    ) -> Self {
        self.failure = Some(Box::new(factory));
        self
    }

    #[must_use]
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn last_barrier_mode(&self) -> Option<BarrierMode> {
        *self
            .last_barrier_mode
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[async_trait]
impl TenantResolverClient for StaticTenantResolver {
    async fn get_tenant(
        &self,
        _: &SecurityContext,
        _: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        Err(TenantResolverError::Internal("not used".into()))
    }

    async fn get_root_tenant(
        &self,
        _: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        Err(TenantResolverError::Internal("not used".into()))
    }

    async fn get_tenants(
        &self,
        _: &SecurityContext,
        _: &[TenantId],
        _: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        Err(TenantResolverError::Internal("not used".into()))
    }

    async fn get_ancestors(
        &self,
        _: &SecurityContext,
        tenant: TenantId,
        options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self
            .last_barrier_mode
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(options.barrier_mode);
        if let Some(hook) = &self.on_call {
            hook();
        }
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        if let Some(failure) = &self.failure {
            return Err(failure());
        }
        let configured = self
            .chains
            .get(&tenant.0)
            .ok_or(TenantResolverError::TenantNotFound { tenant_id: tenant })?;
        let ancestors: Vec<Uuid> = match options.barrier_mode {
            BarrierMode::Ignore => configured.clone(),
            BarrierMode::Respect if self.self_managed.contains(&tenant.0) => Vec::new(),
            BarrierMode::Respect => {
                let barrier = configured
                    .iter()
                    .position(|id| self.self_managed.contains(id));
                configured[..barrier.map_or(configured.len(), |i| i + 1)].to_vec()
            }
        };
        let mut refs = Vec::with_capacity(ancestors.len());
        for (i, id) in ancestors.iter().enumerate() {
            refs.push(tenant_ref(*id, ancestors.get(i + 1).copied()));
        }
        Ok(GetAncestorsResponse {
            tenant: tenant_ref(tenant.0, ancestors.first().copied()),
            ancestors: refs,
        })
    }

    async fn get_descendants(
        &self,
        _: &SecurityContext,
        _: TenantId,
        _: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        Err(TenantResolverError::Internal("not used".into()))
    }

    async fn is_ancestor(
        &self,
        _: &SecurityContext,
        _: TenantId,
        _: TenantId,
        _: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        Err(TenantResolverError::Internal("not used".into()))
    }
}

type Decider =
    Box<dyn Fn(&EvaluationRequest) -> Result<EvaluationResponse, CanonicalError> + Send + Sync>;

/// PDP double driven by a closure over the evaluation request; records every
/// request it saw.
pub struct StaticAuthz {
    decider: Decider,
    delay: Option<Duration>,
    requests: Mutex<Vec<EvaluationRequest>>,
}

impl StaticAuthz {
    #[must_use]
    pub fn with(
        decider: impl Fn(&EvaluationRequest) -> Result<EvaluationResponse, CanonicalError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            decider: Box::new(decider),
            delay: None,
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Permits everything with one `owner_tenant_id = caller tenant` constraint.
    #[must_use]
    pub fn permit_own_tenant() -> Self {
        Self::with(|req| Ok(permit_with(vec![own_tenant_constraint(req)])))
    }

    /// Permits everything with no constraints.
    #[must_use]
    pub fn permit_unconstrained() -> Self {
        Self::with(|_| Ok(permit_with(Vec::new())))
    }

    /// Denies everything.
    #[must_use]
    pub fn deny_all() -> Self {
        Self::with(|_| Ok(deny()))
    }

    /// Fails every evaluation as a transport error.
    #[must_use]
    pub fn failing() -> Self {
        Self::with(|_| {
            Err(CanonicalError::service_unavailable()
                .with_detail("pdp down")
                .create())
        })
    }

    #[must_use]
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    #[must_use]
    pub fn requests(&self) -> Vec<EvaluationRequest> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Actions seen so far, in order.
    #[must_use]
    pub fn actions(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|r| r.action.name.clone())
            .collect()
    }
}

/// The subject tenant the enforcer put on the request.
#[must_use]
pub fn subject_tenant(req: &EvaluationRequest) -> Uuid {
    req.subject
        .properties
        .get("tenant_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_default()
}

#[must_use]
pub fn own_tenant_constraint(req: &EvaluationRequest) -> Constraint {
    Constraint {
        predicates: vec![Predicate::Eq(EqPredicate::new(
            "owner_tenant_id",
            subject_tenant(req),
        ))],
    }
}

#[must_use]
pub fn permit_with(constraints: Vec<Constraint>) -> EvaluationResponse {
    EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints,
            deny_reason: None,
        },
    }
}

#[must_use]
pub fn deny() -> EvaluationResponse {
    EvaluationResponse {
        decision: false,
        context: EvaluationResponseContext {
            constraints: Vec::new(),
            deny_reason: None,
        },
    }
}

#[async_trait]
impl AuthZResolverApi for StaticAuthz {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.clone());
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        (self.decider)(&request)
    }
}

type SchemaFailure = Box<dyn Fn() -> DomainError + Send + Sync>;

/// A Types Registry snapshot of an active provider type with `resolved` as
/// its resolved schema; `content` carries `x-gts-abstract` when `is_abstract`.
#[must_use]
pub fn provider_type_schema(
    type_id: &str,
    resolved: serde_json::Value,
    is_abstract: bool,
) -> TypeSchema {
    let content = if is_abstract {
        serde_json::json!({ "x-gts-abstract": true })
    } else {
        serde_json::json!({})
    };
    TypeSchema {
        type_id: gts::GtsTypeId::new(type_id),
        type_uuid: Uuid::nil(),
        lifecycle_status: LifecycleStatus::Active,
        origin: None,
        content: Some(content),
        resolved_schema: Some(resolved),
        effective_traits: None,
        effective_traits_schema: None,
        provenance: None,
    }
}

/// Provider type schema double: a map of type id to Types Registry snapshot.
#[derive(Default)]
pub struct StaticSchemaSource {
    schemas: HashMap<String, TypeSchema>,
    failure: Option<SchemaFailure>,
    delay: Option<Duration>,
    calls: AtomicUsize,
}

impl StaticSchemaSource {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `type_id` with a resolved schema whose `provider_settings`
    /// subschema is `settings_schema`.
    #[must_use]
    pub fn with_type(self, type_id: &str, settings_schema: &serde_json::Value) -> Self {
        let resolved = serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {
                "gts_type": { "type": "string" },
                "provider_settings": settings_schema
            }
        });
        self.with_type_schema(provider_type_schema(type_id, resolved, false))
    }

    /// Registers `type_id` with an open `provider_settings`.
    #[must_use]
    pub fn with_open_type(self, type_id: &str) -> Self {
        self.with_type(type_id, &serde_json::json!({ "type": "object" }))
    }

    /// Registers a prepared snapshot under its type id.
    #[must_use]
    pub fn with_type_schema(mut self, schema: TypeSchema) -> Self {
        self.schemas.insert(schema.type_id.to_string(), schema);
        self
    }

    #[must_use]
    pub fn failing_with(
        mut self,
        factory: impl Fn() -> DomainError + Send + Sync + 'static,
    ) -> Self {
        self.failure = Some(Box::new(factory));
        self
    }

    #[must_use]
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SchemaSource for StaticSchemaSource {
    async fn type_schema(
        &self,
        _ctx: &SecurityContext,
        type_id: &gts::GtsTypeId,
    ) -> Result<Option<TypeSchema>, DomainError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        if let Some(failure) = &self.failure {
            return Err(failure());
        }
        Ok(self.schemas.get(type_id.as_ref()).cloned())
    }
}

type SecretRefFailure = Box<dyn Fn() -> DomainError + Send + Sync>;

/// `CredStore` double: a map of secret reference to own-row state, an
/// optional failure, an optional delay, and call accounting. Unknown
/// references are `Missing`.
#[derive(Default)]
pub struct StaticSecretRefSource {
    states: HashMap<String, OwnRowState>,
    failure: Option<SecretRefFailure>,
    delay: Option<Duration>,
    calls: AtomicUsize,
    seen: Mutex<Vec<String>>,
}

impl StaticSecretRefSource {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_ref(mut self, secret_ref: &str, state: OwnRowState) -> Self {
        self.states.insert(secret_ref.to_owned(), state);
        self
    }

    /// Registers `secret_ref` as an own, active secret.
    #[must_use]
    pub fn owned(self, secret_ref: &str) -> Self {
        self.with_ref(secret_ref, OwnRowState::Active)
    }

    #[must_use]
    pub fn failing_with(
        mut self,
        factory: impl Fn() -> DomainError + Send + Sync + 'static,
    ) -> Self {
        self.failure = Some(Box::new(factory));
        self
    }

    #[must_use]
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// The references read so far, in call order.
    #[must_use]
    pub fn seen(&self) -> Vec<String> {
        self.seen.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl SecretRefSource for StaticSecretRefSource {
    async fn own_row_state(
        &self,
        _ctx: &SecurityContext,
        secret_ref: &str,
    ) -> Result<OwnRowState, DomainError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(secret_ref.to_owned());
        }
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        if let Some(failure) = &self.failure {
            return Err(failure());
        }
        Ok(self
            .states
            .get(secret_ref)
            .copied()
            .unwrap_or(OwnRowState::Missing))
    }
}

/// Shares a double behind `Arc` while keeping a handle for assertions.
#[must_use]
pub fn shared<T>(value: T) -> (Arc<T>, Arc<T>) {
    let a = Arc::new(value);
    (Arc::clone(&a), a)
}

/// An update request with only the fields `set` assigns.
pub fn provider_update(set: impl FnOnce(&mut UpdateProviderRequestV1)) -> UpdateProviderRequestV1 {
    let mut req = UpdateProviderRequestV1::default();
    set(&mut req);
    req
}

/// An update request with only the fields `set` assigns.
pub fn model_update(set: impl FnOnce(&mut UpdateModelRequestV1)) -> UpdateModelRequestV1 {
    let mut req = UpdateModelRequestV1::default();
    set(&mut req);
    req
}
