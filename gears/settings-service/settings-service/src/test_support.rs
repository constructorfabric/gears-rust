// Created: 2026-09-06 by Virtuozzo International GmbH
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

use secrecy::SecretString;
use serde_json::json;
use toolkit_security::{AccessScope, SecurityContext};

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
    /// How many times a type schema was asked for.
    pub schema_lookups: std::sync::atomic::AtomicUsize,
    /// How many times instances were looked up, whatever the batch size.
    pub instance_lookups: std::sync::atomic::AtomicUsize,
}

impl FakeSource {
    /// A dynamic enumeration this source knows: the source as a registered
    /// type, and its members as instances derived from it.
    pub fn with_enum(self, source: &str, members: &[&str]) -> Self {
        let mut with_source = self.with_type(
            source,
            json!({ "$id": format!("gts://{source}"), "type": "string" }),
        );
        for member in members {
            with_source.instances.insert(format!("{source}{member}"));
        }
        with_source
    }

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
        self.schema_lookups
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.unavailable {
            return Err(DomainError::Unavailable {
                detail: "registry down".to_owned(),
            });
        }
        Ok(self.schemas.get(type_id).cloned())
    }

    async fn resolve_instances(
        &self,
        instance_ids: &[&str],
    ) -> Result<HashMap<String, String>, DomainError> {
        self.instance_lookups
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // As the registry answers: an instance with the type it is registered
        // under — the id up to and including its last `~`.
        Ok(instance_ids
            .iter()
            .filter(|id| self.instances.contains(**id))
            .map(|id| {
                let type_end = id.rfind('~').map_or(id.len(), |i| i + 1);
                ((*id).to_owned(), id[..type_end].to_owned())
            })
            .collect())
    }
}

/// An audit sink that keeps what it was given — and, when told to, refuses
/// the record about one key, standing in for a store that fails at exactly
/// that point of a mutation.
#[derive(Default)]
pub struct RecordingAudit {
    pub records: Mutex<Vec<AuditRecord>>,
    /// The declaration key whose record is refused; every other is kept.
    pub fail_on_key: Mutex<Option<String>>,
}

impl RecordingAudit {
    /// A copy of every record the sink was handed, for assertions about the
    /// record itself rather than the shape of the sequence.
    pub fn records(&self) -> Vec<AuditRecord> {
        self.records.lock().expect("audit lock").clone()
    }

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
        let refused = self
            .fail_on_key
            .lock()
            .expect("audit lock")
            .as_deref()
            .is_some_and(|key| key == record.declaration_key);
        if refused {
            return Err(DomainError::Unavailable {
                detail: format!("audit store down for `{}`", record.declaration_key),
            });
        }
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
    /// How many of the next `chain` calls fail as unavailable before the
    /// resolver answers again: a transient outage, not a standing one.
    pub failing_chains: std::sync::atomic::AtomicUsize,
    /// Runs once, from inside the next `chain` lookup: what a test makes
    /// happen while a resolve is mid-walk — a write that evicts, for one.
    pub on_chain: Mutex<Option<ChainHook>>,
    /// Report every bounded walk as cut short: a subtree larger than any
    /// budget, without building one.
    pub truncate_subtrees: std::sync::atomic::AtomicBool,
}

/// What [`FakeHierarchy::on_chain`] runs.
pub type ChainHook = Box<dyn Fn() + Send + Sync>;

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

    /// Fail the next `n` `chain` calls, then answer normally.
    pub fn fail_next_chains(&self, n: usize) {
        self.failing_chains
            .store(n, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn chain_calls(&self) -> usize {
        self.chain_calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Every tenant below `tenant` that administration from it can reach:
    /// standalone subtrees left out.
    async fn reachable(&self, tenant: Uuid) -> Result<Vec<Uuid>, DomainError> {
        let ids: Vec<Uuid> = self.parents.lock().expect("lock").keys().copied().collect();
        let mut out = Vec::new();
        for candidate in ids {
            if candidate != tenant && self.is_within_subtree(tenant, candidate).await? {
                out.push(candidate);
            }
        }
        Ok(out)
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
        let transient = self
            .failing_chains
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |left| left.checked_sub(1),
            )
            .is_ok();
        if transient || self.unavailable.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(DomainError::Unavailable {
                detail: "tenant resolver down".to_owned(),
            });
        }
        if let Some(hook) = self.on_chain.lock().expect("lock").take() {
            hook();
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

    async fn descendants_bfs(
        &self,
        tenant: Uuid,
        budget: usize,
    ) -> Result<(Vec<Uuid>, bool), DomainError> {
        let reachable = self.reachable(tenant).await?;
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
        if self
            .truncate_subtrees
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            truncated = true;
        }
        Ok((order, truncated))
    }
}

// ---- The resolution harness: tree `root → a → b`, `c` a sibling of `a`, `s` a
// standalone child of `a`; a category, declarations and rows written through
// the real repositories over an in-memory database.

pub const BOOL: &str = "gts.cf.core.settings.type_bool_flag.v1~";
pub const SECRET: &str = "gts.cf.core.settings.type_secret_string.v1~";
/// A plain string, for the settings whose values are personal data rather than
/// credentials: classified `pii`, held inline, masked without the entitlement.
pub const TEXT: &str = "gts.cf.core.settings.type_plain_text.v1~";

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
        .with_type(
            TEXT,
            json!({ "$id": format!("gts://{TEXT}"), "type": "string" }),
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
        let platform: Arc<dyn PlatformScope> = Arc::new(FixedScope(tree.root));
        let resolver = Arc::new(ValueResolver::new(
            DeclarationRepo,
            ValueRepo,
            AccessRepo,
            Arc::clone(&hierarchy) as Arc<dyn crate::domain::resolution::TenantHierarchy>,
            Arc::clone(&platform),
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
            .set_status(
                &conn,
                &AccessScope::allow_all(),
                declaration_id,
                "retired",
                None,
            )
            .await
            .expect("retire");
    }

    pub async fn set(&self, declaration_id: Uuid, tenant: Uuid, value: Value) {
        self.write(declaration_id, tenant, Some(value), None, false, "public")
            .await;
    }

    /// An override carrying the classification the writer would denormalize
    /// from its declaration — `pii`, for a row the corpus rules must see as
    /// personal data.
    pub async fn set_classified(
        &self,
        declaration_id: Uuid,
        tenant: Uuid,
        value: Value,
        classification: &str,
    ) {
        self.write(
            declaration_id,
            tenant,
            Some(value),
            None,
            false,
            classification,
        )
        .await;
    }

    pub async fn set_flagged(&self, declaration_id: Uuid, tenant: Uuid, value: Value) {
        self.write(declaration_id, tenant, Some(value), None, true, "public")
            .await;
    }

    /// A secret override that stopped validating: held by reference, as every
    /// secret row is, and flagged for review.
    pub async fn set_flagged_secret(&self, declaration_id: Uuid, tenant: Uuid, secret_ref: &str) {
        self.write(
            declaration_id,
            tenant,
            None,
            Some(secret_ref.to_owned()),
            true,
            "secret",
        )
        .await;
    }

    pub async fn set_secret(&self, declaration_id: Uuid, tenant: Uuid, secret_ref: &str) {
        self.write(
            declaration_id,
            tenant,
            None,
            Some(secret_ref.to_owned()),
            false,
            "secret",
        )
        .await;
    }

    /// The row as the writer leaves it, `classification` denormalized from
    /// the declaration onto it — the column the corpus and the masking rules
    /// read.
    async fn write(
        &self,
        declaration_id: Uuid,
        tenant: Uuid,
        value: Option<Value>,
        secret_ref: Option<String>,
        flagged: bool,
        classification: &str,
    ) {
        let conn = self.db.conn().expect("connection");
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

/// A Secret Manager that keeps plaintext in memory under deterministic
/// references and remembers what it was asked to release.
#[derive(Default)]
pub struct RecordingSecrets {
    pub entries: Mutex<HashMap<String, String>>,
    pub deleted: Mutex<Vec<String>>,
    pub unavailable: std::sync::atomic::AtomicBool,
    pub stores: std::sync::atomic::AtomicUsize,
    /// The next store lands in the store and answers with a failure: the
    /// ambiguous outcome of a network that dropped the response.
    pub lose_next_answer: std::sync::atomic::AtomicBool,
}

impl RecordingSecrets {
    /// The references currently held, sorted.
    pub fn held(&self) -> Vec<String> {
        let mut refs: Vec<String> = self
            .entries
            .lock()
            .expect("secrets lock")
            .keys()
            .cloned()
            .collect();
        refs.sort();
        refs
    }

    /// Seed an entry the way a completed store would have left it.
    pub fn seed(&self, reference: &str, plaintext: &str) {
        self.entries
            .lock()
            .expect("secrets lock")
            .insert(reference.to_owned(), plaintext.to_owned());
    }

    /// Make the next store land and lose its answer.
    pub fn lose_next_answer(&self) {
        self.lose_next_answer
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Make every operation fail as unavailable from now on.
    pub fn go_down(&self) {
        self.unavailable
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn check(&self) -> Result<(), DomainError> {
        if self.unavailable.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(DomainError::Unavailable {
                detail: "the fake store is down".to_owned(),
            });
        }
        Ok(())
    }
}

#[async_trait]
impl crate::domain::ports::SecretManager for RecordingSecrets {
    fn mint_reference(&self, key: &str, tenant: Uuid) -> String {
        let n = self
            .stores
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("fake-{}-{tenant}-{n}", key.len())
    }

    async fn store_secret(
        &self,
        _key: &str,
        _tenant: Uuid,
        secret_ref: &str,
        plaintext: &Value,
    ) -> Result<(), DomainError> {
        self.check()?;
        let text = match plaintext {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        self.seed(secret_ref, &text);
        if self
            .lose_next_answer
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(DomainError::Unavailable {
                detail: "the store's answer was lost".to_owned(),
            });
        }
        Ok(())
    }

    async fn resolve_plaintext(
        &self,
        _key: &str,
        _tenant: Uuid,
        secret_ref: &str,
    ) -> Result<SecretString, DomainError> {
        self.check()?;
        self.entries
            .lock()
            .expect("secrets lock")
            .get(secret_ref)
            .cloned()
            .map(SecretString::from)
            .ok_or(DomainError::NotFound {
                resource: settings_service_sdk::gts::VALUE_SCHEMA,
            })
    }

    async fn delete_secret(
        &self,
        _key: &str,
        _tenant: Uuid,
        secret_ref: &str,
    ) -> Result<(), DomainError> {
        self.check()?;
        self.entries
            .lock()
            .expect("secrets lock")
            .remove(secret_ref);
        self.deleted
            .lock()
            .expect("secrets lock")
            .push(secret_ref.to_owned());
        Ok(())
    }
}

/// A gate that lets every caller resolve every setting.
pub struct AllowAllGate;

#[async_trait]
impl crate::domain::ports::SecretResolveGate for AllowAllGate {
    async fn may_resolve(
        &self,
        _ctx: &SecurityContext,
        _declaration_id: Uuid,
    ) -> Result<(), DomainError> {
        Ok(())
    }
}

/// A gate that refuses every caller and counts the refusals.
#[derive(Default)]
pub struct DenyAllGate {
    pub asked: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl crate::domain::ports::SecretResolveGate for DenyAllGate {
    async fn may_resolve(
        &self,
        _ctx: &SecurityContext,
        _declaration_id: Uuid,
    ) -> Result<(), DomainError> {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(DomainError::Unauthorized {
            resource: settings_service_sdk::gts::VALUE_SCHEMA,
        })
    }
}

// ---------------------------------------------------------------------------
// The REST harness
// ---------------------------------------------------------------------------

/// A policy decision point that allows everything, with no scope constraints.
///
/// The read surface asks with `require_constraints(false)`, so an unconstrained
/// allow yields an unrestricted `AccessScope` — the grant a platform
/// administrator holds. What a test exercises through it is therefore the
/// gear's **own** rules (the subtree check, tenant access, masking), not the
/// policy manager's, which is a different system with its own tests.
struct AllowAll;

#[async_trait]
impl authz_resolver_sdk::AuthZResolverApi for AllowAll {
    async fn evaluate(
        &self,
        _ctx: toolkit_security::PlatformSecurityContext,
        _request: authz_resolver_sdk::models::EvaluationRequest,
    ) -> Result<
        authz_resolver_sdk::models::EvaluationResponse,
        toolkit::api::canonical_prelude::CanonicalError,
    > {
        Ok(authz_resolver_sdk::models::EvaluationResponse {
            decision: true,
            context: authz_resolver_sdk::models::EvaluationResponseContext::default(),
        })
    }
}

/// A policy decision point that allows every action but the one that unmasks
/// `pii` values.
///
/// The entitlement is a separate decision from the read, and the interesting
/// caller is the one that holds the read and not the entitlement — an
/// administrator who may see that a setting is configured without seeing
/// personal data in it.
struct AllowButMasked;

#[async_trait]
impl authz_resolver_sdk::AuthZResolverApi for AllowButMasked {
    async fn evaluate(
        &self,
        _ctx: toolkit_security::PlatformSecurityContext,
        request: authz_resolver_sdk::models::EvaluationRequest,
    ) -> Result<
        authz_resolver_sdk::models::EvaluationResponse,
        toolkit::api::canonical_prelude::CanonicalError,
    > {
        Ok(authz_resolver_sdk::models::EvaluationResponse {
            decision: request.action.name != "read_unmasked",
            context: authz_resolver_sdk::models::EvaluationResponseContext::default(),
        })
    }
}

/// A policy decision point that denies everything.
struct DenyAll;

#[async_trait]
impl authz_resolver_sdk::AuthZResolverApi for DenyAll {
    async fn evaluate(
        &self,
        _ctx: toolkit_security::PlatformSecurityContext,
        _request: authz_resolver_sdk::models::EvaluationRequest,
    ) -> Result<
        authz_resolver_sdk::models::EvaluationResponse,
        toolkit::api::canonical_prelude::CanonicalError,
    > {
        Ok(authz_resolver_sdk::models::EvaluationResponse {
            decision: false,
            context: authz_resolver_sdk::models::EvaluationResponseContext::default(),
        })
    }
}

/// What a request answered with: the status, the decoded body, and the two
/// headers a conditional write needs to follow a read.
pub struct Answer {
    pub status: axum::http::StatusCode,
    pub body: Value,
    pub etag: Option<String>,
    pub location: Option<String>,
    /// Every response header as text, for the ones a test reads by name —
    /// the RFC 9470 challenge among them.
    pub headers: HashMap<String, String>,
}

/// The gear's read surface, registered exactly as the gear registers it and
/// answering real requests.
///
/// The point of going through the router rather than calling a handler is that
/// the parts only a request exercises are the parts that carry the contract:
/// the query string is parsed by the same extractors, an unsupported `OData`
/// option is refused where a client would meet it, and a refusal is rendered
/// by the error layer into the status code the API promises.
pub struct RestHarness {
    /// The resolution fixtures underneath: the database, the tenant tree and
    /// the seeding helpers.
    pub inner: ResolutionHarness,
    /// Where a secret's plaintext went, for the tests that assert it left the
    /// settings row.
    pub secrets: Arc<RecordingSecrets>,
    /// What the write path published, for the tests that assert the event as
    /// well as the answer.
    pub published: Arc<RecordingPublisher>,
    router: axum::Router,
}

impl RestHarness {
    /// Build the surface over a fresh database, with every authorization
    /// decision allowed and a step-up assertion that passes.
    pub async fn new() -> Self {
        Self::build(Arc::new(AllowAll), Arc::new(FixedStepUp::verified())).await
    }

    /// The same surface with every authorization decision denied, for the
    /// tests that assert the gate rather than what is behind it.
    pub async fn denying() -> Self {
        Self::build(Arc::new(DenyAll), Arc::new(FixedStepUp::verified())).await
    }

    /// The same surface for a caller that may read but may not see `pii`
    /// values unmasked.
    pub async fn without_pii_entitlement() -> Self {
        Self::build(Arc::new(AllowButMasked), Arc::new(FixedStepUp::verified())).await
    }

    /// The same surface where the caller's last re-authentication is too old,
    /// for the tests that assert the second gate.
    pub async fn stale_step_up() -> Self {
        Self::build(
            Arc::new(AllowAll),
            Arc::new(FixedStepUp::refusing(
                crate::domain::stepup::StepUpRefusal::Stale,
            )),
        )
        .await
    }

    async fn build(
        pdp: Arc<dyn authz_resolver_sdk::AuthZResolverApi>,
        step_up: Arc<dyn crate::domain::stepup::StepUpVerifier>,
    ) -> Self {
        let inner = ResolutionHarness::new().await;
        let enforcer = Arc::new(authz_resolver_sdk::PolicyEnforcer::new(pdp));
        let openapi = toolkit::api::OpenApiRegistryImpl::new();
        let search = Arc::new(crate::domain::search::service::SearchService::new(
            crate::infra::storage::search_repo::SearchRepo::new(inner.db.db().backend()),
        ));
        let categories = Arc::new(crate::domain::category::CategoryService::new(
            crate::infra::storage::category_repo::CategoryRepo,
            crate::infra::storage::audit_store::AuditStore,
        ));
        let router = crate::api::rest::routes::register_routes(
            axum::Router::new(),
            &openapi,
            categories,
            Arc::clone(&inner.db),
            Arc::clone(&enforcer),
        );
        let router = crate::api::rest::setting_routes::register_routes(
            router,
            &openapi,
            Arc::clone(&inner.resolver),
            Arc::clone(&inner.db),
            Arc::clone(&enforcer),
        );
        let router = crate::api::rest::search_routes::register_routes(
            router,
            &openapi,
            search,
            Arc::clone(&inner.resolver),
            Arc::clone(&inner.db),
            Arc::clone(&enforcer),
        );
        let access = Arc::new(crate::domain::access::AccessService::new(
            crate::infra::storage::declaration_repo::DeclarationRepo,
            crate::infra::storage::access_repo::AccessRepo,
            crate::infra::storage::audit_store::AuditStore,
            Arc::clone(&inner.hierarchy) as Arc<dyn TenantHierarchy>,
            Arc::new(FixedScope(inner.tree.root)) as Arc<dyn PlatformScope>,
            Arc::clone(&inner.cache),
        ));
        let router = crate::api::rest::access_routes::register_routes(
            router,
            &openapi,
            access,
            Arc::clone(&inner.db),
            Arc::clone(&enforcer),
        );
        // The declaration surface. The types registry is the SDK's own mock:
        // the reads only ask it for a type's traits, and an answer it does not
        // have degrades to an empty trait set rather than failing the read.
        let types: Arc<dyn types_registry_sdk::TypesRegistryClient> =
            Arc::new(types_registry_sdk::testing::MockTypesRegistryClient::new());
        let declarations = Arc::new(crate::domain::declaration::DeclarationService::new(
            crate::infra::storage::declaration_repo::DeclarationRepo,
            Arc::clone(&types),
        ));
        let step_up_for_writes = Arc::clone(&step_up);
        let admin = Arc::new(crate::domain::declaration::DeclarationAdmin::new(
            crate::infra::storage::declaration_repo::DeclarationRepo,
            crate::infra::storage::category_repo::CategoryRepo,
            crate::infra::storage::value_repo::ValueRepo,
            Arc::new(crate::infra::type_validator::GtsTypeValidator::new(
                resolution_catalogue(),
            )),
            Arc::new(RecordingRegistrar::default()),
            step_up,
            crate::infra::storage::audit_store::AuditStore,
            Arc::clone(&inner.cache),
        ));
        let router = crate::api::rest::declaration_routes::register_routes(
            router,
            &openapi,
            declarations,
            admin,
            Arc::clone(&inner.db),
            Arc::clone(&enforcer),
        );
        // The write surface. Every port behind it is doubled except the
        // repositories and the audit store, which run for real against the
        // same database the reads use.
        let secrets = Arc::new(RecordingSecrets::default());
        let published = Arc::new(RecordingPublisher::default());
        let coordinator = write_coordinator(
            &inner,
            Arc::clone(&secrets),
            Arc::clone(&published),
            step_up_for_writes,
        );
        let router = crate::api::rest::value_routes::register_routes(
            router,
            &openapi,
            coordinator,
            enforcer,
        );
        Self {
            inner,
            secrets,
            published,
            router,
        }
    }

    /// Send a `GET` as the given tenant's administrator and answer with the
    /// status and the decoded body.
    pub async fn get(&self, uri: &str, caller: Uuid) -> (axum::http::StatusCode, Value) {
        let answer = self.send("GET", uri, None, None, caller).await;
        (answer.status, answer.body)
    }

    /// Send any request: a method, a URI, an optional JSON body and an
    /// optional `If-Match`, as the given tenant's administrator.
    pub async fn send(
        &self,
        method: &str,
        uri: &str,
        body: Option<Value>,
        if_match: Option<&str>,
        caller: Uuid,
    ) -> Answer {
        self.send_as(method, uri, body, if_match, context_for(caller))
            .await
    }

    /// The same, as a caller the test builds itself — a service principal, for
    /// the rules that turn on there being a person behind the request.
    pub async fn send_as(
        &self,
        method: &str,
        uri: &str,
        body: Option<Value>,
        if_match: Option<&str>,
        caller: SecurityContext,
    ) -> Answer {
        let body = body.map(|json| serde_json::to_vec(&json).expect("serializes"));
        self.dispatch(method, uri, body, if_match, caller).await
    }

    /// The same, with the body sent as the exact text given — for a request
    /// whose literal spelling is the point, such as a number a double cannot
    /// hold, which a `serde_json::Value` would already have rounded.
    pub async fn send_text(
        &self,
        method: &str,
        uri: &str,
        body: &str,
        if_match: Option<&str>,
        caller: Uuid,
    ) -> Answer {
        self.dispatch(
            method,
            uri,
            Some(body.as_bytes().to_vec()),
            if_match,
            context_for(caller),
        )
        .await
    }

    async fn dispatch(
        &self,
        method: &str,
        uri: &str,
        body: Option<Vec<u8>>,
        if_match: Option<&str>,
        caller: SecurityContext,
    ) -> Answer {
        use tower::ServiceExt as _;
        let mut builder = axum::http::Request::builder().method(method).uri(uri);
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        if let Some(tag) = if_match {
            builder = builder.header("if-match", tag);
        }
        let payload = body.map_or_else(axum::body::Body::empty, axum::body::Body::from);
        let mut request = builder.body(payload).expect("a well-formed request");
        request.extensions_mut().insert(caller);
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("the router answers");
        let status = response.status();
        let etag = response
            .headers()
            .get(axum::http::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let location = response
            .headers()
            .get(axum::http::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let headers: HashMap<String, String> = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|v| (name.as_str().to_owned(), v.to_owned()))
            })
            .collect();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("a bounded body");
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Answer {
            status,
            body,
            etag,
            location,
            headers,
        }
    }

    /// Record an access restriction for a `(setting, tenant)` pair, the way an
    /// ancestor's administrator would.
    pub async fn restrict(
        &self,
        declaration_id: Uuid,
        tenant: Uuid,
        access: crate::domain::access::TenantAccess,
    ) {
        use crate::domain::access::AccessRepository as _;
        let conn = self.inner.db.conn().expect("connection");
        crate::infra::storage::access_repo::AccessRepo
            .upsert(
                &conn,
                &AccessScope::allow_all(),
                crate::domain::access::RestrictionDraft {
                    declaration_id,
                    tenant_id: tenant,
                    access,
                    set_by: "an ancestor's administrator".to_owned(),
                },
                None,
            )
            .await
            .expect("restriction");
    }

    /// The items of a paginated answer, or an empty list when the body is a
    /// problem document.
    pub async fn items(&self, uri: &str, caller: Uuid) -> Vec<Value> {
        let (_, body) = self.get(uri, caller).await;
        body.get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }
}

/// The write coordinator over a harness's database.
///
/// Every port behind it is doubled except the repositories and the audit
/// store, which run for real against the same database the reads use.
pub fn write_coordinator(
    inner: &ResolutionHarness,
    secrets: Arc<RecordingSecrets>,
    published: Arc<RecordingPublisher>,
    step_up: Arc<dyn crate::domain::stepup::StepUpVerifier>,
) -> Arc<crate::infra::value_writes::WriteCoordinator> {
    let writer = Arc::new(crate::domain::writes::ValueWriter::new(
        crate::infra::storage::value_repo::ValueRepo,
        Arc::clone(&inner.resolver),
        Arc::new(crate::infra::type_validator::GtsTypeValidator::new(
            resolution_catalogue(),
        )),
        crate::infra::storage::audit_store::AuditStore,
        step_up,
        secrets as Arc<dyn crate::domain::ports::SecretManager>,
        crate::infra::storage::pending_secret_repo::PendingSecretRepo,
        published as Arc<dyn crate::domain::ports::ChangePublisher>,
        Arc::new(crate::domain::ports::NoMetrics),
    ));
    Arc::new(crate::infra::value_writes::WriteCoordinator::new(
        Arc::clone(&inner.db),
        writer,
    ))
}

/// An interactive administrator of one tenant.
///
/// The subject is derived from the tenant rather than drawn fresh, so the same
/// administrator is recognisable across requests — which is what a single-use
/// token staged by one request and claimed by the next depends on.
pub fn context_for(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v5(&Uuid::NAMESPACE_OID, tenant.as_bytes()))
        .subject_tenant_id(tenant)
        .subject_type(crate::domain::stepup::USER_SUBJECT_TYPE)
        .build()
        .expect("context")
}

/// A service principal of one tenant: no person behind it, so a declaration
/// that requires a recent re-authentication has nobody to ask.
pub fn service_context_for(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v5(&Uuid::NAMESPACE_DNS, tenant.as_bytes()))
        .subject_tenant_id(tenant)
        .subject_type("gts.cf.core.security.subject_service.v1~")
        .build()
        .expect("context")
}
