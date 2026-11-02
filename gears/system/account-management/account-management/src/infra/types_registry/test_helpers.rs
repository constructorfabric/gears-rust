//! Test helpers shared across crate-internal tests of the GTS Types
//! Registry adapters. Compiled under `#[cfg(test)]` only.

#![cfg(test)]

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_gts::{gts_id, gts_uri};
use types_registry_sdk::testing_platform::MockTypesRegistry;
use uuid::Uuid;

/// An error scope for the failures a fake registry answers with.
#[toolkit_canonical_errors::resource_error(gts_id!("cf.core.types_registry.entity.v1~"))]
struct Catalogue;

/// Every availability failure
/// [`DomainError::platform_registry_failure`](crate::domain::error::DomainError::platform_registry_failure)
/// treats as an outage.
pub fn outages() -> [CanonicalError; 4] {
    [
        CanonicalError::service_unavailable().create(),
        Catalogue::deadline_exceeded("read timed out").create(),
        Catalogue::resource_exhausted("rate limited")
            .with_quota_violation("read", "rate limit")
            .create(),
        Catalogue::aborted("read aborted")
            .with_reason("READ_ABORTED")
            .create(),
    ]
}

/// A registry's refusals of the caller's credential, and the HTTP status each
/// must keep at AM's boundary.
pub fn refusals() -> [(CanonicalError, u16); 2] {
    [
        (
            CanonicalError::unauthenticated()
                .with_reason("TOKEN_EXPIRED")
                .create(),
            401,
        ),
        (
            Catalogue::permission_denied()
                .with_reason("READ_NOT_GRANTED")
                .create(),
            403,
        ),
    ]
}

/// The category and HTTP status `err`, a domain error, has at AM's boundary.
pub fn at_the_boundary(err: crate::domain::error::DomainError) -> (&'static str, Option<u16>) {
    let public = CanonicalError::from(err);
    let category = public.gts_type();
    (
        category,
        toolkit_canonical_errors::Problem::from(public).status,
    )
}

/// A registry holding what platform bootstrap reads: the linked inventory
/// (the AM tenant-type envelope among it), the platform root tenant type
/// with no allowed parents, and the AM tenant projection schema pinning
/// `name` to `minLength: 1, maxLength: 255`.
pub fn bootstrap_registry() -> Arc<MockTypesRegistry> {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed_inventory();
    registry.seed(
        gts_id!("cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"),
        serde_json::json!({
            "$id": gts_uri!("cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [{ "$ref": gts_uri!("cf.core.am.tenant_type.v1~") }],
            "x-gts-traits": { "allowed_parent_types": [] },
        }),
    );
    registry.seed(
        gts_id!("cf.core.am.tenant.v1~"),
        serde_json::json!({
            "$id": gts_uri!("cf.core.am.tenant.v1~"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "required": ["id", "name"],
            "properties": {
                "id": { "type": "string", "format": "uuid" },
                "name": { "type": "string", "minLength": 1, "maxLength": 255 },
                "parent_id": { "type": ["string", "null"], "format": "uuid" },
            },
        }),
    );
    registry
}

/// A platform registry whose batch reads answer snapshots edited after the
/// read: what admission never stores (a trait value its traits schema
/// refuses) or what a broken client might answer (a selected document
/// missing), to exercise adapters' defensive branches.
pub struct Tampered {
    inner: Arc<types_registry_sdk::testing_platform::MockTypesRegistry>,
    edit: Box<SnapshotEdit>,
}

type SnapshotEdit = dyn Fn(&mut types_registry_sdk::Entity) + Send + Sync;

impl Tampered {
    pub fn new(
        inner: Arc<types_registry_sdk::testing_platform::MockTypesRegistry>,
        edit: impl Fn(&mut types_registry_sdk::Entity) + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner,
            edit: Box::new(edit),
        }
    }

    /// Answers without the documents the caller selected — content and every
    /// materialization.
    pub fn without_documents(
        inner: Arc<types_registry_sdk::testing_platform::MockTypesRegistry>,
    ) -> Self {
        Self::new(inner, |entity| {
            entity.content = None;
            entity.resolved_schema = None;
            entity.effective_traits = None;
            entity.effective_traits_schema = None;
        })
    }
}

#[async_trait]
impl types_registry_sdk::PlatformTypesRegistryApi for Tampered {
    async fn batch_get_entities(
        &self,
        ctx: &toolkit_security::PlatformSecurityContext,
        request: types_registry_sdk::BatchGetEntitiesRequest,
    ) -> Result<types_registry_sdk::BatchGetEntitiesResponse, CanonicalError> {
        let mut response = self.inner.batch_get_entities(ctx, request).await?;
        for lookup in response.0.values_mut() {
            if let types_registry_sdk::EntityLookup::Found { entity, .. } = lookup {
                (self.edit)(entity);
            }
        }
        Ok(response)
    }

    async fn list_entities(
        &self,
        ctx: &toolkit_security::PlatformSecurityContext,
        request: types_registry_sdk::ListEntitiesRequest,
    ) -> Result<types_registry_sdk::ListEntitiesResponse, CanonicalError> {
        self.inner.list_entities(ctx, request).await
    }

    async fn register_entities(
        &self,
        ctx: &toolkit_security::PlatformSecurityContext,
        key: types_registry_sdk::IdempotencyKey,
        request: types_registry_sdk::RegisterEntitiesRequest,
    ) -> Result<types_registry_sdk::RegistrationOperation, CanonicalError> {
        self.inner.register_entities(ctx, key, request).await
    }

    async fn delete_entities(
        &self,
        ctx: &toolkit_security::PlatformSecurityContext,
        key: types_registry_sdk::IdempotencyKey,
        request: types_registry_sdk::DeleteEntitiesRequest,
    ) -> Result<types_registry_sdk::DeletionOperation, CanonicalError> {
        self.inner.delete_entities(ctx, key, request).await
    }

    async fn get_operation(
        &self,
        ctx: &toolkit_security::PlatformSecurityContext,
        operation_id: Uuid,
    ) -> Result<types_registry_sdk::Operation, CanonicalError> {
        self.inner.get_operation(ctx, operation_id).await
    }
}

/// Fixture for opaque tenant-type UUIDs: every UUID read returns the same typed
/// schema with the requested UUID; identifier reads return `NotFound`. This leaves
/// AM resource schemas absent and short-circuits their optional validation.
pub struct ConstantTypeRegistry(pub &'static str);

fn not_implemented() -> CanonicalError {
    CanonicalError::internal("not implemented for the constant test registry").create()
}

#[async_trait]
impl types_registry_sdk::PlatformTypesRegistryApi for ConstantTypeRegistry {
    async fn batch_get_entities(
        &self,
        _ctx: &toolkit_security::PlatformSecurityContext,
        request: types_registry_sdk::BatchGetEntitiesRequest,
    ) -> Result<types_registry_sdk::BatchGetEntitiesResponse, CanonicalError> {
        let gts_id = types_registry_sdk::GtsId::try_new(self.0).expect("valid id");
        let answers = request
            .items
            .into_iter()
            .map(|item| {
                let lookup = match item.key {
                    types_registry_sdk::EntityKey::GtsUuid(uuid) => {
                        types_registry_sdk::EntityLookup::Found {
                            entity: Box::new(types_registry_sdk::Entity {
                                gts_id: gts_id.clone(),
                                gts_uuid: uuid,
                                kind: types_registry_sdk::EntityKind::TypeSchema,
                                lifecycle_status: types_registry_sdk::LifecycleStatus::Active,
                                origin: None,
                                content: None,
                                resolved_schema: None,
                                effective_traits: None,
                                effective_traits_schema: None,
                                provenance: None,
                            }),
                            etag: types_registry_sdk::Validator::from_bytes("constant"),
                        }
                    }
                    types_registry_sdk::EntityKey::GtsId(_) => {
                        types_registry_sdk::EntityLookup::NotFound
                    }
                };
                (item.key, lookup)
            })
            .collect();
        Ok(types_registry_sdk::BatchGetEntitiesResponse(answers))
    }

    async fn list_entities(
        &self,
        _ctx: &toolkit_security::PlatformSecurityContext,
        _request: types_registry_sdk::ListEntitiesRequest,
    ) -> Result<types_registry_sdk::ListEntitiesResponse, CanonicalError> {
        Err(not_implemented())
    }

    async fn register_entities(
        &self,
        _ctx: &toolkit_security::PlatformSecurityContext,
        _key: types_registry_sdk::IdempotencyKey,
        _request: types_registry_sdk::RegisterEntitiesRequest,
    ) -> Result<types_registry_sdk::RegistrationOperation, CanonicalError> {
        Err(not_implemented())
    }

    async fn delete_entities(
        &self,
        _ctx: &toolkit_security::PlatformSecurityContext,
        _key: types_registry_sdk::IdempotencyKey,
        _request: types_registry_sdk::DeleteEntitiesRequest,
    ) -> Result<types_registry_sdk::DeletionOperation, CanonicalError> {
        Err(not_implemented())
    }

    async fn get_operation(
        &self,
        _ctx: &toolkit_security::PlatformSecurityContext,
        _operation_id: Uuid,
    ) -> Result<types_registry_sdk::Operation, CanonicalError> {
        Err(not_implemented())
    }
}

/// The API a [`CallerSpy`] refuses: calling it panics the test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forbid {
    /// A request path: every read must name the caller, so the platform
    /// API is never reached.
    Platform,
    /// A platform operation (bootstrap, retention, reaper): there is no
    /// caller to name, so the tenant API is never reached.
    Tenant,
}

/// One read the spy answered: the caller's full context for a tenant
/// read, nothing for a platform one.
#[derive(Debug, Clone)]
pub enum Seen {
    Tenant(toolkit_security::SecurityContext),
    Platform,
}

/// A registry that records who every read is made for and answers it
/// from `inner`. The SDK mock and the local client ignore the caller, so
/// only a spy shows that a request's own `SecurityContext` arrived — the
/// clone keeps the bearer token that serde would drop.
pub struct CallerSpy<R> {
    inner: Arc<R>,
    forbid: Forbid,
    seen: parking_lot::Mutex<Vec<Seen>>,
}

impl<R> CallerSpy<R> {
    /// A spy for request paths: tenant reads are recorded, a platform
    /// call panics.
    pub fn for_requests(inner: Arc<R>) -> Arc<Self> {
        Self::forbidding(inner, Forbid::Platform)
    }

    /// A spy for platform operations: platform reads are recorded, a
    /// tenant call panics.
    pub fn for_platform(inner: Arc<R>) -> Arc<Self> {
        Self::forbidding(inner, Forbid::Tenant)
    }

    fn forbidding(inner: Arc<R>, forbid: Forbid) -> Arc<Self> {
        Arc::new(Self {
            inner,
            forbid,
            seen: parking_lot::Mutex::new(Vec::new()),
        })
    }

    /// The contexts of every tenant read, in order.
    pub fn callers(&self) -> Vec<toolkit_security::SecurityContext> {
        self.seen
            .lock()
            .iter()
            .filter_map(|seen| match seen {
                Seen::Tenant(ctx) => Some(ctx.clone()),
                Seen::Platform => None,
            })
            .collect()
    }

    /// How many platform reads were made.
    pub fn platform_reads(&self) -> usize {
        self.seen
            .lock()
            .iter()
            .filter(|seen| matches!(seen, Seen::Platform))
            .count()
    }

    /// Forget what was seen, so a second caller's reads stand alone.
    pub fn clear(&self) {
        self.seen.lock().clear();
    }

    fn tenant(&self, ctx: &toolkit_security::SecurityContext) {
        assert_ne!(
            self.forbid,
            Forbid::Tenant,
            "a platform operation read through the tenant API"
        );
        self.seen.lock().push(Seen::Tenant(ctx.clone()));
    }

    fn platform(&self) {
        assert_ne!(
            self.forbid,
            Forbid::Platform,
            "a request path read through the platform API"
        );
        self.seen.lock().push(Seen::Platform);
    }
}

#[async_trait]
impl<R: types_registry_sdk::TypesRegistryApi + 'static> types_registry_sdk::TypesRegistryApi
    for CallerSpy<R>
{
    async fn batch_get_entities(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: types_registry_sdk::BatchGetEntitiesRequest,
    ) -> Result<types_registry_sdk::BatchGetEntitiesResponse, CanonicalError> {
        self.tenant(ctx);
        self.inner.batch_get_entities(ctx, request).await
    }

    async fn list_entities(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: types_registry_sdk::ListEntitiesRequest,
    ) -> Result<types_registry_sdk::ListEntitiesResponse, CanonicalError> {
        self.tenant(ctx);
        self.inner.list_entities(ctx, request).await
    }
}

#[async_trait]
impl<R: types_registry_sdk::PlatformTypesRegistryApi + 'static>
    types_registry_sdk::PlatformTypesRegistryApi for CallerSpy<R>
{
    async fn batch_get_entities(
        &self,
        ctx: &toolkit_security::PlatformSecurityContext,
        request: types_registry_sdk::BatchGetEntitiesRequest,
    ) -> Result<types_registry_sdk::BatchGetEntitiesResponse, CanonicalError> {
        self.platform();
        self.inner.batch_get_entities(ctx, request).await
    }

    async fn list_entities(
        &self,
        ctx: &toolkit_security::PlatformSecurityContext,
        request: types_registry_sdk::ListEntitiesRequest,
    ) -> Result<types_registry_sdk::ListEntitiesResponse, CanonicalError> {
        self.platform();
        self.inner.list_entities(ctx, request).await
    }

    async fn register_entities(
        &self,
        ctx: &toolkit_security::PlatformSecurityContext,
        key: types_registry_sdk::IdempotencyKey,
        request: types_registry_sdk::RegisterEntitiesRequest,
    ) -> Result<types_registry_sdk::RegistrationOperation, CanonicalError> {
        self.platform();
        self.inner.register_entities(ctx, key, request).await
    }

    async fn delete_entities(
        &self,
        ctx: &toolkit_security::PlatformSecurityContext,
        key: types_registry_sdk::IdempotencyKey,
        request: types_registry_sdk::DeleteEntitiesRequest,
    ) -> Result<types_registry_sdk::DeletionOperation, CanonicalError> {
        self.platform();
        self.inner.delete_entities(ctx, key, request).await
    }

    async fn get_operation(
        &self,
        ctx: &toolkit_security::PlatformSecurityContext,
        operation_id: Uuid,
    ) -> Result<types_registry_sdk::Operation, CanonicalError> {
        self.platform();
        self.inner.get_operation(ctx, operation_id).await
    }
}

/// A distinct, fully populated caller: subject, subject type, home
/// tenant, scopes and bearer token all derive from `n`.
pub fn caller(n: u128) -> toolkit_security::SecurityContext {
    caller_in(n, Uuid::from_u128(0x7E4A_4700_0000 + n))
}

/// As [`caller`], homed in `tenant`: for services whose PDP scope is the
/// caller's home subtree.
pub fn caller_in(n: u128, tenant: Uuid) -> toolkit_security::SecurityContext {
    toolkit_security::SecurityContext::builder()
        .subject_id(Uuid::from_u128(0xC0FF_EE00_0000 + n))
        .subject_type("user")
        .subject_tenant_id(tenant)
        .token_scopes(vec![format!("scope-{n}"), "am:read".to_owned()])
        .bearer_token(format!("bearer-{n}"))
        .build()
        .expect("a complete caller context")
}

/// Asserts every context the spy recorded is `expected` itself — the
/// bearer token included, which a serde round-trip would lose — and
/// that at least one was recorded.
pub fn assert_only_caller(
    seen: &[toolkit_security::SecurityContext],
    expected: &toolkit_security::SecurityContext,
) {
    use secrecy::ExposeSecret;
    assert!(!seen.is_empty(), "no tenant read named the caller");
    for ctx in seen {
        assert_eq!(ctx.subject_id(), expected.subject_id());
        assert_eq!(ctx.subject_type(), expected.subject_type());
        assert_eq!(ctx.subject_tenant_id(), expected.subject_tenant_id());
        assert_eq!(ctx.token_scopes(), expected.token_scopes());
        assert_eq!(
            ctx.bearer_token().map(ExposeSecret::expose_secret),
            expected.bearer_token().map(ExposeSecret::expose_secret),
            "the caller's bearer token was not forwarded"
        );
    }
}

#[async_trait]
impl types_registry_sdk::TypesRegistryApi for Tampered {
    async fn batch_get_entities(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        request: types_registry_sdk::BatchGetEntitiesRequest,
    ) -> Result<types_registry_sdk::BatchGetEntitiesResponse, CanonicalError> {
        types_registry_sdk::PlatformTypesRegistryApi::batch_get_entities(
            self,
            &toolkit_security::PlatformSecurityContext::outbound_marker(),
            request,
        )
        .await
    }

    async fn list_entities(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        request: types_registry_sdk::ListEntitiesRequest,
    ) -> Result<types_registry_sdk::ListEntitiesResponse, CanonicalError> {
        types_registry_sdk::PlatformTypesRegistryApi::list_entities(
            self,
            &toolkit_security::PlatformSecurityContext::outbound_marker(),
            request,
        )
        .await
    }
}

#[async_trait]
impl types_registry_sdk::TypesRegistryApi for ConstantTypeRegistry {
    async fn batch_get_entities(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        request: types_registry_sdk::BatchGetEntitiesRequest,
    ) -> Result<types_registry_sdk::BatchGetEntitiesResponse, CanonicalError> {
        types_registry_sdk::PlatformTypesRegistryApi::batch_get_entities(
            self,
            &toolkit_security::PlatformSecurityContext::outbound_marker(),
            request,
        )
        .await
    }

    async fn list_entities(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _request: types_registry_sdk::ListEntitiesRequest,
    ) -> Result<types_registry_sdk::ListEntitiesResponse, CanonicalError> {
        Err(not_implemented())
    }
}

/// Wires one test registry as both of a `TenantService`'s clients, for
/// tests that do not distinguish who a read is made for.
pub trait WithRegistry {
    #[must_use]
    fn with_registry<T>(self, registry: Arc<T>) -> Self
    where
        T: types_registry_sdk::TypesRegistryApi
            + types_registry_sdk::PlatformTypesRegistryApi
            + 'static;
}

impl<R: crate::domain::tenant::repo::TenantRepo> WithRegistry
    for crate::domain::tenant::service::TenantService<R>
{
    fn with_registry<T>(self, registry: Arc<T>) -> Self
    where
        T: types_registry_sdk::TypesRegistryApi
            + types_registry_sdk::PlatformTypesRegistryApi
            + 'static,
    {
        self.with_types_registry(Arc::clone(&registry) as _, registry as _)
    }
}
