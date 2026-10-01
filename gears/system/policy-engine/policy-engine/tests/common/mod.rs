//! Shared fixture of the gear-level integration tests: a config provider, a
//! types-registry fake (which, unlike `MockTypesRegistryClient`, accepts
//! `register` calls instead of panicking on them), a single-tenant
//! tenant-resolver fake,
//! an allow-all authz stub, and a `GearCtx` wired with them over a migrated
//! in-memory `SQLite` database.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverApi;
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use policy_engine::PolicyEngine;
use serde_json::{Value, json};
use tenant_resolver_sdk::{
    GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
    GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo, TenantResolverClient,
    TenantResolverError, TenantStatus,
};
use tokio_util::sync::CancellationToken;
use toolkit::config::ConfigProvider;
use toolkit::contracts::DatabaseCapability;
use toolkit::{ClientHub, GearCtx};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::migration_runner::run_migrations_for_gear;
use toolkit_db::{ConnectOpts, DBProvider, Db, DbError, connect_db};
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_type_schema};
use types_registry_sdk::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery, TypesRegistryClient,
};
use uuid::Uuid;

pub const GEAR: &str = "policy-engine";
pub const WIDGET: &str = "gts.cf.core.test.widget.v1~";
pub const TENANT: Uuid = Uuid::from_u128(0xA1);
pub const SUBJECT: Uuid = Uuid::from_u128(0x51);

/// A caller in [`TENANT`].
pub fn tenant_user() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(SUBJECT)
        .subject_tenant_id(TENANT)
        .build()
        .unwrap()
}

pub struct MockConfigProvider {
    gears: HashMap<String, Value>,
}

impl MockConfigProvider {
    pub fn new(config: &Value) -> Self {
        let mut gears = HashMap::new();
        gears.insert(GEAR.to_owned(), json!({ "config": config.clone() }));
        Self { gears }
    }
}

impl ConfigProvider for MockConfigProvider {
    fn get_gear_config(&self, gear_name: &str) -> Option<&Value> {
        self.gears.get(gear_name)
    }
}

/// A types-registry fake that knows [`WIDGET`] and accepts registration
/// (delegating every other call to a wrapped `MockTypesRegistryClient`,
/// which panics if `register` reaches it directly - this fake exists so
/// `register` does not panic in these tests).
pub struct FakeTypesRegistry {
    inner: MockTypesRegistryClient,
}

impl FakeTypesRegistry {
    pub fn new() -> Self {
        Self {
            inner: MockTypesRegistryClient::new()
                .with_type_schemas([make_test_type_schema(WIDGET)]),
        }
    }
}

#[async_trait]
impl TypesRegistryClient for FakeTypesRegistry {
    async fn register(&self, entities: Vec<Value>) -> Result<Vec<RegisterResult>, CanonicalError> {
        Ok(entities
            .iter()
            .map(|entity| {
                let id = entity["$id"].as_str().unwrap_or_default().to_owned();
                RegisterResult::Ok { gts_id: id }
            })
            .collect())
    }

    async fn register_type_schemas(
        &self,
        type_schemas: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        self.inner.register_type_schemas(type_schemas).await
    }

    async fn get_type_schema(&self, type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        self.inner.get_type_schema(type_id).await
    }

    async fn get_type_schema_by_uuid(&self, id: Uuid) -> Result<GtsTypeSchema, CanonicalError> {
        self.inner.get_type_schema_by_uuid(id).await
    }

    async fn get_type_schemas(
        &self,
        ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        self.inner.get_type_schemas(ids).await
    }

    async fn get_type_schemas_by_uuid(
        &self,
        ids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        self.inner.get_type_schemas_by_uuid(ids).await
    }

    async fn list_type_schemas(
        &self,
        query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        self.inner.list_type_schemas(query).await
    }

    async fn register_instances(
        &self,
        instances: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        self.inner.register_instances(instances).await
    }

    async fn get_instance(&self, id: &str) -> Result<GtsInstance, CanonicalError> {
        self.inner.get_instance(id).await
    }

    async fn get_instance_by_uuid(&self, id: Uuid) -> Result<GtsInstance, CanonicalError> {
        self.inner.get_instance_by_uuid(id).await
    }

    async fn get_instances(
        &self,
        ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, CanonicalError>> {
        self.inner.get_instances(ids).await
    }

    async fn get_instances_by_uuid(
        &self,
        ids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        self.inner.get_instances_by_uuid(ids).await
    }

    async fn list_instances(
        &self,
        query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        self.inner.list_instances(query).await
    }
}

/// A single non-root, non-self-managed tenant ([`TENANT`]), reachable from
/// itself: the tree has exactly one node, so every ancestry and reachability
/// read answers about that one tenant.
pub struct FakeTenantResolver {
    tenant: Uuid,
}

impl FakeTenantResolver {
    pub fn new(tenant: Uuid) -> Self {
        Self { tenant }
    }

    fn info(&self) -> TenantInfo {
        TenantInfo {
            id: TenantId(self.tenant),
            name: "tenant".to_owned(),
            status: TenantStatus::Active,
            tenant_type: None,
            parent_id: None,
            self_managed: false,
        }
    }

    fn not_found(id: TenantId) -> TenantResolverError {
        TenantResolverError::TenantNotFound { tenant_id: id }
    }
}

#[async_trait]
impl TenantResolverClient for FakeTenantResolver {
    async fn get_tenant(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        if id.0 == self.tenant {
            Ok(self.info())
        } else {
            Err(Self::not_found(id))
        }
    }

    async fn get_root_tenant(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        Ok(self.info())
    }

    async fn get_tenants(
        &self,
        _ctx: &SecurityContext,
        ids: &[TenantId],
        _options: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        Ok(ids
            .iter()
            .filter(|id| id.0 == self.tenant)
            .map(|_| self.info())
            .collect())
    }

    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
        _options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        if id.0 != self.tenant {
            return Err(Self::not_found(id));
        }
        Ok(GetAncestorsResponse {
            tenant: self.info().into(),
            ancestors: Vec::new(),
        })
    }

    async fn get_descendants(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
        _options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        if id.0 != self.tenant {
            return Err(Self::not_found(id));
        }
        Ok(GetDescendantsResponse {
            tenant: self.info().into(),
            descendants: Vec::new(),
        })
    }

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        _ancestor_id: TenantId,
        _descendant_id: TenantId,
        _options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        // The tree has one node: no tenant is an ancestor of another.
        Ok(false)
    }
}

/// A decision point that grants every request unconstrained.
pub struct AllowAllAuthz;

#[async_trait]
impl AuthZResolverApi for AllowAllAuthz {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext::default(),
        })
    }
}

/// A `GearCtx` for the gear over a fresh hub holding the fakes above and a
/// migrated in-memory `SQLite` database.
pub async fn gear_ctx(config: &Value) -> (GearCtx, Arc<ClientHub>) {
    let db: Db = connect_db(
        "sqlite::memory:",
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("db connect");
    run_migrations_for_gear(&db, GEAR, PolicyEngine::default().migrations())
        .await
        .expect("migrate");
    let dbp: DBProvider<DbError> = DBProvider::new(db);

    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TypesRegistryClient>(Arc::new(FakeTypesRegistry::new()));
    hub.register::<dyn TenantResolverClient>(Arc::new(FakeTenantResolver::new(TENANT)));
    hub.register::<dyn AuthZResolverApi>(Arc::new(AllowAllAuthz));

    let ctx = GearCtx::new(
        GEAR,
        Uuid::new_v4(),
        Arc::new(MockConfigProvider::new(config)),
        Arc::clone(&hub),
        CancellationToken::new(),
    )
    .with_db(dbp);
    (ctx, hub)
}
