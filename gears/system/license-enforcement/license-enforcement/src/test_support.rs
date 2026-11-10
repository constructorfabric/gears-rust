#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

#[cfg(feature = "integration")]
mod pg_server;

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
use tenant_resolver_sdk::{
    BarrierMode, GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions,
    GetDescendantsResponse, GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo,
    TenantResolverClient, TenantResolverError,
};
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
/// A resource whose `admitted_subjects` is not a list: an unusable contract.
pub const BROKEN: &str = "gts.cf.core.lic.res.v1~test.le.app.broken.v1~";
/// An abstract resource contract.
pub const ABSTRACT: &str = "gts.cf.core.lic.res.v1~test.le.app.abstract.v1~";
/// Derives from `USER` and needs a `plan` in its metadata.
pub const STRICT: &str = "gts.cf.core.lic.subj.v1~test.le.app.user.v1~test.le.app.strict.v1~";
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
    let strict = schema(
        STRICT,
        json!({ "properties": { "metadata": { "type": "object", "required": ["plan"] } } }),
        Some(&user),
    );
    let abstract_resource = schema(
        ABSTRACT,
        json!({ "x-gts-abstract": true }),
        Some(&resource_base),
    );
    let tenant = schema(tenant_subject(), json!({}), Some(&subject_base));
    let resource = schema(
        RESOURCE,
        json!({ "x-gts-traits": { "admitted_subjects": [USER, tenant_subject()] } }),
        Some(&resource_base),
    );
    let broken = schema(
        BROKEN,
        json!({ "x-gts-traits": { "admitted_subjects": USER } }),
        Some(&resource_base),
    );
    MockTypesRegistryClient::new().with_type_schemas([
        subject_base,
        resource_base,
        user,
        admin,
        service,
        strict,
        tenant,
        resource,
        broken,
        abstract_resource,
    ])
}

/// Allows everything. Scopes reads as the platform PDP does for a gear
/// without hierarchy capabilities: the caller's tenant and its descendants up
/// to the barriers, or the caller's tenant alone in `RootOnly` mode.
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
        let root_only = request
            .context
            .tenant_context
            .as_ref()
            .is_some_and(|c| c.mode == authz_resolver_sdk::models::TenantMode::RootOnly);
        let constraints = match (request.context.require_constraints, tenant) {
            (true, Some(t)) => vec![Constraint {
                predicates: vec![Predicate::In(InPredicate::new(
                    pep_properties::OWNER_TENANT_ID,
                    if root_only {
                        vec![t]
                    } else {
                        TENANTS.subtree(t)
                    },
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

/// [`AllowAuthZ`] but for the `(resource type, action)` pairs of its rules:
/// `None` denies, `Some(ids)` admits these resource ids alone.
#[derive(Default)]
pub struct RuleAuthZ(std::sync::Mutex<Vec<Rule>>);

type Rule = (&'static str, &'static str, Option<Vec<Uuid>>);

impl RuleAuthZ {
    pub fn set(&self, resource: &'static str, action: &'static str, ids: Option<Vec<Uuid>>) {
        self.0.lock().unwrap().push((resource, action, ids));
    }
}

#[async_trait]
impl AuthZResolverApi for RuleAuthZ {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let rule = self
            .0
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(r, a, _)| *r == request.resource.resource_type && *a == request.action.name)
            .map(|(_, _, ids)| ids.clone());
        match rule {
            None => AllowAuthZ.evaluate(ctx, request).await,
            Some(None) => DenyAuthZ.evaluate(ctx, request).await,
            Some(Some(ids)) => Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints: vec![Constraint {
                        predicates: vec![Predicate::In(InPredicate::new(
                            pep_properties::RESOURCE_ID,
                            ids,
                        ))],
                    }],
                    ..Default::default()
                },
            }),
        }
    }
}

/// A tenant tree for `is_ancestor` and the test PDP: child to parent, and
/// the self-managed tenants (behind a barrier). Other methods are not called
/// by the gear.
#[derive(Default)]
pub struct Tenants(
    pub std::sync::Mutex<std::collections::HashMap<Uuid, Uuid>>,
    pub std::sync::Mutex<std::collections::HashSet<Uuid>>,
);

impl Tenants {
    /// `child` is a child of `parent`.
    pub fn link(&self, parent: Uuid, child: Uuid) {
        self.0.lock().unwrap().insert(child, parent);
    }

    /// `tenant` is self-managed: a subtree scope of its ancestors stops there.
    pub fn self_managed(&self, tenant: Uuid) {
        self.1.lock().unwrap().insert(tenant);
    }

    /// `root` and its descendants up to the barriers.
    fn subtree(&self, root: Uuid) -> Vec<Uuid> {
        let tree = self.0.lock().unwrap();
        let barriers = self.1.lock().unwrap();
        let mut out = vec![root];
        for &t in tree.keys() {
            let mut at = t;
            while !barriers.contains(&at) {
                match tree.get(&at) {
                    Some(&p) if p == root => {
                        out.push(t);
                        break;
                    }
                    Some(&p) => at = p,
                    None => break,
                }
            }
        }
        out
    }
}

/// One tenant tree for every test: tests use fresh tenant ids, so they do
/// not meet.
static TENANTS: std::sync::LazyLock<Arc<Tenants>> =
    std::sync::LazyLock::new(|| Arc::new(Tenants::default()));

#[async_trait]
impl TenantResolverClient for Tenants {
    async fn get_tenant(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        unreachable!("not called by the gear")
    }

    async fn get_root_tenant(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        unreachable!("not called by the gear")
    }

    async fn get_tenants(
        &self,
        _ctx: &SecurityContext,
        _ids: &[TenantId],
        _options: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        unreachable!("not called by the gear")
    }

    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        _options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        unreachable!("not called by the gear")
    }

    async fn get_descendants(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        _options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        unreachable!("not called by the gear")
    }

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        ancestor_id: TenantId,
        descendant_id: TenantId,
        options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        let tree = self.0.lock().unwrap();
        let barriers = self.1.lock().unwrap();
        let mut at = descendant_id.0;
        while let Some(parent) = tree.get(&at) {
            // A self-managed tenant on the way hides the descendant.
            if options.barrier_mode == BarrierMode::Respect && barriers.contains(&at) {
                return Ok(false);
            }
            if *parent == ancestor_id.0 {
                return Ok(true);
            }
            at = *parent;
        }
        Ok(false)
    }
}

pub struct Env {
    pub grants: Arc<GrantService<Store>>,
    pub check: Arc<CheckService<Store>>,
    pub db: Arc<DBProvider<DomainError>>,
    pub store: Store,
    pub hub: Arc<ClientHub>,
    pub tenants: Arc<Tenants>,
    /// The test database, for a connection of the test's own.
    #[cfg(feature = "integration")]
    dsn: String,
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

/// A fresh database on the shared `PostgreSQL` server; Docker is required.
#[cfg(feature = "integration")]
async fn connect() -> (toolkit_db::Db, String) {
    let dsn = pg_server::fresh_database().await;
    let db = connect_db(
        &dsn,
        ConnectOpts {
            // One server carries the pools of every test process at once.
            max_conns: Some(4),
            min_conns: Some(0),
            ..Default::default()
        },
    )
    .await
    .expect("connect to the test database");
    (db, dsn)
}

async fn env_full(
    authz: Arc<dyn AuthZResolverApi>,
    plugin: Arc<dyn GrantAdmissionPluginV1>,
    window: Duration,
) -> Env {
    #[cfg(not(feature = "integration"))]
    let db = connect().await;
    #[cfg(feature = "integration")]
    let (db, dsn) = connect().await;
    run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    let db = Arc::new(DBProvider::<DomainError>::new(db));
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TypesRegistryClient>(Arc::new(registry()));
    let tenants = Arc::clone(&TENANTS);
    hub.register::<dyn TenantResolverClient>(Arc::clone(&tenants) as Arc<dyn TenantResolverClient>);
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
        tenants,
        #[cfg(feature = "integration")]
        dsn,
    }
}

/// The caller of `tenant`: one subject per tenant, so a token replays.
pub fn ctx(tenant: Uuid) -> SecurityContext {
    ctx_as(tenant, tenant)
}

/// The caller `subject` in `tenant`.
pub fn ctx_as(tenant: Uuid, subject: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .build()
        .unwrap()
}

/// Inserts one sub-grant under `root`, as `delegate` inserts its sub-grants.
pub async fn insert_sub_grant(
    env: &Env,
    scope: &toolkit_security::AccessScope,
    g: &license_enforcement_sdk::models::Grant,
    root: license_enforcement_sdk::GrantId,
) -> Result<(), DomainError> {
    use crate::domain::repo::GrantRepo as _;
    let conn = env.db.conn().expect("a connection");
    env.store
        .insert_sub_grants(&conn, scope, &[(g.clone(), root)], now())
        .await
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

/// Issue one grant to `tenant`, as `tenant`.
pub async fn issue_one(
    env: &Env,
    tenant: Uuid,
    t: GrantTerms,
) -> license_enforcement_sdk::models::Grant {
    env.grants
        .issue(&ctx(tenant), tenant, vec![t], false, token())
        .await
        .unwrap()
        .grants
        .remove(0)
}

/// Terms that can be delegated `depth` levels down.
pub fn delegable(depth: u16) -> GrantTerms {
    let mut t = terms();
    t.permissions.max_depth = depth;
    t
}

pub fn token() -> String {
    Uuid::now_v7().to_string()
}

/// One sweep at a time across tests: the in-memory `SQLite` databases of all
/// tests share one advisory lock key.
pub async fn sweep(env: &Env, retention: Duration) -> (u64, u64) {
    static ONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one = ONE.lock().await;
    env.grants.sweep(retention).await.unwrap().unwrap()
}

/// Run `sql` on the test database, as an operator with access to it does:
/// `postgres` on `PostgreSQL`, `sqlite` on `SQLite`.
pub async fn operator_sql(env: &Env, postgres: &'static str, sqlite: &'static str) {
    use sea_orm_migration::prelude::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend};
    struct Sql(String, &'static str, &'static str);
    impl MigrationName for Sql {
        fn name(&self) -> &str {
            &self.0
        }
    }
    #[async_trait]
    impl MigrationTrait for Sql {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            let sql = match manager.get_database_backend() {
                DatabaseBackend::Postgres => self.1,
                _ => self.2,
            };
            manager.get_connection().execute_unprepared(sql).await?;
            Ok(())
        }
    }
    run_migrations_for_testing(
        &env.db.db(),
        vec![Box::new(Sql(
            format!("m_operator_{}", Uuid::now_v7().simple()),
            postgres,
            sqlite,
        ))],
    )
    .await
    .unwrap();
}

/// Set one column of a stored grant row, as an operator with access to the
/// database does.
pub async fn edit_row(
    env: &Env,
    id: license_enforcement_sdk::GrantId,
    column: crate::infra::storage::entity::grant::Column,
    value: impl Into<sea_orm::Value>,
) {
    use crate::infra::storage::entity::grant;
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;
    let conn = env.db.conn().unwrap();
    let done = grant::Entity::update_many()
        .col_expr(column, Expr::value(value.into()))
        .filter(grant::Column::Id.eq(id.0))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .unwrap();
    assert_eq!(done.rows_affected, 1);
}

/// The sides of a delegation, as one party reads it.
pub trait Sides {
    /// The grants of the grantor; panics on the recipient's view.
    fn given(&self) -> &[license_enforcement_sdk::models::Grant];
    /// The sub-grants of the recipient; panics on the grantor's view.
    fn received(&self) -> &[license_enforcement_sdk::models::Grant];
    /// The sub-grant of the `i`-th grant of the grantor.
    fn sub(&self, i: usize) -> license_enforcement_sdk::GrantId;
}

impl Sides for license_enforcement_sdk::dto::Delegation {
    fn given(&self) -> &[license_enforcement_sdk::models::Grant] {
        match &self.grants {
            license_enforcement_sdk::dto::DelegationGrants::Given { grants } => grants,
            other => panic!("not the grantor's view: {other:?}"),
        }
    }

    fn received(&self) -> &[license_enforcement_sdk::models::Grant] {
        match &self.grants {
            license_enforcement_sdk::dto::DelegationGrants::Received { grants } => grants,
            other => panic!("not the recipient's view: {other:?}"),
        }
    }

    fn sub(&self, i: usize) -> license_enforcement_sdk::GrantId {
        let g = &self.given()[i];
        match &g.binding {
            license_enforcement_sdk::models::Binding::Delegated { grant_id, .. } => *grant_id,
            other => panic!("grant {} is not delegated: {other:?}", g.id),
        }
    }
}

/// The sub-grants of a delegation, as its recipient reads them.
pub async fn sub_grants(
    env: &Env,
    d: &license_enforcement_sdk::dto::Delegation,
) -> Vec<license_enforcement_sdk::models::Grant> {
    env.grants
        .get_delegation(&ctx(d.recipient_tenant_id), d.id)
        .await
        .unwrap()
        .received()
        .to_vec()
}

/// Wait until some `PostgreSQL` backend of the test database waits on a lock:
/// the competing transaction is then provably blocked.
#[cfg(feature = "integration")]
pub async fn until_a_lock_waits(env: &Env) {
    use sea_orm::{ConnectionTrait, Statement};
    let conn = sea_orm::Database::connect(env.dsn.as_str()).await.unwrap();
    let started = std::time::Instant::now();
    loop {
        let waiting = conn
            .query_all_raw(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT pid FROM pg_stat_activity \
                 WHERE wait_event_type = 'Lock' AND datname = current_database()",
            ))
            .await
            .unwrap()
            .len();
        if waiting > 0 {
            return;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "no backend waited on a lock"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// `err` is an invalid transition.
pub fn transition(err: &crate::domain::error::DomainError) {
    assert!(
        matches!(err, crate::domain::error::DomainError::InvalidTransition(_)),
        "{err:?}"
    );
}

/// Delegate `ids` to `recipient` as they are.
pub fn by_ids(
    recipient: Uuid,
    ids: Vec<license_enforcement_sdk::GrantId>,
) -> license_enforcement_sdk::dto::DelegateRequest {
    license_enforcement_sdk::dto::DelegateRequest::new(recipient, ids, token())
}

type AdmitHook = Box<
    dyn Fn(&license_enforcement_sdk::AdmissionInput, &mut license_enforcement_sdk::Admission)
        + Send
        + Sync,
>;
type VerifyHook = Box<
    dyn Fn(
            license_enforcement_sdk::GrantId,
            &license_enforcement_sdk::StoredEvidence,
        ) -> Option<license_enforcement_sdk::Verification>
        + Send
        + Sync,
>;

/// The `issue` plugin with hooks: a test changes only what it needs.
#[derive(Default)]
pub struct TestPlugin {
    /// Id and `verifies_evidence`; those of the `issue` plugin when `None`.
    pub info: Option<(&'static str, bool)>,
    /// `true` withdraws the `issue` intake.
    pub no_issue: std::sync::atomic::AtomicBool,
    /// Changes what the `issue` plugin admitted.
    pub admit: Option<AdmitHook>,
    /// The result of `verify`; that of the `issue` plugin when it gives `None`.
    pub verify: Option<VerifyHook>,
}

impl TestPlugin {
    pub fn admitting(
        f: impl Fn(&license_enforcement_sdk::AdmissionInput, &mut license_enforcement_sdk::Admission)
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            admit: Some(Box::new(f)),
            ..Self::default()
        }
    }

    pub fn verifying(
        verifies: bool,
        f: impl Fn(
            license_enforcement_sdk::GrantId,
            &license_enforcement_sdk::StoredEvidence,
        ) -> Option<license_enforcement_sdk::Verification>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        let id = le_issue_admission_plugin::IssueAdmissionPlugin.info().id;
        Self {
            info: Some((id, verifies)),
            verify: Some(Box::new(f)),
            ..Self::default()
        }
    }
}

#[async_trait]
impl GrantAdmissionPluginV1 for TestPlugin {
    fn info(&self) -> license_enforcement_sdk::PluginInfo {
        match self.info {
            Some((id, verifies)) => license_enforcement_sdk::PluginInfo::new(id, verifies, None),
            None => le_issue_admission_plugin::IssueAdmissionPlugin.info(),
        }
    }

    fn intakes(&self) -> license_enforcement_sdk::Intakes {
        let issue = !self.no_issue.load(std::sync::atomic::Ordering::SeqCst);
        license_enforcement_sdk::Intakes::new(issue, false)
    }

    async fn admit(
        &self,
        input: license_enforcement_sdk::AdmissionInput,
        instant: OffsetDateTime,
        material: &dyn license_enforcement_sdk::PluginMaterial,
    ) -> Result<license_enforcement_sdk::Admission, license_enforcement_sdk::AdmissionError> {
        let hook_input = self.admit.as_ref().map(|_| input.clone());
        let mut admission = le_issue_admission_plugin::IssueAdmissionPlugin
            .admit(input, instant, material)
            .await?;
        if let (Some(hook), Some(input)) = (&self.admit, hook_input) {
            hook(&input, &mut admission);
        }
        Ok(admission)
    }

    async fn verify(
        &self,
        root_id: license_enforcement_sdk::GrantId,
        evidence: &license_enforcement_sdk::StoredEvidence,
        instant: OffsetDateTime,
        material: &dyn license_enforcement_sdk::PluginMaterial,
    ) -> Result<license_enforcement_sdk::Verification, license_enforcement_sdk::PluginFault> {
        if let Some(v) = self.verify.as_ref().and_then(|f| f(root_id, evidence)) {
            return Ok(v);
        }
        le_issue_admission_plugin::IssueAdmissionPlugin
            .verify(root_id, evidence, instant, material)
            .await
    }

    async fn health(&self) -> Result<(), license_enforcement_sdk::PluginFault> {
        Ok(())
    }
}
