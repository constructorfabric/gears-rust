// Created: 2026-09-06 by Constructor Tech
//! Fakes and fixtures shared by this crate's unit tests.
//!
//! Every fake here stands in for a port the gear resolves from the platform at
//! init — the types registry, the audit sink, the tenant resolver — so a test
//! can run the real domain code and the real repositories over an in-memory
//! database without a platform around it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sea_orm_migration::MigratorTrait;
use serde_json::Value;
use settings_service_sdk::SettingKey;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use types_registry_sdk::{GtsTypeId, GtsTypeSchema};
use uuid::Uuid;

use crate::audit::{AuditRecord, AuditSink};
use crate::domain::contribution::SettingTypeRegistrar;
use crate::domain::error::DomainError;
use crate::domain::platform_scope::PlatformScope;
use crate::domain::resolution::TenantHierarchy;
use crate::infra::type_validator::SchemaSource;

use std::num::NonZeroU32;
use std::time::Duration;

use serde_json::json;
use toolkit_security::AccessScope;

use crate::domain::category::{CategoryDraft, CategoryKey, CategoryRepository};
use crate::domain::declaration::{DeclarationDraft, DeclarationRepository};
use crate::domain::resolution::{EffectiveCache, ScopeTarget, ValueResolver};
use crate::domain::value::{ValueDraft, ValueRepository};
use crate::infra::storage::access_repo::AccessRepo;
use crate::infra::storage::category_repo::CategoryRepo;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::infra::type_validator::GtsTypeValidator;

/// A registry standing in for the value-type catalogue.
#[derive(Default)]
pub struct FakeSource {
    pub schemas: HashMap<String, GtsTypeSchema>,
    pub instances: HashSet<String>,
    pub unavailable: bool,
}

impl FakeSource {
    pub fn with_type(mut self, id: &str, schema: Value) -> Self {
        let schema = GtsTypeSchema::try_new(GtsTypeId::new(id), schema, None, None)
            .expect("fixture schema is a valid root type");
        self.schemas.insert(id.to_owned(), schema);
        self
    }

    pub fn with_instance(mut self, id: &str) -> Self {
        self.instances.insert(id.to_owned());
        self
    }
}

#[async_trait]
impl SchemaSource for FakeSource {
    async fn type_schema(&self, type_id: &str) -> Result<Option<GtsTypeSchema>, DomainError> {
        if self.unavailable {
            return Err(DomainError::Unavailable {
                detail: "registry down".to_owned(),
            });
        }
        Ok(self.schemas.get(type_id).cloned())
    }

    async fn instance_exists(&self, instance_id: &str) -> Result<bool, DomainError> {
        Ok(self.instances.contains(instance_id))
    }
}

/// An audit sink that keeps what it was given.
#[derive(Default)]
pub struct RecordingAudit {
    pub records: Mutex<Vec<AuditRecord>>,
}

impl RecordingAudit {
    pub fn operations(&self) -> Vec<&'static str> {
        self.records
            .lock()
            .expect("audit lock")
            .iter()
            .map(|r| r.operation.as_str())
            .collect()
    }
}

#[async_trait]
impl AuditSink for Arc<RecordingAudit> {
    async fn append<C: toolkit_db::secure::DBRunner>(
        &self,
        _conn: &C,
        _scope: &AccessScope,
        record: AuditRecord,
    ) -> Result<(), DomainError> {
        self.records.lock().expect("audit lock").push(record);
        Ok(())
    }
}

/// A sink that refuses every record, standing in for a database that cannot
/// take the row: the mutation must roll back with it.
pub struct FailingSink;

#[async_trait]
impl AuditSink for FailingSink {
    async fn append<C: toolkit_db::secure::DBRunner>(
        &self,
        _conn: &C,
        _scope: &AccessScope,
        _record: AuditRecord,
    ) -> Result<(), DomainError> {
        Err(DomainError::Unavailable {
            detail: "audit store down".to_owned(),
        })
    }
}

/// A platform scope with a fixed root tenant.
pub struct FixedScope(pub Uuid);

#[async_trait]
impl PlatformScope for FixedScope {
    async fn root_tenant(&self) -> Result<Uuid, DomainError> {
        Ok(self.0)
    }
}

/// A setting-type registrar that records the keys it was asked to register,
/// and can be told to fail.
#[derive(Default)]
pub struct RecordingRegistrar {
    pub registered: Mutex<Vec<(String, String)>>,
    pub fail: bool,
}

#[async_trait]
impl SettingTypeRegistrar for RecordingRegistrar {
    async fn register_setting_type(
        &self,
        key: &SettingKey,
        value_type_id: &str,
    ) -> Result<(), DomainError> {
        if self.fail {
            return Err(DomainError::Unavailable {
                detail: "types registry down".to_owned(),
            });
        }
        self.registered
            .lock()
            .expect("registrar lock")
            .push((key.to_string(), value_type_id.to_owned()));
        Ok(())
    }
}

/// A fresh in-memory `SQLite` database with this gear's migrations applied.
///
/// One connection, because `SQLite` `:memory:` is per-connection: every
/// repository and every transaction must see the same schema and data.
pub async fn sqlite_provider() -> Arc<DBProvider<DbError>> {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("in-memory sqlite connects");
    run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .map_err(|e| e.to_string())
    .expect("migrations apply on sqlite");
    Arc::new(DBProvider::new(db))
}

/// An in-memory tenant tree standing in for the tenant resolver.
///
/// `parents` maps every tenant to its parent, `None` for the root. A tenant in
/// `standalone` is a barrier: administration from above cannot reach it, while
/// runtime resolution still walks through it to the root.
#[derive(Default)]
pub struct FakeHierarchy {
    pub parents: Mutex<HashMap<Uuid, Option<Uuid>>>,
    pub standalone: Mutex<HashSet<Uuid>>,
    pub chain_calls: std::sync::atomic::AtomicUsize,
    pub unavailable: std::sync::atomic::AtomicBool,
}

impl FakeHierarchy {
    pub fn with_tenant(self, id: Uuid, parent: Option<Uuid>) -> Self {
        self.parents.lock().expect("lock").insert(id, parent);
        self
    }

    pub fn with_standalone(self, id: Uuid) -> Self {
        self.standalone.lock().expect("lock").insert(id);
        self
    }

    pub fn set_unavailable(&self, unavailable: bool) {
        self.unavailable
            .store(unavailable, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn chain_calls(&self) -> usize {
        self.chain_calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn path_to_root(&self, tenant: Uuid) -> Result<Vec<Uuid>, DomainError> {
        let parents = self.parents.lock().expect("lock");
        let mut path = vec![tenant];
        let mut cursor = tenant;
        loop {
            match parents.get(&cursor) {
                Some(Some(parent)) => {
                    path.push(*parent);
                    cursor = *parent;
                }
                Some(None) => return Ok(path),
                None => {
                    return Err(DomainError::NotFound { resource: "tenant" });
                }
            }
        }
    }
}

#[async_trait]
impl TenantHierarchy for FakeHierarchy {
    async fn chain(&self, tenant: Uuid) -> Result<Vec<Uuid>, DomainError> {
        self.chain_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.unavailable.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(DomainError::Unavailable {
                detail: "tenant resolver down".to_owned(),
            });
        }
        let mut path = self.path_to_root(tenant)?;
        path.reverse();
        Ok(path)
    }

    async fn is_within_subtree(&self, caller: Uuid, target: Uuid) -> Result<bool, DomainError> {
        if caller == target {
            return Ok(true);
        }
        let path = self.path_to_root(target)?;
        let standalone = self.standalone.lock().expect("lock");
        // Walk up from the target; a barrier strictly below the caller seals
        // the subtree it roots, the target itself included.
        for tenant in &path {
            if *tenant == caller {
                return Ok(true);
            }
            if standalone.contains(tenant) {
                return Ok(false);
            }
        }
        Ok(false)
    }

    async fn is_standalone(&self, tenant: Uuid) -> Result<bool, DomainError> {
        self.path_to_root(tenant)?;
        Ok(self.standalone.lock().expect("lock").contains(&tenant))
    }

    async fn descendants(&self, tenant: Uuid) -> Result<Vec<Uuid>, DomainError> {
        let ids: Vec<Uuid> = self.parents.lock().expect("lock").keys().copied().collect();
        let mut out = Vec::new();
        for candidate in ids {
            if candidate != tenant && self.is_within_subtree(tenant, candidate).await? {
                out.push(candidate);
            }
        }
        Ok(out)
    }

    async fn descendants_bfs(
        &self,
        tenant: Uuid,
        budget: usize,
    ) -> Result<(Vec<Uuid>, bool), DomainError> {
        let reachable = self.descendants(tenant).await?;
        let parents = self.parents.lock().expect("lock").clone();
        let mut order = Vec::new();
        let mut queue = std::collections::VecDeque::from([tenant]);
        let mut truncated = false;
        while let Some(next) = queue.pop_front() {
            let mut kids: Vec<Uuid> = reachable
                .iter()
                .copied()
                .filter(|c| parents.get(c) == Some(&Some(next)))
                .collect();
            kids.sort();
            for kid in kids {
                if order.len() >= budget {
                    truncated = true;
                    break;
                }
                order.push(kid);
                queue.push_back(kid);
            }
            if truncated {
                break;
            }
        }
        Ok((order, truncated))
    }
}

// ---- The resolution harness: tree `root → a → b`, `c` a sibling of `a`, `s` a
// standalone child of `a`; a category, declarations and rows written through
// the real repositories over an in-memory database.

pub const BOOL: &str = "gts.cf.toolkit.settings.type_bool_flag.v1~";
pub const SECRET: &str = "gts.cf.toolkit.settings.type_secret_string.v1~";

pub fn resolution_catalogue() -> FakeSource {
    FakeSource::default()
        .with_type(
            BOOL,
            json!({ "$id": format!("gts://{BOOL}"), "type": "boolean" }),
        )
        .with_type(
            SECRET,
            json!({
                "$id": format!("gts://{SECRET}"),
                "type": "string",
                "x-gts-traits": { "secret": true }
            }),
        )
}

pub struct Tree {
    pub root: Uuid,
    pub a: Uuid,
    pub b: Uuid,
    pub c: Uuid,
    pub s: Uuid,
}

impl Tree {
    pub fn new() -> Self {
        Self {
            root: Uuid::new_v4(),
            a: Uuid::new_v4(),
            b: Uuid::new_v4(),
            c: Uuid::new_v4(),
            s: Uuid::new_v4(),
        }
    }

    pub fn hierarchy(&self) -> FakeHierarchy {
        FakeHierarchy::default()
            .with_tenant(self.root, None)
            .with_tenant(self.a, Some(self.root))
            .with_tenant(self.b, Some(self.a))
            .with_tenant(self.c, Some(self.root))
            .with_tenant(self.s, Some(self.a))
            .with_standalone(self.s)
    }
}

pub struct ResolutionHarness {
    pub db: Arc<DBProvider<DbError>>,
    pub tree: Tree,
    pub hierarchy: Arc<FakeHierarchy>,
    pub cache: Arc<EffectiveCache>,
    pub resolver: Arc<ValueResolver<DeclarationRepo, ValueRepo, AccessRepo>>,
    category_id: Uuid,
}

impl ResolutionHarness {
    pub async fn new() -> Self {
        Self::with_ttl(Duration::from_secs(30)).await
    }

    pub async fn with_ttl(ttl: Duration) -> Self {
        let db = sqlite_provider().await;
        let tree = Tree::new();
        let hierarchy = Arc::new(tree.hierarchy());
        let cache = Arc::new(EffectiveCache::new(ttl));
        let resolver = Arc::new(ValueResolver::new(
            DeclarationRepo,
            ValueRepo,
            AccessRepo,
            Arc::clone(&hierarchy) as Arc<dyn crate::domain::resolution::TenantHierarchy>,
            Arc::new(FixedScope(tree.root)),
            Arc::new(GtsTypeValidator::new(resolution_catalogue())),
            Arc::clone(&cache),
        ));
        let conn = db.conn().expect("connection");
        let category = CategoryRepo
            .insert(
                &conn,
                &AccessScope::allow_all(),
                CategoryDraft {
                    key: CategoryKey::parse("network").expect("slug"),
                    name: "network".to_owned(),
                    description: None,
                    domain_affinity: None,
                    sort_order: 0,
                    icon: None,
                },
            )
            .await
            .expect("category");
        Self {
            db,
            tree,
            hierarchy,
            cache,
            resolver,
            category_id: category.id,
        }
    }

    #[allow(clippy::unused_self)]
    /// The category every harness declaration files under.
    pub fn category_id(&self) -> Uuid {
        self.category_id
    }

    #[allow(clippy::unused_self)]
    pub fn key(&self, name: &str) -> SettingKey {
        SettingKey::contributed("cf", "demo", "network", name, NonZeroU32::MIN).expect("key")
    }

    /// Declare a setting, returning its id.
    pub async fn declare(&self, name: &str, scope_class: &str, default: Value) -> Uuid {
        self.declare_typed(name, scope_class, default, BOOL, "public")
            .await
    }

    pub async fn declare_typed(
        &self,
        name: &str,
        scope_class: &str,
        default: Value,
        value_type_id: &str,
        classification: &str,
    ) -> Uuid {
        let conn = self.db.conn().expect("connection");
        let key = self.key(name);
        DeclarationRepo
            .insert(
                &conn,
                &AccessScope::allow_all(),
                DeclarationDraft {
                    key: key.to_string(),
                    leaf_slug: name.to_owned(),
                    value_type_id: value_type_id.to_owned(),
                    category_id: self.category_id,
                    default_value: default,
                    scope_class: scope_class.to_owned(),
                    mode: "standard".to_owned(),
                    requires_step_up: true,
                    anonymous_exposable: false,
                    domain_affinity: None,
                    has_secret_trait: classification == "secret",
                    data_classification: classification.to_owned(),
                    source: "module_contributed".to_owned(),
                    owner_module: Some("test".to_owned()),
                    licence_feature: None,
                    description: None,
                    created_by: "test".to_owned(),
                },
            )
            .await
            .expect("declaration")
            .id
    }

    pub async fn retire(&self, declaration_id: Uuid) {
        let conn = self.db.conn().expect("connection");
        DeclarationRepo
            .set_status(&conn, &AccessScope::allow_all(), declaration_id, "retired")
            .await
            .expect("retire");
    }

    pub async fn set(&self, declaration_id: Uuid, tenant: Uuid, value: Value) {
        self.write(declaration_id, tenant, Some(value), None, false)
            .await;
    }

    pub async fn set_flagged(&self, declaration_id: Uuid, tenant: Uuid, value: Value) {
        self.write(declaration_id, tenant, Some(value), None, true)
            .await;
    }

    pub async fn set_secret(&self, declaration_id: Uuid, tenant: Uuid, secret_ref: &str) {
        self.write(
            declaration_id,
            tenant,
            None,
            Some(secret_ref.to_owned()),
            false,
        )
        .await;
    }

    async fn write(
        &self,
        declaration_id: Uuid,
        tenant: Uuid,
        value: Option<Value>,
        secret_ref: Option<String>,
        flagged: bool,
    ) {
        let conn = self.db.conn().expect("connection");
        let classification = if secret_ref.is_some() {
            "secret"
        } else {
            "public"
        };
        ValueRepo
            .insert(
                &conn,
                &AccessScope::allow_all(),
                ValueDraft {
                    declaration_id,
                    tenant_id: tenant,
                    value,
                    secret_ref,
                    data_classification: classification.to_owned(),
                    needs_review: flagged,
                    needs_review_detail: flagged.then(|| "no longer validates".to_owned()),
                    set_by: format!("admin-of-{tenant}"),
                },
            )
            .await
            .expect("value row");
    }

    pub async fn resolve(
        &self,
        name: &str,
        target: ScopeTarget,
    ) -> Result<Arc<crate::domain::resolution::EffectiveValue>, DomainError> {
        let conn = self.db.conn().expect("connection");
        self.resolver.resolve(&conn, &self.key(name), target).await
    }
}

/// A Change Publisher that keeps what it was given.
#[derive(Default)]
pub struct RecordingPublisher {
    pub events: Mutex<Vec<crate::domain::ports::ValueEvent>>,
}

#[async_trait]
impl crate::domain::ports::ChangePublisher for RecordingPublisher {
    async fn publish(&self, event: crate::domain::ports::ValueEvent) {
        self.events.lock().expect("lock").push(event);
    }
}

/// A step-up verifier whose verdict the test fixes.
pub struct FixedStepUp {
    pub verdict: Result<(), crate::domain::stepup::StepUpRefusal>,
    pub requirement: crate::domain::stepup::StepUpRequirement,
}

impl FixedStepUp {
    pub fn verified() -> Self {
        Self {
            verdict: Ok(()),
            requirement: crate::domain::stepup::StepUpRequirement {
                max_age: Duration::from_mins(5),
                acr_values: Vec::new(),
                amr_values: Vec::new(),
            },
        }
    }

    pub fn refusing(refusal: crate::domain::stepup::StepUpRefusal) -> Self {
        Self {
            verdict: Err(refusal),
            ..Self::verified()
        }
    }
}

#[async_trait]
impl crate::domain::stepup::StepUpVerifier for FixedStepUp {
    async fn verify(
        &self,
        _token: Option<&str>,
        _subject: &crate::domain::stepup::StepUpSubject,
    ) -> Result<(), crate::domain::stepup::StepUpRefusal> {
        self.verdict.clone()
    }

    fn requirement(&self) -> &crate::domain::stepup::StepUpRequirement {
        &self.requirement
    }
}
