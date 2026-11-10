#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use gts::{GtsSchema, GtsTypeId};
use license_enforcement_sdk::models::{
    GrantTerms, PermissionsInput, ResourceRefInput, SubjectRef, SubjectRefInput, WindowInput,
};
use license_enforcement_sdk::{GrantAdmissionPluginV1, linked_admission_plugin, linked_evaluator};
use license_resolver_sdk::{
    LicenseCheckContext, LicenseCheckRequest, LicenseResourceV1, LicenseSubjectV1,
    LicenseTenantSubjectV1, Resource, Subject,
};
use sea_orm_migration::MigratorTrait;
use serde_json::json;
use time::{Duration, OffsetDateTime};
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::odata::LimitCfg;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use types_registry_sdk::testing::MockTypesRegistryClient;
use types_registry_sdk::{GtsTypeSchema, TypesRegistryClient};
use uuid::Uuid;

use crate::domain::check::CheckService;
use crate::domain::error::DomainError;
use crate::domain::grants::GrantService;
use crate::infra::storage::Store;

// The tests link the default implementations, as a binary does.
use le_default_evaluator as _;
use le_issue_admission_plugin as _;

pub const RESOURCE: &str = "gts.cf.core.lic.res.v1~test.le.app.seat.v1~";
pub const USER: &str = "gts.cf.core.lic.subj.v1~test.le.app.user.v1~";
pub const ADMIN: &str = "gts.cf.core.lic.subj.v1~test.le.app.user.v1~test.le.app.admin.v1~";
/// Registered, but not admitted by the resource.
pub const SERVICE: &str = "gts.cf.core.lic.subj.v1~test.le.app.service.v1~";
/// Derives from `USER` by its id, but is not registered.
pub const GHOST: &str = "gts.cf.core.lic.subj.v1~test.le.app.user.v1~test.le.app.ghost.v1~";

pub fn tenant_subject() -> &'static str {
    <LicenseTenantSubjectV1 as GtsSchema>::TYPE_ID
}

fn schema(type_id: &str, body: serde_json::Value, parent: Option<&GtsTypeSchema>) -> GtsTypeSchema {
    GtsTypeSchema::try_new(
        GtsTypeId::new(type_id),
        body,
        None,
        parent.map(|p| Arc::new(p.clone())),
    )
    .unwrap()
}

fn registry() -> MockTypesRegistryClient {
    let subject_base = schema(
        <LicenseSubjectV1<()> as GtsSchema>::TYPE_ID,
        <LicenseSubjectV1<()> as GtsSchema>::gts_schema_with_refs(),
        None,
    );
    let resource_base = schema(
        <LicenseResourceV1<()> as GtsSchema>::TYPE_ID,
        <LicenseResourceV1<()> as GtsSchema>::gts_schema_with_refs(),
        None,
    );
    let user = schema(USER, json!({}), Some(&subject_base));
    let admin = schema(ADMIN, json!({}), Some(&user));
    let service = schema(SERVICE, json!({}), Some(&subject_base));
    let tenant = schema(tenant_subject(), json!({}), Some(&subject_base));
    let resource = schema(
        RESOURCE,
        json!({ "x-gts-traits": { "admitted_subjects": [USER, tenant_subject()] } }),
        Some(&resource_base),
    );
    MockTypesRegistryClient::new().with_type_schemas([
        subject_base,
        resource_base,
        user,
        admin,
        service,
        tenant,
        resource,
    ])
}

/// Allows everything; scopes reads to the caller's tenant.
pub struct AllowAuthZ;

#[async_trait]
impl AuthZResolverApi for AllowAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let tenant = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());
        let constraints = match (request.context.require_constraints, tenant) {
            (true, Some(t)) => vec![Constraint {
                predicates: vec![Predicate::In(InPredicate::new(
                    pep_properties::OWNER_TENANT_ID,
                    [t],
                ))],
            }],
            _ => vec![],
        };
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints,
                ..Default::default()
            },
        })
    }
}

/// Denies everything.
pub struct DenyAuthZ;

#[async_trait]
impl AuthZResolverApi for DenyAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext::default(),
        })
    }
}

pub struct Env {
    pub grants: Arc<GrantService<Store>>,
    pub check: Arc<CheckService<Store>>,
    pub db: Arc<DBProvider<DomainError>>,
    pub store: Store,
    pub hub: Arc<ClientHub>,
    #[cfg(feature = "integration")]
    _pg: testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
}

pub async fn env() -> Env {
    env_with(Arc::new(AllowAuthZ)).await
}

pub async fn env_with(authz: Arc<dyn AuthZResolverApi>) -> Env {
    env_with_plugin(authz, linked_admission_plugin().unwrap()).await
}

pub async fn env_with_plugin(
    authz: Arc<dyn AuthZResolverApi>,
    plugin: Arc<dyn GrantAdmissionPluginV1>,
) -> Env {
    env_full(authz, plugin, Duration::hours(1)).await
}

/// Idempotency records live for `window`.
pub async fn env_with_window(window: Duration) -> Env {
    env_full(
        Arc::new(AllowAuthZ),
        linked_admission_plugin().unwrap(),
        window,
    )
    .await
}

/// In-memory `SQLite`.
#[cfg(not(feature = "integration"))]
async fn connect() -> toolkit_db::Db {
    connect_db(
        "sqlite::memory:",
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

/// A `PostgreSQL` container per test; Docker is required.
#[cfg(feature = "integration")]
async fn connect() -> (
    toolkit_db::Db,
    testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
) {
    use testcontainers::ImageExt;
    use testcontainers::runners::AsyncRunner;

    let container = test_containers::postgres()
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "le")
        .start()
        .await
        .expect("start a PostgreSQL container; Docker is required");
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let dsn = format!("postgres://user:pass@127.0.0.1:{port}/le");
    let mut last = None;
    for _ in 0..50 {
        match connect_db(&dsn, ConnectOpts::default()).await {
            Ok(db) => return (db, container),
            Err(e) => last = Some(e),
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    panic!("PostgreSQL is not reachable: {last:?}");
}

async fn env_full(
    authz: Arc<dyn AuthZResolverApi>,
    plugin: Arc<dyn GrantAdmissionPluginV1>,
    window: Duration,
) -> Env {
    #[cfg(not(feature = "integration"))]
    let db = connect().await;
    #[cfg(feature = "integration")]
    let (db, pg) = connect().await;
    run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    let db = Arc::new(DBProvider::<DomainError>::new(db));
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TypesRegistryClient>(Arc::new(registry()));
    let evaluator = linked_evaluator().unwrap();
    let store = Store::new(
        LimitCfg {
            default: 50,
            max: 1000,
        },
        100,
    );
    let check = Arc::new(CheckService::new(
        Arc::clone(&db),
        store,
        Arc::clone(&evaluator),
        Arc::clone(&plugin),
    ));
    let grants = Arc::new(GrantService::new(
        Arc::clone(&db),
        store,
        PolicyEnforcer::new(authz),
        Arc::clone(&hub),
        evaluator,
        plugin,
        check.material(),
        window,
    ));
    Env {
        grants,
        check,
        db,
        store,
        hub,
        #[cfg(feature = "integration")]
        _pg: pg,
    }
}

pub fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .build()
        .unwrap()
}

pub fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

pub fn terms() -> GrantTerms {
    GrantTerms {
        resource: ResourceRefInput {
            gts_type: GtsTypeId::new(RESOURCE),
            id: None,
        },
        subject_type: GtsTypeId::new(USER),
        fixed_subject: None,
        window: WindowInput {
            not_before: now() - Duration::days(1),
            expires_at: now() + Duration::days(30),
            grace_until: None,
        },
        conditions: None,
        permissions: PermissionsInput {
            may_bind: true,
            max_depth: 0,
            requires_acceptance: false,
        },
        recipient_rules: None,
    }
}

pub fn user(id: &str) -> SubjectRef {
    SubjectRef {
        gts_type: GtsTypeId::new(USER),
        id: id.to_owned(),
    }
}

/// `subject` as a field of a request.
pub fn input(subject: SubjectRef) -> SubjectRefInput {
    SubjectRefInput {
        gts_type: subject.gts_type,
        id: subject.id,
    }
}

pub fn request(subject_type: &str, subject_id: &str, tenant: Uuid) -> LicenseCheckRequest {
    LicenseCheckRequest::new(
        Subject {
            gts_type: GtsTypeId::new(subject_type),
            id: Some(subject_id.to_owned()),
            metadata: serde_json::Map::new(),
        },
        Resource {
            gts_type: GtsTypeId::new(RESOURCE),
            id: None,
            metadata: serde_json::Map::new(),
        },
        LicenseCheckContext::builder()
            .tenant_id(tenant)
            .build()
            .unwrap(),
    )
}

pub fn token() -> String {
    Uuid::now_v7().to_string()
}
