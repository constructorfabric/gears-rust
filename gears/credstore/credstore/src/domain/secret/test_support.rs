//! Shared test infrastructure for domain-layer unit tests.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InTenantSubtreePredicate, Predicate};
use authz_resolver_sdk::models::{
    Capability, EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError, PolicyEnforcer};
use credstore_sdk::{
    CredStoreError, CredStorePluginClientV1, OwnerId, SecretRef, SecretValue, SharingMode, TenantId,
};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::error::DomainError;
pub use crate::domain::ports::metrics::NoopMetrics;
use crate::domain::ports::metrics::{
    CredStoreMetricsPort, Dep, DepOp, FenceVerify, Outcome, ReadOutcome, SecretCounts,
};
use crate::domain::ports::plugin::PluginSelector;
use crate::domain::resolver::TenantDirectory;
use crate::domain::secret::model::{NewSecret, SecretRow, SecretStatus};
use crate::domain::secret::repo::SecretRepo;
use crate::domain::secret::type_resolver::{ResolvedSecretType, SecretTypeResolver};
use crate::domain::secret::typing::reasons;
use time::OffsetDateTime;

// ── SecurityContext helpers ───────────────────────────────────────────────────

/// Build a minimal [`SecurityContext`] for unit tests with custom subject and tenant.
///
/// # Panics
///
/// Panics if the builder fails (only possible on missing fields which we always supply).
#[must_use]
pub fn make_ctx(subject_id: Uuid, tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject_id)
        .subject_tenant_id(tenant_id)
        .build()
        .expect("test ctx")
}

// ── mock PolicyEnforcer ───────────────────────────────────────────────────────

/// Permissive PDP fake: emits one `InTenantSubtree` permit rooted at the
/// caller's `subject_tenant_id` (the slot the PEP populates). The repo fakes
/// ignore scope contents, so this only has to compile to a valid scope under
/// `require_constraints(true)`.
struct MockAuthZResolver;

#[async_trait]
impl AuthZResolverClient for MockAuthZResolver {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let root = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
            .expect("MockAuthZResolver: subject.properties[\"tenant_id\"]; build ctx via make_ctx");
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::InTenantSubtree(InTenantSubtreePredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        root,
                    ))],
                }],
                ..Default::default()
            },
        })
    }
}

/// Permissive [`PolicyEnforcer`] for `Service` unit tests.
#[must_use]
pub fn mock_enforcer() -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(MockAuthZResolver))
        .with_capabilities(vec![Capability::TenantHierarchy])
}

/// PDP fake that always denies (`decision: false`).
struct DenyAuthZResolver;

#[async_trait]
impl AuthZResolverClient for DenyAuthZResolver {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext::default(),
        })
    }
}

/// Denying [`PolicyEnforcer`] — drives `scope_for` → `DomainError::AccessDenied`.
#[must_use]
pub fn deny_enforcer() -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(DenyAuthZResolver))
        .with_capabilities(vec![Capability::TenantHierarchy])
}

/// Type-aware PDP fake: permissive (like [`mock_enforcer`]) except for the
/// listed resource-type ids, which are denied. Records every evaluated
/// resource type so tests can assert what the PEP targeted.
pub struct TypeAwareAuthZResolver {
    denied_types: Vec<String>,
    pub seen_resource_types: Mutex<Vec<String>>,
}

#[async_trait]
impl AuthZResolverClient for TypeAwareAuthZResolver {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        self.seen_resource_types
            .lock()
            .expect("lock")
            .push(request.resource.resource_type.clone());
        if self.denied_types.contains(&request.resource.resource_type) {
            return Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext::default(),
            });
        }
        MockAuthZResolver.evaluate(request).await
    }
}

/// Enforcer denying exactly the given resource-type ids; returns the
/// resolver too so tests can inspect the evaluated types.
#[must_use]
pub fn type_deny_enforcer(
    denied_types: Vec<String>,
) -> (PolicyEnforcer, Arc<TypeAwareAuthZResolver>) {
    let resolver = Arc::new(TypeAwareAuthZResolver {
        denied_types,
        seen_resource_types: Mutex::new(Vec::new()),
    });
    (
        PolicyEnforcer::new(resolver.clone()).with_capabilities(vec![Capability::TenantHierarchy]),
        resolver,
    )
}

/// PDP fake that always returns a transport failure.
struct FailAuthZResolver;

#[async_trait]
impl AuthZResolverClient for FailAuthZResolver {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Err(AuthZResolverError::ServiceUnavailable(
            "failing_enforcer: simulated PDP transport failure".to_owned(),
        ))
    }
}

/// Failing [`PolicyEnforcer`] — drives `scope_for` → `DomainError::ServiceUnavailable`.
#[must_use]
pub fn failing_enforcer() -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(FailAuthZResolver))
        .with_capabilities(vec![Capability::TenantHierarchy])
}

// ── Fake type resolvers ───────────────────────────────────────────────────────

/// Catalog-backed [`SecretTypeResolver`]: resolves the built-in catalog
/// types by their deterministic UUID, mirroring what the production
/// resolver returns for the registry-seeded schemas. Unknown UUIDs map to
/// `UNKNOWN_SECRET_TYPE`, like an unregistered type.
pub struct CatalogTypeResolver;

#[async_trait]
impl SecretTypeResolver for CatalogTypeResolver {
    async fn resolve(&self, type_uuid: Uuid) -> Result<ResolvedSecretType, DomainError> {
        credstore_sdk::SECRET_TYPE_CATALOG
            .iter()
            .find(|d| credstore_sdk::types::type_uuid(d.gts_id) == Some(type_uuid))
            .map(|d| ResolvedSecretType {
                gts_id: d.gts_id.to_owned(),
                traits: d.traits(),
            })
            .ok_or_else(|| DomainError::TypeViolation {
                field: "type",
                reason: reasons::UNKNOWN_SECRET_TYPE,
                detail: format!("secret type {type_uuid} is not registered"),
            })
    }
}

/// Catalog-backed resolver as an `Arc<dyn SecretTypeResolver>` — the
/// default for `Service` unit tests.
#[must_use]
pub fn catalog_type_resolver() -> Arc<dyn SecretTypeResolver> {
    Arc::new(CatalogTypeResolver)
}

/// [`SecretTypeResolver`] that always fails with `ServiceUnavailable` —
/// drives the registry-outage (503) paths.
pub struct FailingTypeResolver;

#[async_trait]
impl SecretTypeResolver for FailingTypeResolver {
    async fn resolve(&self, _type_uuid: Uuid) -> Result<ResolvedSecretType, DomainError> {
        Err(DomainError::ServiceUnavailable {
            detail: "types-registry unavailable".to_owned(),
            retry_after: None,
            cause: None,
        })
    }
}

// ── FakeDir ───────────────────────────────────────────────────────────────────

/// Returns a preset ancestor chain (self first, root last).
pub struct FakeDir {
    chain: Vec<Uuid>,
}

impl FakeDir {
    #[must_use]
    pub fn new(chain: Vec<Uuid>) -> Self {
        Self { chain }
    }

    /// Single-tenant chain (only self).
    #[must_use]
    pub fn single(id: Uuid) -> Self {
        Self { chain: vec![id] }
    }
}

#[async_trait]
impl TenantDirectory for FakeDir {
    async fn ancestor_chain(
        &self,
        _ctx: &SecurityContext,
        _req: TenantId,
    ) -> Result<Vec<Uuid>, DomainError> {
        Ok(self.chain.clone())
    }
}

// ── FakePlugin ────────────────────────────────────────────────────────────────

/// Key: `(tenant_id, reference, owner_id)`.
type PluginKey = (Uuid, String, Option<Uuid>);

/// In-memory plugin store.
pub struct FakePlugin {
    store: Mutex<HashMap<PluginKey, Vec<u8>>>,
    /// Number of upcoming `put` calls that fail before the plugin recovers,
    /// simulating a transient backend outage mid create-saga.
    put_failures: Mutex<usize>,
    /// Number of upcoming `delete` calls that fail before the plugin
    /// recovers, simulating a transient backend outage mid delete-saga.
    delete_failures: Mutex<usize>,
    /// When set, every `get` returns [`CredStoreError::AccessDenied`],
    /// modelling a backend whose own ACLs reject a read the gear's PDP has
    /// already allowed.
    get_denied: bool,
    /// Count of backend reads of the reserved fence-key entry — lets tests
    /// assert the mismatch-triggered refresh does not re-read the key on every
    /// poisoned get (the cache-thrash guard).
    fence_key_gets: AtomicUsize,
}

impl FakePlugin {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            store: Mutex::new(HashMap::new()),
            put_failures: Mutex::new(0),
            delete_failures: Mutex::new(0),
            get_denied: false,
            fence_key_gets: AtomicUsize::new(0),
        })
    }

    /// Plugin that fails the next `n` `put` calls, then behaves normally.
    #[must_use]
    pub fn with_put_failures(n: usize) -> Arc<Self> {
        Arc::new(Self {
            store: Mutex::new(HashMap::new()),
            put_failures: Mutex::new(n),
            delete_failures: Mutex::new(0),
            get_denied: false,
            fence_key_gets: AtomicUsize::new(0),
        })
    }

    /// Plugin that fails the next `n` `delete` calls, then behaves normally.
    #[must_use]
    pub fn with_delete_failures(n: usize) -> Arc<Self> {
        Arc::new(Self {
            store: Mutex::new(HashMap::new()),
            put_failures: Mutex::new(0),
            delete_failures: Mutex::new(n),
            get_denied: false,
            fence_key_gets: AtomicUsize::new(0),
        })
    }

    /// Plugin whose every `get` denies — models a backend ACL rejecting a read
    /// the gear's PDP already allowed.
    #[must_use]
    pub fn with_get_denied() -> Arc<Self> {
        Arc::new(Self {
            store: Mutex::new(HashMap::new()),
            put_failures: Mutex::new(0),
            delete_failures: Mutex::new(0),
            get_denied: true,
            fence_key_gets: AtomicUsize::new(0),
        })
    }

    /// Number of backend reads of the reserved fence-key entry so far.
    ///
    /// # Panics
    ///
    /// Never panics.
    #[must_use]
    pub fn fence_key_gets(&self) -> usize {
        self.fence_key_gets.load(Ordering::Relaxed)
    }

    /// True when the store holds a value for `(tenant, key, owner)`.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn contains(
        &self,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> bool {
        let k = Self::key(tenant_id, key, owner_id);
        self.store.lock().expect("lock").contains_key(&k)
    }

    /// Insert a value directly into the store, bypassing the failure
    /// injection — used to pre-seed the reserved fence-key entry so tests
    /// that inject `put` failures exercise the *value* write, not the fence
    /// key bootstrap.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn seed_value(
        &self,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
        bytes: &[u8],
    ) {
        let k = Self::key(tenant_id, key, owner_id);
        self.store.lock().expect("lock").insert(k, bytes.to_vec());
    }

    /// Pre-seed the reserved fence-key entry (32 fixed bytes), so the fence
    /// bootstrap is a pure read and never consumes an injected `put` failure.
    ///
    /// # Panics
    ///
    /// Panics if the fence key reference is invalid (it is a tested constant).
    pub fn seed_fence_key(&self) {
        let key_ref = SecretRef::new(crate::domain::secret::fence::FENCE_KEY_REF)
            .expect("valid fence key ref");
        self.seed_value(
            &TenantId(Uuid::nil()),
            &key_ref,
            None,
            &[42u8; crate::domain::secret::fence::FENCE_KEY_LEN],
        );
    }

    fn key(tenant_id: &TenantId, key: &SecretRef, owner_id: Option<&OwnerId>) -> PluginKey {
        (tenant_id.0, key.as_ref().to_owned(), owner_id.map(|o| o.0))
    }
}

impl Default for FakePlugin {
    fn default() -> Self {
        Self {
            store: Mutex::new(HashMap::new()),
            put_failures: Mutex::new(0),
            delete_failures: Mutex::new(0),
            get_denied: false,
            fence_key_gets: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl CredStorePluginClientV1 for FakePlugin {
    async fn get(
        &self,
        _ctx: &SecurityContext,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Result<Option<SecretValue>, CredStoreError> {
        if key.as_ref() == crate::domain::secret::fence::FENCE_KEY_REF {
            self.fence_key_gets.fetch_add(1, Ordering::Relaxed);
        }
        if self.get_denied {
            return Err(CredStoreError::AccessDenied);
        }
        let k = Self::key(tenant_id, key, owner_id);
        let guard = self.store.lock().expect("lock");
        Ok(guard.get(&k).map(|v| SecretValue::new(v.clone())))
    }

    async fn put(
        &self,
        _ctx: &SecurityContext,
        tenant_id: &TenantId,
        key: &SecretRef,
        value: SecretValue,
        owner_id: Option<&OwnerId>,
    ) -> Result<(), CredStoreError> {
        {
            let mut remaining = self.put_failures.lock().expect("lock");
            if *remaining > 0 {
                *remaining -= 1;
                return Err(CredStoreError::Internal(
                    "simulated backend put failure".to_owned(),
                ));
            }
        }
        let k = Self::key(tenant_id, key, owner_id);
        self.store
            .lock()
            .expect("lock")
            .insert(k, value.as_bytes().to_vec());
        Ok(())
    }

    async fn delete(
        &self,
        _ctx: &SecurityContext,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Result<(), CredStoreError> {
        {
            let mut remaining = self.delete_failures.lock().expect("lock");
            if *remaining > 0 {
                *remaining -= 1;
                return Err(CredStoreError::Internal(
                    "simulated backend delete failure".to_owned(),
                ));
            }
        }
        let k = Self::key(tenant_id, key, owner_id);
        self.store.lock().expect("lock").remove(&k);
        Ok(())
    }
}

// ── FakePluginSelector ────────────────────────────────────────────────────────

pub struct FakePluginSelector {
    plugin: Arc<FakePlugin>,
}

impl FakePluginSelector {
    #[must_use]
    pub fn new(plugin: Arc<FakePlugin>) -> Self {
        Self { plugin }
    }
}

#[async_trait]
impl PluginSelector for FakePluginSelector {
    async fn resolve(&self) -> Result<Arc<dyn CredStorePluginClientV1>, DomainError> {
        Ok(self.plugin.clone())
    }
}

/// [`PluginSelector`] that always fails to resolve a plugin — models the
/// `NoPluginAvailable` (misconfigured / unregistered backend) 503 path.
pub struct NoPluginSelector;

#[async_trait]
impl PluginSelector for NoPluginSelector {
    async fn resolve(&self) -> Result<Arc<dyn CredStorePluginClientV1>, DomainError> {
        Err(DomainError::ServiceUnavailable {
            detail: "no storage plugin registered".to_owned(),
            retry_after: None,
            cause: None,
        })
    }
}

// ── FakeSecretRepo ────────────────────────────────────────────────────────────

/// In-memory [`SecretRepo`].
///
/// `scope_allows` controls the result of [`SecretRepo::scope_includes_tenant`].
// Independent failure-injection toggles for a test double, not a state machine.
#[allow(clippy::struct_excessive_bools)]
pub struct FakeSecretRepo {
    rows: Mutex<Vec<SecretRow>>,
    pub scope_allows: bool,
    /// When set, a unique-violation in `insert_provisioning` first promotes the
    /// conflicting Provisioning row(s) to Active before returning Conflict —
    /// simulating the create-race winner finishing its saga, so the service's
    /// bounded retry can resolve to the update path deterministically.
    promote_on_conflict: bool,
    /// When set, `delete_by_id` returns an error — simulating a DB failure on
    /// the create-saga rollback path (the reference then stays wedged).
    fail_delete: bool,
    /// When set, `delete_by_id` matches 0 rows (`NotFound`) — simulating a row
    /// that moved/vanished between `find_own` and the conditional delete.
    delete_not_found: bool,
    /// When set, `mark_deprovisioning` matches 0 rows — simulating a row that
    /// moved/vanished between `find_own` and the gated status flip.
    mark_not_found: bool,
    /// When set, `touch` matches 0 rows (`Ok(None)`) — simulating a row that was
    /// concurrently deleted/reaped between `find_for_write` and the version bump
    /// on the overwrite path.
    touch_not_found: bool,
    /// When set, `list_stale_pending` returns each provisioning row as a stale
    /// snapshot but atomically flips the stored row to `Active` — simulating a
    /// slow create saga's `mark_active` landing between the reaper's list and
    /// its status-gated delete. The reaper must then leave the now-active row
    /// (and its backend value) alone.
    promote_provisioning_on_list: bool,
}

impl FakeSecretRepo {
    #[must_use]
    pub fn new() -> Self {
        Self {
            rows: Mutex::new(Vec::new()),
            scope_allows: true,
            promote_on_conflict: false,
            fail_delete: false,
            delete_not_found: false,
            mark_not_found: false,
            touch_not_found: false,
            promote_provisioning_on_list: false,
        }
    }

    /// Repo whose `list_stale_pending` flips each returned provisioning row to
    /// `Active` — simulates a slow create saga winning the race against the
    /// reaper between the list and the status-gated delete.
    #[must_use]
    pub fn with_provisioning_promoted_on_list() -> Self {
        Self {
            promote_provisioning_on_list: true,
            ..Self::new()
        }
    }

    #[must_use]
    pub fn with_scope_allows(scope_allows: bool) -> Self {
        Self {
            scope_allows,
            ..Self::new()
        }
    }

    #[must_use]
    pub fn with_promote_on_conflict(promote_on_conflict: bool) -> Self {
        Self {
            promote_on_conflict,
            ..Self::new()
        }
    }

    /// Repo whose `delete_by_id` always fails — exercises the rollback-failed path.
    #[must_use]
    pub fn with_delete_failure() -> Self {
        Self {
            fail_delete: true,
            ..Self::new()
        }
    }

    /// Repo whose `delete_by_id` matches 0 rows — simulates a row that moved or
    /// vanished between `find_own` and the conditional delete.
    #[must_use]
    pub fn with_delete_not_found() -> Self {
        Self {
            delete_not_found: true,
            ..Self::new()
        }
    }

    /// Repo whose `touch` matches 0 rows (`Ok(None)`) — simulates a row that was
    /// concurrently deleted/reaped between `find_for_write` and the version bump
    /// on the overwrite path.
    #[must_use]
    pub fn with_touch_not_found() -> Self {
        Self {
            touch_not_found: true,
            ..Self::new()
        }
    }

    /// Repo whose `mark_deprovisioning` matches 0 rows — simulates a row that
    /// moved or vanished between `find_own` and the gated status flip.
    #[must_use]
    pub fn with_mark_not_found() -> Self {
        Self {
            mark_not_found: true,
            ..Self::new()
        }
    }

    /// Seed rows directly (for pre-seeding parent/inherited state).
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn seed(&self, row: SecretRow) {
        self.rows.lock().expect("lock").push(row);
    }

    /// Snapshot of all rows (for asserting saga state).
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn rows(&self) -> Vec<SecretRow> {
        self.rows.lock().expect("lock").clone()
    }
}

impl Default for FakeSecretRepo {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SecretRepo for FakeSecretRepo {
    async fn resolve_for_get(
        &self,
        req_tenant: TenantId,
        subject: OwnerId,
        key: &SecretRef,
        chain: &[Uuid],
    ) -> Result<Option<SecretRow>, DomainError> {
        let rows = self.rows.lock().expect("lock");
        let key_str = key.as_ref();

        // Winner: closest tenant position; private beats non-private at same level.
        // Candidates: active, reference matches, tenant in chain, sharing rules.
        let pos = |t: Uuid| chain.iter().position(|c| *c == t).unwrap_or(usize::MAX);
        let best = rows
            .iter()
            .filter(|r| {
                r.status == SecretStatus::Active
                    && r.expires_at.is_none_or(|at| at > OffsetDateTime::now_utc())
                    && r.reference == key_str
                    && chain.contains(&r.tenant_id.0)
                    && match r.sharing {
                        SharingMode::Private => r.owner_id.0 == subject.0,
                        SharingMode::Tenant => r.tenant_id == req_tenant,
                        SharingMode::Shared => true,
                    }
            })
            .min_by(|a, b| {
                pos(a.tenant_id.0).cmp(&pos(b.tenant_id.0)).then(
                    (a.sharing != SharingMode::Private).cmp(&(b.sharing != SharingMode::Private)),
                )
            });
        Ok(best.cloned())
    }

    async fn insert_provisioning(
        &self,
        _scope: &AccessScope,
        new: &NewSecret,
    ) -> Result<(), DomainError> {
        let mut rows = self.rows.lock().expect("lock");
        // Enforce uniqueness: (tenant_id, reference, sharing class).
        // For private: (tenant_id, reference, owner_id) must be unique.
        // For tenant/shared: (tenant_id, reference) must be unique among non-private.
        let conflict = rows.iter().any(|r| {
            r.tenant_id == new.tenant_id
                && r.reference == new.reference.as_ref()
                && match new.sharing {
                    SharingMode::Private => {
                        r.sharing == SharingMode::Private && r.owner_id == new.owner_id
                    }
                    _ => r.sharing != SharingMode::Private,
                }
        });
        if conflict {
            if self.promote_on_conflict {
                for r in rows.iter_mut().filter(|r| {
                    r.tenant_id == new.tenant_id
                        && r.reference == new.reference.as_ref()
                        && r.status == SecretStatus::Provisioning
                }) {
                    r.status = SecretStatus::Active;
                }
            }
            return Err(DomainError::Conflict);
        }
        rows.push(SecretRow {
            id: new.id,
            tenant_id: new.tenant_id,
            reference: new.reference.as_ref().to_owned(),
            sharing: new.sharing,
            owner_id: new.owner_id,
            status: SecretStatus::Provisioning,
            version: 1,
            secret_type_uuid: new.secret_type_uuid,
            expires_at: new.expires_at,
            value_fp: Some(new.value_fp.clone()),
            fp_key_id: Some(new.fp_key_id),
        });
        Ok(())
    }

    async fn mark_active(&self, _scope: &AccessScope, id: Uuid) -> Result<(), DomainError> {
        let mut rows = self.rows.lock().expect("lock");
        let row = rows.iter_mut().find(|r| r.id == id);
        match row {
            Some(r) => {
                r.status = SecretStatus::Active;
                Ok(())
            }
            None => Err(DomainError::Conflict),
        }
    }

    async fn touch(
        &self,
        _scope: &AccessScope,
        id: Uuid,
        sharing: SharingMode,
        expected_version: Option<i64>,
        expires_at: Option<OffsetDateTime>,
        value_fp: Vec<u8>,
    ) -> Result<Option<SecretRow>, DomainError> {
        if self.touch_not_found {
            return Ok(None);
        }
        let mut rows = self.rows.lock().expect("lock");
        let row = rows.iter_mut().find(|r| {
            r.id == id
                && r.status == SecretStatus::Active
                && expected_version.is_none_or(|v| r.version == v)
        });
        match row {
            Some(r) => {
                r.version += 1;
                r.sharing = sharing;
                r.expires_at = expires_at;
                r.value_fp = Some(value_fp);
                r.fp_key_id = Some(crate::domain::secret::fence::CURRENT_FENCE_KEY_ID);
                Ok(Some(r.clone()))
            }
            None => Ok(None),
        }
    }

    async fn backfill_fp(
        &self,
        id: Uuid,
        value_fp: Vec<u8>,
        fp_key_id: i16,
    ) -> Result<bool, DomainError> {
        let mut rows = self.rows.lock().expect("lock");
        let row = rows.iter_mut().find(|r| r.id == id && r.value_fp.is_none());
        match row {
            Some(r) => {
                r.value_fp = Some(value_fp);
                r.fp_key_id = Some(fp_key_id);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn list_unfenced(&self, limit: u64) -> Result<Vec<SecretRow>, DomainError> {
        let rows = self.rows.lock().expect("lock");
        Ok(rows
            .iter()
            .filter(|r| r.status == SecretStatus::Active && r.value_fp.is_none())
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .cloned()
            .collect())
    }

    async fn find_own(
        &self,
        _scope: &AccessScope,
        tenant: TenantId,
        subject: OwnerId,
        key: &SecretRef,
    ) -> Result<Option<SecretRow>, DomainError> {
        let rows = self.rows.lock().expect("lock");
        let key_str = key.as_ref();
        // Prefer private row. Active + deprovisioning (saga resume), never
        // provisioning — mirrors the SQL implementation.
        let best = rows
            .iter()
            .filter(|r| {
                r.tenant_id == tenant
                    && r.reference == key_str
                    && matches!(
                        r.status,
                        SecretStatus::Active | SecretStatus::Deprovisioning
                    )
                    && match r.sharing {
                        SharingMode::Private => r.owner_id.0 == subject.0,
                        _ => true,
                    }
            })
            .min_by_key(|r| i32::from(r.sharing != SharingMode::Private));
        Ok(best.cloned())
    }

    async fn find_for_write(
        &self,
        _scope: &AccessScope,
        tenant: TenantId,
        subject: OwnerId,
        key: &SecretRef,
        sharing: SharingMode,
    ) -> Result<Option<SecretRow>, DomainError> {
        let rows = self.rows.lock().expect("lock");
        let key_str = key.as_ref();
        // Address only the target sharing class — private and non-private secrets
        // coexist under one (tenant, ref); a write of one class ignores the other.
        let row = rows.iter().find(|r| {
            r.tenant_id == tenant
                && r.reference == key_str
                && r.status == SecretStatus::Active
                && match sharing {
                    SharingMode::Private => {
                        r.sharing == SharingMode::Private && r.owner_id == subject
                    }
                    _ => r.sharing != SharingMode::Private,
                }
        });
        Ok(row.cloned())
    }

    async fn delete_by_id(
        &self,
        _scope: &AccessScope,
        id: Uuid,
        expected_version: Option<i64>,
    ) -> Result<(), DomainError> {
        if self.fail_delete {
            return Err(DomainError::internal("simulated delete failure"));
        }
        if self.delete_not_found {
            return Err(DomainError::NotFound);
        }
        let mut rows = self.rows.lock().expect("lock");
        let len_before = rows.len();
        rows.retain(|r| !(r.id == id && expected_version.is_none_or(|v| r.version == v)));
        if rows.len() == len_before {
            Err(DomainError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn mark_deprovisioning(
        &self,
        _scope: &AccessScope,
        id: Uuid,
        expected_version: Option<i64>,
    ) -> Result<bool, DomainError> {
        if self.mark_not_found {
            return Ok(false);
        }
        let mut rows = self.rows.lock().expect("lock");
        let row = rows.iter_mut().find(|r| {
            r.id == id
                && r.status == SecretStatus::Active
                && expected_version.is_none_or(|v| r.version == v)
        });
        match row {
            Some(r) => {
                r.status = SecretStatus::Deprovisioning;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn list_stale_pending(
        &self,
        _provisioning_older_than_secs: u64,
        _deprovisioning_older_than_secs: u64,
        limit: u64,
    ) -> Result<Vec<SecretRow>, DomainError> {
        // The fake has no timestamps: every non-active row counts as stale.
        let mut rows = self.rows.lock().expect("lock");
        let stale: Vec<SecretRow> = rows
            .iter()
            .filter(|r| r.status != SecretStatus::Active)
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .cloned()
            .collect();
        if self.promote_provisioning_on_list {
            // Simulate mark_active landing between list and reap: the returned
            // snapshot still reads Provisioning, but the stored row is now Active.
            for r in rows.iter_mut() {
                if r.status == SecretStatus::Provisioning {
                    r.status = SecretStatus::Active;
                }
            }
        }
        Ok(stale)
    }

    async fn reap_by_id(&self, id: Uuid, expected: SecretStatus) -> Result<bool, DomainError> {
        if self.fail_delete {
            return Err(DomainError::internal("simulated reap failure"));
        }
        let mut rows = self.rows.lock().expect("lock");
        let before = rows.len();
        // Status-gated like the real repo: only remove the row if it still
        // holds the status the reaper observed.
        rows.retain(|r| !(r.id == id && r.status == expected));
        Ok(rows.len() != before)
    }

    async fn mark_expired_deprovisioning(&self) -> Result<u64, DomainError> {
        let now = OffsetDateTime::now_utc();
        let mut rows = self.rows.lock().expect("lock");
        let mut flipped = 0u64;
        for r in rows.iter_mut() {
            if r.status == SecretStatus::Active && r.expires_at.is_some_and(|at| at <= now) {
                r.status = SecretStatus::Deprovisioning;
                flipped += 1;
            }
        }
        Ok(flipped)
    }

    async fn inventory(&self) -> Result<SecretCounts, DomainError> {
        let rows = self.rows.lock().expect("lock");
        let mut counts = SecretCounts::default();
        let tenants: std::collections::HashSet<Uuid> = rows
            .iter()
            .filter(|r| r.status == SecretStatus::Active)
            .map(|r| r.tenant_id.0)
            .collect();
        #[allow(clippy::cast_possible_wrap)]
        let tenant_count = tenants.len() as i64;
        counts.tenants = tenant_count;
        for r in rows.iter() {
            match r.status {
                SecretStatus::Provisioning => counts.provisioning += 1,
                SecretStatus::Deprovisioning => counts.deprovisioning += 1,
                SecretStatus::Active => match r.sharing {
                    SharingMode::Private => counts.private += 1,
                    SharingMode::Tenant => counts.tenant += 1,
                    SharingMode::Shared => counts.shared += 1,
                },
            }
        }
        Ok(counts)
    }

    async fn scope_includes_tenant(
        &self,
        _scope: &AccessScope,
        _tenant: Uuid,
    ) -> Result<bool, DomainError> {
        Ok(self.scope_allows)
    }
}

// ── FakeMetrics ───────────────────────────────────────────────────────────────

/// Recording metrics fake for assertions in tests.
pub struct FakeMetrics {
    pub cross_tenant_denied_count: Mutex<u64>,
    pub read_outcomes: Mutex<Vec<ReadOutcome>>,
    pub deps: Mutex<Vec<(Dep, DepOp, Outcome)>>,
    pub provisioning_rollbacks: Mutex<Vec<Outcome>>,
    pub provisioning_reaped_total: Mutex<u64>,
    pub deprovisioning_reaped_total: Mutex<u64>,
    pub fence_verifies: Mutex<Vec<FenceVerify>>,
    pub fence_backfills: Mutex<Vec<Outcome>>,
}

impl FakeMetrics {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Total secrets counted by `provisioning_reaped`.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn provisioning_reaped_total(&self) -> u64 {
        *self.provisioning_reaped_total.lock().expect("lock")
    }

    /// Total secrets counted by `deprovisioning_reaped`.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn deprovisioning_reaped_total(&self) -> u64 {
        *self.deprovisioning_reaped_total.lock().expect("lock")
    }

    /// Returns all recorded provisioning-rollback outcomes.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn provisioning_rollbacks(&self) -> Vec<Outcome> {
        self.provisioning_rollbacks.lock().expect("lock").clone()
    }

    /// Returns all recorded dependency `(dep, op, outcome)` tuples.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn deps(&self) -> Vec<(Dep, DepOp, Outcome)> {
        self.deps.lock().expect("lock").clone()
    }

    /// Returns the number of times [`CredStoreMetricsPort::cross_tenant_denied`] was called.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned (only possible after a panic in another thread).
    pub fn cross_tenant_denied_count(&self) -> u64 {
        *self.cross_tenant_denied_count.lock().expect("lock")
    }

    /// Returns the last recorded [`ReadOutcome`], if any.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn last_read_outcome(&self) -> Option<ReadOutcome> {
        self.read_outcomes.lock().expect("lock").last().copied()
    }

    /// Returns all recorded fence-verify verdicts.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn fence_verifies(&self) -> Vec<FenceVerify> {
        self.fence_verifies.lock().expect("lock").clone()
    }

    /// Returns all recorded fence-backfill outcomes.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn fence_backfills(&self) -> Vec<Outcome> {
        self.fence_backfills.lock().expect("lock").clone()
    }
}

impl Default for FakeMetrics {
    fn default() -> Self {
        Self {
            cross_tenant_denied_count: Mutex::new(0),
            read_outcomes: Mutex::new(Vec::new()),
            deps: Mutex::new(Vec::new()),
            provisioning_rollbacks: Mutex::new(Vec::new()),
            provisioning_reaped_total: Mutex::new(0),
            deprovisioning_reaped_total: Mutex::new(0),
            fence_verifies: Mutex::new(Vec::new()),
            fence_backfills: Mutex::new(Vec::new()),
        }
    }
}

impl CredStoreMetricsPort for FakeMetrics {
    fn record_inventory(&self, _counts: SecretCounts) {}
    fn read_outcome(&self, outcome: ReadOutcome) {
        self.read_outcomes.lock().expect("lock").push(outcome);
    }
    fn walkup_depth(&self, _depth: u64) {}
    fn dependency(&self, dep: Dep, op: DepOp, outcome: Outcome, _secs: f64) {
        self.deps.lock().expect("lock").push((dep, op, outcome));
    }
    fn provisioning_reaped(&self, n: u64) {
        *self.provisioning_reaped_total.lock().expect("lock") += n;
    }
    fn deprovisioning_reaped(&self, n: u64) {
        *self.deprovisioning_reaped_total.lock().expect("lock") += n;
    }
    fn provisioning_rollback(&self, outcome: Outcome) {
        self.provisioning_rollbacks
            .lock()
            .expect("lock")
            .push(outcome);
    }
    fn cross_tenant_denied(&self) {
        *self.cross_tenant_denied_count.lock().expect("lock") += 1;
    }
    fn fence_verify(&self, outcome: FenceVerify) {
        self.fence_verifies.lock().expect("lock").push(outcome);
    }
    fn fence_backfill(&self, outcome: Outcome) {
        self.fence_backfills.lock().expect("lock").push(outcome);
    }
}
