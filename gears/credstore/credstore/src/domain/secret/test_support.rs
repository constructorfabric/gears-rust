//! Shared test infrastructure for domain-layer unit tests (ADR-0006).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, EqPredicate, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use credstore_sdk::{
    CredStoreError, CredStorePluginClientV2, DestroySelector, OwnerId, SecretRef, SecretValue,
    SharingMode, StoreKey, TenantId, ValueVersion,
};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_security::{AccessScope, PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::audit::{AuditEvent, AuditSink};
pub use crate::domain::ports::metrics::NoopMetrics;
use crate::domain::ports::metrics::{
    CredStoreMetricsPort, Dep, DepOp, Outcome, ReadOutcome, ReadRetryOutcome,
};
use crate::domain::ports::plugin::PluginSelector;
use crate::domain::resolver::TenantDirectory;
use crate::domain::secret::model::{NewDeclaredSecret, NewSecret, SecretRow, SecretStatus};
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

/// Permissive PDP fake: emits one `Eq` permit on the caller's
/// `subject_tenant_id` (the slot the PEP populates) — the flat, pre-expanded
/// shape a capability-less PEP receives (`AUTHZ_USAGE_SCENARIOS` S09–S11).
/// The repo fakes ignore scope contents, so this only has to compile to a
/// valid scope under `require_constraints(true)`.
struct MockAuthZResolver;

#[async_trait]
impl AuthZResolverApi for MockAuthZResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
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
                    predicates: vec![Predicate::Eq(EqPredicate::new(
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
}

/// PDP fake that always denies (`decision: false`).
struct DenyAuthZResolver;

#[async_trait]
impl AuthZResolverApi for DenyAuthZResolver {
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

/// Denying [`PolicyEnforcer`] — drives `scope_for` → `DomainError::AccessDenied`.
#[must_use]
pub fn deny_enforcer() -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(DenyAuthZResolver))
}

/// Type constraint a fake PDP attaches to a base-type decision: `In(secret_type,
/// <built-in catalog minus the denied types>)`. Returns `None` (no type
/// narrowing) when nothing is denied, and an empty list when every catalog
/// type is denied (the caller turns that into a deny).
fn allowed_type_uuids(denied_gts_ids: &[String]) -> Option<Vec<Uuid>> {
    if denied_gts_ids.is_empty() {
        return None;
    }
    Some(
        credstore_sdk::SECRET_TYPE_CATALOG
            .iter()
            .filter(|d| !denied_gts_ids.iter().any(|g| g == d.gts_id))
            .filter_map(|d| credstore_sdk::types::type_uuid(d.gts_id))
            .collect(),
    )
}

/// Decision of a type-restricting fake PDP: permissive for the tenant, with a
/// `secret_type` constraint excluding `denied_gts_ids` on the base type (only
/// when the PEP declared the property, as a real policy PDP answers a
/// base-type request with the set of types the caller's grants cover); a
/// request on a denied concrete type is denied outright.
fn type_restricted_response(
    request: &EvaluationRequest,
    denied_gts_ids: &[String],
) -> EvaluationResponse {
    let deny = || EvaluationResponse {
        decision: false,
        context: EvaluationResponseContext::default(),
    };
    if denied_gts_ids.contains(&request.resource.resource_type) {
        return deny();
    }
    let tenant = request
        .subject
        .properties
        .get("tenant_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .expect("fake PDP: subject.properties[\"tenant_id\"]; build ctx via make_ctx");
    let mut predicates = vec![Predicate::Eq(EqPredicate::new(
        pep_properties::OWNER_TENANT_ID,
        tenant,
    ))];
    let declares_type = request
        .context
        .supported_properties
        .iter()
        .any(|p| p == crate::domain::authz::SECRET_TYPE_PROP);
    if declares_type && let Some(allowed) = allowed_type_uuids(denied_gts_ids) {
        if allowed.is_empty() {
            return deny();
        }
        predicates.push(Predicate::In(InPredicate::new(
            crate::domain::authz::SECRET_TYPE_PROP,
            allowed,
        )));
    }
    EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint { predicates }],
            ..Default::default()
        },
    }
}

/// Type-aware PDP fake: permissive (like [`mock_enforcer`]) except for the
/// listed credential types, which a base-type decision excludes through a
/// `secret_type` constraint (and a concrete-type request for one is denied).
/// Records every evaluated resource type so tests can assert what the PEP
/// targeted.
pub struct TypeAwareAuthZResolver {
    denied_types: Vec<String>,
    pub seen_resource_types: Mutex<Vec<String>>,
}

#[async_trait]
impl AuthZResolverApi for TypeAwareAuthZResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.seen_resource_types
            .lock()
            .expect("lock")
            .push(request.resource.resource_type.clone());
        Ok(type_restricted_response(&request, &self.denied_types))
    }
}

/// Enforcer excluding exactly the given credential-type ids; returns the
/// resolver too so tests can inspect the evaluated types.
#[must_use]
pub fn type_deny_enforcer(
    denied_types: Vec<String>,
) -> (PolicyEnforcer, Arc<TypeAwareAuthZResolver>) {
    let resolver = Arc::new(TypeAwareAuthZResolver {
        denied_types,
        seen_resource_types: Mutex::new(Vec::new()),
    });
    (PolicyEnforcer::new(resolver.clone()), resolver)
}

/// Permissive PDP fake recording every `(action)` evaluated — for asserting
/// ADR-0004's body-derived action selection (`write`/`write_secret`).
pub struct RecordingAuthZResolver {
    seen: Mutex<Vec<String>>,
    seen_resources: Mutex<Vec<String>>,
}

#[async_trait]
impl AuthZResolverApi for RecordingAuthZResolver {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.seen
            .lock()
            .expect("lock")
            .push(request.action.name.clone());
        self.seen_resources
            .lock()
            .expect("lock")
            .push(request.resource.resource_type.clone());
        MockAuthZResolver.evaluate(ctx, request).await
    }
}

impl RecordingAuthZResolver {
    /// Every action name evaluated so far, in call order.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn seen_actions(&self) -> Vec<String> {
        self.seen.lock().expect("lock").clone()
    }

    /// Every resource type evaluated so far, in call order (parallel to
    /// [`Self::seen_actions`]).
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn seen_resource_types(&self) -> Vec<String> {
        self.seen_resources.lock().expect("lock").clone()
    }
}

/// Permissive enforcer recording every evaluated action name; returns the
/// resolver so tests can assert which of `write`/`write_secret` a body
/// actually required.
#[must_use]
pub fn type_recording_enforcer() -> (PolicyEnforcer, Arc<RecordingAuthZResolver>) {
    let resolver = Arc::new(RecordingAuthZResolver {
        seen: Mutex::new(Vec::new()),
        seen_resources: Mutex::new(Vec::new()),
    });
    (PolicyEnforcer::new(resolver.clone()), resolver)
}

/// PDP fake denying exactly one `(credential type, action)` pair; permissive
/// otherwise. Models a policy that grants everything except one action on
/// one type - e.g. `write` without `write_secret`: for that action a
/// base-type decision excludes the type through a `secret_type` constraint,
/// and a concrete-type request for it is denied outright.
pub struct ActionDenyAuthZResolver {
    resource_type: String,
    action: String,
}

#[async_trait]
impl AuthZResolverApi for ActionDenyAuthZResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let denied = if request.action.name == self.action {
            vec![self.resource_type.clone()]
        } else {
            Vec::new()
        };
        Ok(type_restricted_response(&request, &denied))
    }
}

/// Enforcer denying exactly `action` on the credential type
/// `resource_type`, permissive for every other `(type, action)` pair.
#[must_use]
pub fn action_deny_enforcer(
    resource_type: String,
    action: &str,
) -> (PolicyEnforcer, Arc<ActionDenyAuthZResolver>) {
    let resolver = Arc::new(ActionDenyAuthZResolver {
        resource_type,
        action: action.to_owned(),
    });
    (PolicyEnforcer::new(resolver.clone()), resolver)
}

/// Alternatives (OR) of `(property, values)` conjunctions (AND).
pub type RowAlternatives = Vec<Vec<(&'static str, Vec<String>)>>;

/// PDP fake answering with caller-chosen row constraints: each alternative
/// is one `Constraint` (OR-ed), each `(property, values)` pair one `In`
/// predicate (AND-ed) next to the caller's tenant `Eq`. A predicate whose
/// property the PEP did not declare is left out, as a real PDP would not
/// emit it. A `None` action list applies to every action; otherwise only the
/// listed actions are restricted and the others stay permissive.
pub struct ConstraintsAuthZResolver {
    alternatives: RowAlternatives,
    actions: Option<Vec<String>>,
}

#[async_trait]
impl AuthZResolverApi for ConstraintsAuthZResolver {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        if self
            .actions
            .as_ref()
            .is_some_and(|a| !a.contains(&request.action.name))
        {
            return MockAuthZResolver.evaluate(ctx, request).await;
        }
        let tenant = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
            .expect("ConstraintsAuthZResolver: subject.properties[\"tenant_id\"]");
        let constraints = self
            .alternatives
            .iter()
            .map(|alt| {
                let mut predicates = vec![Predicate::Eq(EqPredicate::new(
                    pep_properties::OWNER_TENANT_ID,
                    tenant,
                ))];
                for (prop, values) in alt {
                    if request
                        .context
                        .supported_properties
                        .iter()
                        .any(|p| p == prop)
                    {
                        predicates.push(Predicate::In(InPredicate::new(*prop, values.clone())));
                    }
                }
                Constraint { predicates }
            })
            .collect();
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints,
                ..Default::default()
            },
        })
    }
}

/// Enforcer answering every action in `actions` (all actions when `None`)
/// with the given row-constraint alternatives (see
/// [`ConstraintsAuthZResolver`]); other actions stay permissive.
#[must_use]
pub fn constraints_enforcer(
    alternatives: RowAlternatives,
    actions: Option<&[&str]>,
) -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(ConstraintsAuthZResolver {
        alternatives,
        actions: actions.map(|a| a.iter().map(|s| (*s).to_owned()).collect()),
    }))
}

/// Enforcer restricting `actions` (all when `None`) to rows whose
/// `reference` is one of `refs`; other actions stay permissive.
#[must_use]
pub fn reference_enforcer(refs: &[&str], actions: Option<&[&str]>) -> PolicyEnforcer {
    constraints_enforcer(
        vec![vec![(
            crate::domain::authz::REFERENCE_PROP,
            refs.iter().map(|r| (*r).to_owned()).collect(),
        )]],
        actions,
    )
}

/// Permissive PDP fake that counts evaluations: one per call, whatever the
/// action or resource. Backs the "one PDP call per action, independent of
/// the number of types" assertions.
pub struct CountingAuthZResolver {
    calls: AtomicUsize,
}

#[async_trait]
impl AuthZResolverApi for CountingAuthZResolver {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        MockAuthZResolver.evaluate(ctx, request).await
    }
}

impl CountingAuthZResolver {
    /// Number of PDP evaluations so far.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// Permissive enforcer counting every PDP evaluation.
#[must_use]
pub fn counting_enforcer() -> (PolicyEnforcer, Arc<CountingAuthZResolver>) {
    let resolver = Arc::new(CountingAuthZResolver {
        calls: AtomicUsize::new(0),
    });
    (PolicyEnforcer::new(resolver.clone()), resolver)
}

/// PDP fake that always returns a transport failure.
struct FailAuthZResolver;

#[async_trait]
impl AuthZResolverApi for FailAuthZResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Err(CanonicalError::service_unavailable()
            .with_detail("failing_enforcer: simulated PDP transport failure".to_owned())
            .create())
    }
}

/// Failing [`PolicyEnforcer`] — drives `scope_for` → `DomainError::ServiceUnavailable`.
#[must_use]
pub fn failing_enforcer() -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(FailAuthZResolver))
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

/// Key: `(tenant_id, record_id)` (ADR-0006).
type PluginKey = (Uuid, Uuid);

fn plugin_key(key: &StoreKey) -> PluginKey {
    (key.tenant_id.0, key.record_id)
}

/// Injected `get` fault for one key - see [`FakePlugin::deny_get_for`] /
/// [`FakePlugin::fail_get_for`]. Used by the secret-mode-list concurrency
/// tests, which need a *specific* winner's read (not just "the next `get`
/// call", nondeterministic once reads run concurrently) to fail.
#[derive(Clone, Copy)]
enum FakeGetFault {
    /// Backend ACL refusal (`CredStoreError::AccessDenied`) - a legitimate
    /// per-item miss (`fetch_with_retry` maps it to `Ok(None)`), not a
    /// request failure.
    Denied,
    /// A generic backend outage (`CredStoreError::ServiceUnavailable`) - a
    /// non-`NotFound` error that fails the whole request.
    Error,
    /// A permanently unreadable version (`CredStoreError::SecretUnreadable`).
    Unreadable,
}

/// All versions of one key plus its monotonic counter.
#[derive(Default)]
struct FakeKeyState {
    last: u64,
    versions: BTreeMap<u64, Vec<u8>>,
}

/// In-memory versioned plugin store keyed by `(tenant_id, record_id)`: the
/// n-th `put` under a key returns version `"n"`. By default it declares
/// `destroy` support (like the static and Vault plugins);
/// [`FakePlugin::without_destroy`] models a backend that does not (AWS
/// Secrets Manager, Azure Key Vault). Fault-injection hooks cover the
/// write-protocol tests, and every `destroy`/`delete_key` call is recorded.
pub struct FakePlugin {
    store: Mutex<HashMap<PluginKey, FakeKeyState>>,
    destroy_supported: bool,
    /// Number of upcoming `put` calls that fail before the plugin recovers,
    /// simulating a transient backend outage mid-write.
    put_failures: Mutex<usize>,
    /// Number of upcoming `put` calls that persist the bytes but then report
    /// a failure - the ambiguous outcome (the ack was lost after the store
    /// committed), which leaves an orphan version.
    put_persist_then_fail: Mutex<usize>,
    /// Number of upcoming `destroy` calls that fail.
    destroy_failures: Mutex<usize>,
    /// Number of upcoming `delete_key` calls that fail.
    delete_key_failures: Mutex<usize>,
    /// Number of upcoming `get` calls that report `Ok(None)` regardless of
    /// the store's actual contents - models a read racing a concurrent
    /// pointer switch (ADR-0006's re-read-once case).
    not_found_gets: Mutex<usize>,
    /// When set, every `get` returns [`CredStoreError::AccessDenied`],
    /// modelling a backend whose own ACLs reject a read the gear's PDP has
    /// already allowed.
    get_denied: bool,
    /// Per-key injected `get` faults (secret-mode-list concurrency tests).
    get_faults: Mutex<HashMap<PluginKey, FakeGetFault>>,
    /// Per-key injected read latency in milliseconds (secret-mode-list
    /// concurrency tests): `get` sleeps this long for a key present here,
    /// tracked by `in_flight`/`max_in_flight` so a test can observe how many
    /// reads were in flight at once.
    delays: Mutex<HashMap<PluginKey, u64>>,
    /// Number of delayed `get` calls currently sleeping.
    in_flight: AtomicUsize,
    /// The largest `in_flight` value observed - see [`Self::max_in_flight`].
    max_in_flight: AtomicUsize,
    /// Number of `get` calls made (any key).
    get_calls: AtomicUsize,
    /// Every `destroy` call received, in order.
    destroy_log: Mutex<Vec<(StoreKey, DestroySelector)>>,
    /// Every `delete_key` call received, in order.
    delete_key_log: Mutex<Vec<StoreKey>>,
}

impl FakePlugin {
    fn build(destroy_supported: bool, get_denied: bool) -> Self {
        Self {
            store: Mutex::new(HashMap::new()),
            destroy_supported,
            put_failures: Mutex::new(0),
            put_persist_then_fail: Mutex::new(0),
            destroy_failures: Mutex::new(0),
            delete_key_failures: Mutex::new(0),
            not_found_gets: Mutex::new(0),
            get_denied,
            get_faults: Mutex::new(HashMap::new()),
            delays: Mutex::new(HashMap::new()),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            get_calls: AtomicUsize::new(0),
            destroy_log: Mutex::new(Vec::new()),
            delete_key_log: Mutex::new(Vec::new()),
        }
    }

    /// A plugin that supports `destroy` (the default).
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::build(true, false))
    }

    /// A plugin that does not declare `destroy`: `supports_destroy()` is
    /// `false`, and any `destroy` call it still receives is recorded as a
    /// contract violation (see [`Self::destroy_calls`]).
    #[must_use]
    pub fn without_destroy() -> Arc<Self> {
        Arc::new(Self::build(false, false))
    }

    /// Plugin that fails the next `n` `put` calls, then behaves normally.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn with_put_failures(n: usize) -> Arc<Self> {
        let p = Self::new();
        *p.put_failures.lock().expect("lock") = n;
        p
    }

    /// Plugin whose every `get` denies - models a backend ACL rejecting a read
    /// the gear's PDP already allowed.
    #[must_use]
    pub fn with_get_denied() -> Arc<Self> {
        Arc::new(Self::build(true, true))
    }

    /// Arrange for the next `n` `get` calls to report `Ok(None)` regardless
    /// of what is actually stored.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn fail_next_gets_with_not_found(&self, n: usize) {
        *self.not_found_gets.lock().expect("lock") += n;
    }

    /// Arrange for the next `n` `put` calls to fail (nothing is stored).
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn fail_next_puts(&self, n: usize) {
        *self.put_failures.lock().expect("lock") += n;
    }

    /// Arrange for the next `n` `put` calls to store the bytes and then
    /// report a failure (an ambiguous outcome that leaves an orphan version).
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn fail_next_puts_after_persisting(&self, n: usize) {
        *self.put_persist_then_fail.lock().expect("lock") += n;
    }

    /// Arrange for the next `n` `destroy` calls to fail.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn fail_next_destroys(&self, n: usize) {
        *self.destroy_failures.lock().expect("lock") += n;
    }

    /// Arrange for the next `n` `delete_key` calls to fail.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn fail_next_delete_keys(&self, n: usize) {
        *self.delete_key_failures.lock().expect("lock") += n;
    }

    /// True when the store holds `version` under `key`.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn contains(&self, key: &StoreKey, version: &ValueVersion) -> bool {
        let Ok(n) = version.as_str().parse::<u64>() else {
            return false;
        };
        self.store
            .lock()
            .expect("lock")
            .get(&plugin_key(key))
            .is_some_and(|k| k.versions.contains_key(&n))
    }

    /// The versions currently held under `key`, oldest first.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn versions(&self, key: &StoreKey) -> Vec<String> {
        self.store
            .lock()
            .expect("lock")
            .get(&plugin_key(key))
            .map(|k| k.versions.keys().map(ToString::to_string).collect())
            .unwrap_or_default()
    }

    /// True when the store holds anything at all under `key`.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn holds_key(&self, key: &StoreKey) -> bool {
        self.store
            .lock()
            .expect("lock")
            .get(&plugin_key(key))
            .is_some_and(|k| !k.versions.is_empty())
    }

    /// Store `bytes` under `key` directly (bypassing fault injection and
    /// call logs), returning the version - for seeding a state the gear's own
    /// write path would not produce.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn seed(&self, key: &StoreKey, bytes: &[u8]) -> ValueVersion {
        let mut store = self.store.lock().expect("lock");
        let state = store.entry(plugin_key(key)).or_default();
        state.last += 1;
        state.versions.insert(state.last, bytes.to_vec());
        ValueVersion::new(state.last.to_string())
    }

    /// Remove one version out of band (as a concurrent writer's `destroy`
    /// would), without logging a call.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn drop_version(&self, key: &StoreKey, version: &ValueVersion) {
        if let Ok(n) = version.as_str().parse::<u64>()
            && let Some(state) = self.store.lock().expect("lock").get_mut(&plugin_key(key))
        {
            state.versions.remove(&n);
        }
    }

    /// Every `destroy` call received, in order.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn destroy_calls(&self) -> Vec<(StoreKey, DestroySelector)> {
        self.destroy_log.lock().expect("lock").clone()
    }

    /// Every `delete_key` call received, in order.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn delete_key_calls(&self) -> Vec<StoreKey> {
        self.delete_key_log.lock().expect("lock").clone()
    }

    /// Number of `get` calls received so far.
    #[must_use]
    pub fn get_calls(&self) -> usize {
        self.get_calls.load(Ordering::SeqCst)
    }

    /// Always fail `get` for `key` with `CredStoreError::AccessDenied` - a
    /// backend ACL refusing a read the gear's PDP already allowed, which
    /// `fetch_with_retry` treats as a legitimate per-item miss (`Ok(None)`),
    /// not a request failure. For the secret-mode-list "refused item omitted"
    /// test: unlike [`Self::fail_next_gets_with_not_found`] (a global counter
    /// over the *next* call, nondeterministic once reads run concurrently),
    /// this targets one specific winner.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn deny_get_for(&self, key: &StoreKey) {
        self.get_faults
            .lock()
            .expect("lock")
            .insert(plugin_key(key), FakeGetFault::Denied);
    }

    /// Always fail `get` for `key` with a simulated backend outage
    /// (`CredStoreError::ServiceUnavailable`) - a non-`NotFound` error that
    /// fails the whole request, targeted at one specific winner (see
    /// [`Self::deny_get_for`]).
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn fail_get_for(&self, key: &StoreKey) {
        self.get_faults
            .lock()
            .expect("lock")
            .insert(plugin_key(key), FakeGetFault::Error);
    }

    /// Always fail `get` for `key` with the permanent
    /// `CredStoreError::SecretUnreadable` (a lost key or corrupt entry),
    /// targeted at one specific winner (see [`Self::deny_get_for`]).
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn unreadable_get_for(&self, key: &StoreKey) {
        self.get_faults
            .lock()
            .expect("lock")
            .insert(plugin_key(key), FakeGetFault::Unreadable);
    }

    /// Make every future `get` for `key` sleep `ms` milliseconds, with the
    /// sleep tracked by [`Self::max_in_flight`] - for asserting the
    /// secret-mode list's bounded-concurrency fan-out.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn set_delay_ms(&self, key: &StoreKey, ms: u64) {
        self.delays
            .lock()
            .expect("lock")
            .insert(plugin_key(key), ms);
    }

    /// The largest number of [`Self::set_delay_ms`]-delayed `get` calls this
    /// plugin ever had sleeping at the same time.
    #[must_use]
    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }

    fn take_one(counter: &Mutex<usize>) -> bool {
        let mut remaining = counter.lock().expect("lock");
        if *remaining > 0 {
            *remaining -= 1;
            true
        } else {
            false
        }
    }
}

impl Default for FakePlugin {
    fn default() -> Self {
        Self::build(true, false)
    }
}

#[async_trait]
impl CredStorePluginClientV2 for FakePlugin {
    async fn put(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
        value: SecretValue,
    ) -> Result<ValueVersion, CredStoreError> {
        if Self::take_one(&self.put_failures) {
            return Err(CredStoreError::Internal(
                "simulated backend put failure".to_owned(),
            ));
        }
        let version = {
            let mut store = self.store.lock().expect("lock");
            let state = store.entry(plugin_key(key)).or_default();
            state.last += 1;
            state.versions.insert(state.last, value.as_bytes().to_vec());
            ValueVersion::new(state.last.to_string())
        };
        if Self::take_one(&self.put_persist_then_fail) {
            return Err(CredStoreError::ServiceUnavailable {
                detail: "simulated lost put ack".to_owned(),
                retry_after: None,
            });
        }
        Ok(version)
    }

    async fn get(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
        version: &ValueVersion,
    ) -> Result<Option<SecretValue>, CredStoreError> {
        self.get_calls.fetch_add(1, Ordering::SeqCst);
        if self.get_denied {
            return Err(CredStoreError::AccessDenied);
        }
        let k = plugin_key(key);
        if let Some(fault) = self.get_faults.lock().expect("lock").get(&k).copied() {
            return match fault {
                FakeGetFault::Denied => Err(CredStoreError::AccessDenied),
                FakeGetFault::Error => Err(CredStoreError::ServiceUnavailable {
                    detail: "simulated backend get failure".to_owned(),
                    retry_after: None,
                }),
                FakeGetFault::Unreadable => Err(CredStoreError::SecretUnreadable),
            };
        }
        if Self::take_one(&self.not_found_gets) {
            return Ok(None);
        }
        let delay_ms = self
            .delays
            .lock()
            .expect("lock")
            .get(&k)
            .copied()
            .unwrap_or(0);
        if delay_ms > 0 {
            let in_flight = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(in_flight, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
        }
        let Ok(n) = version.as_str().parse::<u64>() else {
            return Ok(None);
        };
        let guard = self.store.lock().expect("lock");
        Ok(guard
            .get(&k)
            .and_then(|state| state.versions.get(&n))
            .map(|v| SecretValue::new(v.clone())))
    }

    async fn delete_key(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
    ) -> Result<(), CredStoreError> {
        self.delete_key_log.lock().expect("lock").push(key.clone());
        if Self::take_one(&self.delete_key_failures) {
            return Err(CredStoreError::Internal(
                "simulated backend delete_key failure".to_owned(),
            ));
        }
        self.store.lock().expect("lock").remove(&plugin_key(key));
        Ok(())
    }

    fn supports_destroy(&self) -> bool {
        self.destroy_supported
    }

    async fn destroy(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
        selector: DestroySelector,
    ) -> Result<(), CredStoreError> {
        self.destroy_log
            .lock()
            .expect("lock")
            .push((key.clone(), selector.clone()));
        if !self.destroy_supported {
            return Err(CredStoreError::Internal(
                "destroy called on a plugin that does not support it".to_owned(),
            ));
        }
        if Self::take_one(&self.destroy_failures) {
            return Err(CredStoreError::ServiceUnavailable {
                detail: "simulated backend destroy failure".to_owned(),
                retry_after: None,
            });
        }
        let mut store = self.store.lock().expect("lock");
        let Some(state) = store.get_mut(&plugin_key(key)) else {
            return Ok(());
        };
        match selector {
            DestroySelector::Below(v) => {
                if let Ok(n) = v.as_str().parse::<u64>() {
                    state.versions = state.versions.split_off(&n);
                }
            }
            DestroySelector::Exactly(v) => {
                if let Ok(n) = v.as_str().parse::<u64>() {
                    state.versions.remove(&n);
                }
            }
        }
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
    async fn resolve(&self) -> Result<Arc<dyn CredStorePluginClientV2>, DomainError> {
        Ok(self.plugin.clone())
    }
}

/// [`PluginSelector`] that always fails to resolve a plugin — models the
/// `NoPluginAvailable` (misconfigured / unregistered backend) 503 path.
pub struct NoPluginSelector;

#[async_trait]
impl PluginSelector for NoPluginSelector {
    async fn resolve(&self) -> Result<Arc<dyn CredStorePluginClientV2>, DomainError> {
        Err(DomainError::ServiceUnavailable {
            detail: "no storage plugin registered".to_owned(),
            retry_after: None,
            cause: None,
        })
    }
}

// ── FakeSecretRepo ────────────────────────────────────────────────────────────

/// `(row_id, new_value_version)` - see `pending_switch`'s field docs.
type PendingSwitch = (Uuid, ValueVersion);

/// In-memory [`SecretRepo`] replicating the real (transactional) semantics of
/// each method, for domain-service unit tests.
///
/// `scope_allows` controls the result of [`SecretRepo::scope_includes_tenant`].
pub struct FakeSecretRepo {
    rows: Mutex<Vec<SecretRow>>,
    /// Every key purge a delete enqueued in the platform outbox, in order.
    purged: Mutex<Vec<StoreKey>>,
    pub scope_allows: bool,
    /// When `> 0`, the next `insert_active` call fails with a simulated
    /// internal error (before touching rows) and decrements; consumed
    /// once per call.
    insert_active_failures: Mutex<usize>,
    /// When `> 0`, the next `insert_active` call reports `Conflict` (a unique
    /// violation: a concurrent create won) without touching rows - a definite
    /// loss, distinct from the ambiguous failure above.
    insert_active_conflicts: Mutex<usize>,
    /// When `> 0`, the next `switch_value` call fails with a simulated
    /// internal error (before touching rows) rather than returning
    /// `Ok(None)`/`Ok(Some(_))` — models an ambiguous DB failure (the commit
    /// may or may not have happened), distinct from an ordinary lost CAS.
    switch_value_failures: Mutex<usize>,
    /// When `> 0`, the next `switch_value` call returns `Ok(None)` (a lost
    /// CAS) without touching rows, regardless of the actual id/version —
    /// models "another writer's CAS committed first", which a purely
    /// sequential test cannot otherwise reproduce.
    force_switch_value_none: Mutex<usize>,
    /// One-shot hook for the read-races-a-switch scenario: after the *next*
    /// `resolve_for_get` call whose result matches `row_id` returns (with the
    /// pre-switch snapshot), atomically flips the stored row to
    /// `new_value_version` (bumping its version) — so a second,
    /// subsequently-issued `resolve_for_get` observes the post-switch row,
    /// exactly like a concurrent writer's pointer switch landing between two
    /// reads, without needing real concurrency.
    pending_switch: Mutex<Option<PendingSwitch>>,
    /// One-shot: after the next `resolve_for_get` that resolves to this row
    /// id returns, the stored row is removed — a concurrent delete landing
    /// before the read's re-read.
    pending_vanish: Mutex<Option<Uuid>>,
    /// When set, `delete_by_id` returns an error — simulating a DB failure.
    fail_delete: bool,
    /// When `> 0`, the next `delete_by_id` call reports `NotFound`
    /// regardless of actual row state — models a row vanishing concurrently
    /// between the caller's precheck and this call.
    force_delete_by_id_not_found: Mutex<usize>,
    /// Every `type_uuid_in` clamp `list_candidate_references` (step 1) was
    /// called with, in call order (`None` when that call carried no type
    /// clamp) — lets tests assert the PDP-permitted set
    /// `Service::permitted_types` computed actually reached step 1's SQL
    /// clamp, and that step 1 was skipped entirely (an empty vec here) when
    /// nothing was permitted.
    list_candidate_references_calls: Mutex<Vec<Option<Vec<Uuid>>>>,
}

impl FakeSecretRepo {
    #[must_use]
    pub fn new() -> Self {
        Self {
            rows: Mutex::new(Vec::new()),
            purged: Mutex::new(Vec::new()),
            scope_allows: true,
            insert_active_failures: Mutex::new(0),
            insert_active_conflicts: Mutex::new(0),
            switch_value_failures: Mutex::new(0),
            force_switch_value_none: Mutex::new(0),
            pending_switch: Mutex::new(None),
            pending_vanish: Mutex::new(None),
            fail_delete: false,
            force_delete_by_id_not_found: Mutex::new(0),
            list_candidate_references_calls: Mutex::new(Vec::new()),
        }
    }

    #[must_use]
    pub fn with_scope_allows(scope_allows: bool) -> Self {
        Self {
            scope_allows,
            ..Self::new()
        }
    }

    /// Repo whose `delete_by_id` always fails — exercises the delete-failure
    /// path.
    #[must_use]
    pub fn with_delete_failure() -> Self {
        Self {
            fail_delete: true,
            ..Self::new()
        }
    }

    /// Arrange for the next `n` `insert_active` calls to fail with a
    /// simulated internal (DB-unreachable-like) error.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn fail_next_insert_active(&self, n: usize) {
        *self.insert_active_failures.lock().expect("lock") += n;
    }

    /// Arrange for the next `n` `insert_active` calls to report `Conflict`
    /// (a concurrent create won the unique index) without inserting.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn conflict_next_insert_active(&self, n: usize) {
        *self.insert_active_conflicts.lock().expect("lock") += n;
    }

    /// Arrange for the next `n` `switch_value` calls to fail with a
    /// simulated internal (DB-unreachable-like) error, instead of running
    /// the ordinary CAS.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn fail_next_switch_value(&self, n: usize) {
        *self.switch_value_failures.lock().expect("lock") += n;
    }

    /// Arrange for the next `n` `switch_value` calls to report a lost CAS
    /// (`Ok(None)`) without touching rows — models a concurrent writer
    /// having already moved the row by the time this call's CAS ran.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn force_next_switch_value_none(&self, n: usize) {
        *self.force_switch_value_none.lock().expect("lock") += n;
    }

    /// One-shot: once a `resolve_for_get` call resolves to `row_id`, flip
    /// that stored row to `new_value_version` (row version bumped) right
    /// after computing *that* call's (pre-switch) result — so the next
    /// `resolve_for_get` call sees the post-switch row. See the field docs
    /// on `pending_switch`.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn switch_after_next_resolve(&self, row_id: Uuid, new_value_version: ValueVersion) {
        *self.pending_switch.lock().expect("lock") = Some((row_id, new_value_version));
    }

    /// One-shot: once a `resolve_for_get` call resolves to `row_id`, delete
    /// that stored row right after computing that call's result, so the next
    /// `resolve_for_get` finds the reference gone.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn vanish_after_next_resolve(&self, row_id: Uuid) {
        *self.pending_vanish.lock().expect("lock") = Some(row_id);
    }

    /// Arrange for the next `n` `delete_by_id` calls to report `NotFound`
    /// regardless of actual row state — models a row vanishing concurrently
    /// between the caller's precheck and the delete transaction.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn force_next_delete_by_id_not_found(&self, n: usize) {
        *self.force_delete_by_id_not_found.lock().expect("lock") += n;
    }

    /// Force `row_id`'s `expires_at` into the past, for tests that need an
    /// already-expired row without waiting.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned or no row matches `row_id`.
    pub fn force_expire(&self, row_id: Uuid) {
        let mut rows = self.rows.lock().expect("lock");
        let row = rows
            .iter_mut()
            .find(|r| r.id == row_id)
            .expect("row_id must exist");
        row.expires_at = Some(OffsetDateTime::now_utc() - time::Duration::seconds(5));
    }

    /// Seed rows directly (for pre-seeding parent/inherited state).
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn seed(&self, row: SecretRow) {
        self.rows.lock().expect("lock").push(row);
    }

    /// Snapshot of all rows (for asserting write-protocol state).
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn rows(&self) -> Vec<SecretRow> {
        self.rows.lock().expect("lock").clone()
    }

    /// Every key purge enqueued so far (a delete), in order.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn purged_keys(&self) -> Vec<StoreKey> {
        self.purged.lock().expect("lock").clone()
    }

    /// Every `type_uuid_in` clamp `list_candidate_references` (step 1) was
    /// called with, in call order — an empty vec means step 1 was never
    /// called at all (e.g. the PDP-permitted set was empty and the service
    /// returned before running it).
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn list_candidate_references_type_clamps(&self) -> Vec<Option<Vec<Uuid>>> {
        self.list_candidate_references_calls
            .lock()
            .expect("lock")
            .clone()
    }

    /// The type UUIDs a row scope admits: per constraint the intersection of
    /// its `secret_type` filters, unioned across constraints (a constraint
    /// without a type filter contributes nothing).
    fn scope_type_uuids(scope: &AccessScope) -> Vec<Uuid> {
        let mut out: Vec<Uuid> = Vec::new();
        for c in scope.constraints() {
            let mut acc: Option<Vec<Uuid>> = None;
            for f in c
                .filters()
                .iter()
                .filter(|f| f.property() == crate::domain::authz::SECRET_TYPE_PROP)
            {
                let vals = f.uuid_values();
                acc = Some(match acc {
                    None => vals,
                    Some(prev) => prev.into_iter().filter(|u| vals.contains(u)).collect(),
                });
            }
            for u in acc.unwrap_or_default() {
                if !out.contains(&u) {
                    out.push(u);
                }
            }
        }
        out
    }

    /// Whether `scope` admits `row` - the in-memory twin of the secure ORM's
    /// SQL compilation for the properties credstore maps (tenant, credential
    /// type and reference). A constraint admits a row when every filter does;
    /// constraints are OR-ed; an unknown property admits nothing.
    fn scope_admits_row(scope: &AccessScope, row: &SecretRow) -> bool {
        if scope.is_unconstrained() {
            return true;
        }
        scope.constraints().iter().any(|c| {
            c.filters().iter().all(|f| {
                if f.property() == crate::domain::authz::REFERENCE_PROP {
                    return f.values().iter().any(
                        |v| matches!(v, toolkit_security::ScopeValue::String(s) if *s == row.reference),
                    );
                }
                let wanted = match f.property() {
                    p if p == pep_properties::OWNER_TENANT_ID => row.tenant_id.0,
                    p if p == crate::domain::authz::SECRET_TYPE_PROP => row.secret_type_uuid,
                    _ => return false,
                };
                f.values().iter().any(|v| v.as_uuid() == Some(wanted))
            })
        })
    }

    /// Resolution-eligible (ADR-0004, Suppression): `active` (expired or not —
    /// expiry applies to the secret, not to the record), or `declared` with `fallback: none` (a suppressing row that competes
    /// and blocks). Mirrors the production predicate in
    /// `infra::storage::repo_impl::reads::resolution_eligible_condition`.
    fn resolution_eligible(r: &SecretRow) -> bool {
        match r.status {
            SecretStatus::Active => true,
            SecretStatus::Declared => r.fallback == crate::domain::secret::model::Fallback::None,
        }
    }

    /// Collection-read visibility predicate (ADR-0005): own tenant, every
    /// sharing-visible row of any status; ancestor tenants, resolution-
    /// eligible `shared` rows only. Mirrors
    /// `resolve_candidates`'s per-row predicate above (minus the reference
    /// match) and
    /// `infra::storage::repo_impl::reads::chain_visibility_condition`.
    fn is_candidate_visible(
        r: &SecretRow,
        req_tenant: TenantId,
        subject: OwnerId,
        chain: &[Uuid],
    ) -> bool {
        chain.contains(&r.tenant_id.0)
            && if r.tenant_id == req_tenant {
                match r.sharing {
                    SharingMode::Private => r.owner_id.0 == subject.0,
                    SharingMode::Tenant | SharingMode::Shared => true,
                }
            } else {
                r.sharing == SharingMode::Shared && Self::resolution_eligible(r)
            }
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

        let pos = |t: Uuid| chain.iter().position(|c| *c == t).unwrap_or(usize::MAX);
        let best = rows
            .iter()
            .filter(|r| {
                Self::resolution_eligible(r)
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
        let result = best.cloned();
        drop(rows);

        // Apply a one-shot pending switch (read-races-a-switch test support):
        // this call's result stays the pre-switch snapshot, but the stored
        // row is mutated now, so the *next* resolve_for_get sees the switch.
        if let Some(r) = &result {
            let mut pending = self.pending_switch.lock().expect("lock");
            if pending.as_ref().is_some_and(|(row_id, ..)| *row_id == r.id) {
                let (_, new_value_version) = pending.take().expect("checked Some above");
                drop(pending);
                let mut rows = self.rows.lock().expect("lock");
                if let Some(stored) = rows.iter_mut().find(|x| x.id == r.id) {
                    stored.value_version = Some(new_value_version);
                    stored.version += 1;
                }
            }
        }
        if let Some(r) = &result {
            let mut pending = self.pending_vanish.lock().expect("lock");
            if *pending == Some(r.id) {
                pending.take();
                drop(pending);
                self.rows.lock().expect("lock").retain(|x| x.id != r.id);
            }
        }
        Ok(result)
    }

    async fn resolve_candidates(
        &self,
        req_tenant: TenantId,
        subject: OwnerId,
        key: &SecretRef,
        chain: &[Uuid],
    ) -> Result<Vec<SecretRow>, DomainError> {
        let rows = self.rows.lock().expect("lock");
        let key_str = key.as_ref();
        let candidates = rows
            .iter()
            .filter(|r| {
                r.reference == key_str
                    && chain.contains(&r.tenant_id.0)
                    && if r.tenant_id == req_tenant {
                        match r.sharing {
                            SharingMode::Private => r.owner_id.0 == subject.0,
                            SharingMode::Tenant | SharingMode::Shared => true,
                        }
                    } else {
                        r.sharing == SharingMode::Shared && Self::resolution_eligible(r)
                    }
            })
            .cloned()
            .collect();
        Ok(candidates)
    }

    async fn find_own(
        &self,
        scope: &AccessScope,
        tenant: TenantId,
        subject: OwnerId,
        key: &SecretRef,
    ) -> Result<Option<SecretRow>, DomainError> {
        let rows = self.rows.lock().expect("lock");
        let key_str = key.as_ref();
        let best = rows
            .iter()
            .filter(|r| {
                r.tenant_id == tenant
                    && r.reference == key_str
                    && matches!(r.status, SecretStatus::Active | SecretStatus::Declared)
                    && Self::scope_admits_row(scope, r)
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
        scope: &AccessScope,
        tenant: TenantId,
        subject: OwnerId,
        key: &SecretRef,
        sharing: SharingMode,
    ) -> Result<Option<SecretRow>, DomainError> {
        let rows = self.rows.lock().expect("lock");
        let key_str = key.as_ref();
        let row = rows.iter().find(|r| {
            r.tenant_id == tenant
                && r.reference == key_str
                && matches!(r.status, SecretStatus::Active | SecretStatus::Declared)
                && Self::scope_admits_row(scope, r)
                && match sharing {
                    SharingMode::Private => {
                        r.sharing == SharingMode::Private && r.owner_id == subject
                    }
                    _ => r.sharing != SharingMode::Private,
                }
        });
        Ok(row.cloned())
    }

    async fn scope_includes_tenant(
        &self,
        _scope: &AccessScope,
        _tenant: Uuid,
    ) -> Result<bool, DomainError> {
        Ok(self.scope_allows)
    }

    async fn list_candidate_references(
        &self,
        req_tenant: TenantId,
        subject: OwnerId,
        chain: &[Uuid],
        reference_in: Option<&[String]>,
        type_scope: &AccessScope,
        cursor: Option<&str>,
        desc: bool,
        limit: u64,
    ) -> Result<Vec<String>, DomainError> {
        self.list_candidate_references_calls
            .lock()
            .expect("lock")
            .push((!type_scope.is_unconstrained()).then(|| Self::scope_type_uuids(type_scope)));
        let rows = self.rows.lock().expect("lock");
        let mut refs: Vec<String> = rows
            .iter()
            .filter(|r| Self::is_candidate_visible(r, req_tenant, subject, chain))
            .filter(|r| reference_in.is_none_or(|refs| refs.contains(&r.reference)))
            .filter(|r| Self::scope_admits_row(type_scope, r))
            .map(|r| r.reference.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if desc {
            refs.reverse();
        }
        if let Some(after) = cursor {
            refs.retain(|r| {
                if desc {
                    r.as_str() < after
                } else {
                    r.as_str() > after
                }
            });
        }
        refs.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(refs)
    }

    async fn list_candidates_for_references(
        &self,
        req_tenant: TenantId,
        subject: OwnerId,
        chain: &[Uuid],
        references: &[String],
    ) -> Result<Vec<SecretRow>, DomainError> {
        let rows = self.rows.lock().expect("lock");
        Ok(rows
            .iter()
            .filter(|r| references.contains(&r.reference))
            .filter(|r| Self::is_candidate_visible(r, req_tenant, subject, chain))
            .cloned()
            .collect())
    }

    async fn insert_active(
        &self,
        _scope: &AccessScope,
        new: &NewSecret,
    ) -> Result<(), DomainError> {
        {
            let mut remaining = self.insert_active_failures.lock().expect("lock");
            if *remaining > 0 {
                *remaining -= 1;
                return Err(DomainError::internal("simulated insert_active failure"));
            }
        }
        {
            let mut remaining = self.insert_active_conflicts.lock().expect("lock");
            if *remaining > 0 {
                *remaining -= 1;
                return Err(DomainError::Conflict);
            }
        }
        let mut rows = self.rows.lock().expect("lock");
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
            return Err(DomainError::Conflict);
        }
        rows.push(SecretRow {
            id: new.id,
            tenant_id: new.tenant_id,
            reference: new.reference.as_ref().to_owned(),
            sharing: new.sharing,
            owner_id: new.owner_id,
            status: SecretStatus::Active,
            version: 1,
            updated_at: OffsetDateTime::now_utc(),
            secret_type_uuid: new.secret_type_uuid,
            expires_at: new.expires_at,
            value_version: Some(new.value_version.clone()),
            fallback: new.fallback,
        });
        Ok(())
    }

    async fn insert_declared(
        &self,
        _scope: &AccessScope,
        new: &NewDeclaredSecret,
    ) -> Result<(), DomainError> {
        let mut rows = self.rows.lock().expect("lock");
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
            return Err(DomainError::Conflict);
        }
        rows.push(SecretRow {
            id: new.id,
            tenant_id: new.tenant_id,
            reference: new.reference.as_ref().to_owned(),
            sharing: new.sharing,
            owner_id: new.owner_id,
            status: SecretStatus::Declared,
            version: 1,
            updated_at: OffsetDateTime::now_utc(),
            secret_type_uuid: new.secret_type_uuid,
            expires_at: new.expires_at,
            value_version: None,
            fallback: new.fallback,
        });
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors SecretRepo::switch_value's one-CAS-with-every-field-it-may-update shape"
    )]
    async fn switch_value(
        &self,
        _scope: &AccessScope,
        id: Uuid,
        expected_version: i64,
        sharing: SharingMode,
        fallback: crate::domain::secret::model::Fallback,
        expires_at: Option<OffsetDateTime>,
        new_value_version: ValueVersion,
    ) -> Result<Option<SecretRow>, DomainError> {
        {
            let mut remaining = self.switch_value_failures.lock().expect("lock");
            if *remaining > 0 {
                *remaining -= 1;
                return Err(DomainError::internal("simulated switch_value failure"));
            }
        }
        {
            let mut remaining = self.force_switch_value_none.lock().expect("lock");
            if *remaining > 0 {
                *remaining -= 1;
                return Ok(None);
            }
        }
        let mut rows = self.rows.lock().expect("lock");
        let row = rows.iter_mut().find(|r| {
            r.id == id
                && matches!(r.status, SecretStatus::Active | SecretStatus::Declared)
                && r.version == expected_version
        });
        let Some(row) = row else {
            return Ok(None);
        };
        row.value_version = Some(new_value_version);
        row.sharing = sharing;
        row.fallback = fallback;
        row.expires_at = expires_at;
        row.status = SecretStatus::Active;
        row.version += 1;
        row.updated_at = OffsetDateTime::now_utc();
        Ok(Some(row.clone()))
    }

    async fn update_metadata(
        &self,
        _scope: &AccessScope,
        id: Uuid,
        expected_version: Option<i64>,
        sharing: SharingMode,
        fallback: crate::domain::secret::model::Fallback,
        expires_at: Option<OffsetDateTime>,
    ) -> Result<Option<SecretRow>, DomainError> {
        let mut rows = self.rows.lock().expect("lock");
        let row = rows
            .iter_mut()
            .find(|r| r.id == id && expected_version.is_none_or(|v| r.version == v));
        let Some(row) = row else {
            return Ok(None);
        };
        row.sharing = sharing;
        row.fallback = fallback;
        row.expires_at = expires_at;
        row.version += 1;
        row.updated_at = OffsetDateTime::now_utc();
        Ok(Some(row.clone()))
    }

    async fn remove_value(
        &self,
        _scope: &AccessScope,
        id: Uuid,
        expected_version: Option<i64>,
        sharing: SharingMode,
        fallback: crate::domain::secret::model::Fallback,
        expires_at: Option<OffsetDateTime>,
    ) -> Result<Option<(SecretRow, Option<ValueVersion>)>, DomainError> {
        let mut rows = self.rows.lock().expect("lock");
        let row = rows
            .iter_mut()
            .find(|r| r.id == id && expected_version.is_none_or(|v| r.version == v));
        let Some(row) = row else {
            return Ok(None);
        };
        let old_value_version = row.value_version.take();
        row.status = SecretStatus::Declared;
        row.sharing = sharing;
        row.fallback = fallback;
        row.expires_at = expires_at;
        row.version += 1;
        row.updated_at = OffsetDateTime::now_utc();
        Ok(Some((row.clone(), old_value_version)))
    }

    async fn delete_by_id(
        &self,
        _scope: &AccessScope,
        key: &StoreKey,
        expected_version: Option<i64>,
    ) -> Result<(), DomainError> {
        if self.fail_delete {
            return Err(DomainError::internal("simulated delete failure"));
        }
        {
            let mut remaining = self.force_delete_by_id_not_found.lock().expect("lock");
            if *remaining > 0 {
                *remaining -= 1;
                return Err(DomainError::NotFound);
            }
        }
        let mut rows = self.rows.lock().expect("lock");
        let idx = rows
            .iter()
            .position(|r| r.id == key.record_id && expected_version.is_none_or(|v| r.version == v));
        let Some(idx) = idx else {
            return Err(DomainError::NotFound);
        };
        rows.remove(idx);
        drop(rows);
        // The row delete and the purge enqueue are one transaction.
        self.purged.lock().expect("lock").push(key.clone());
        Ok(())
    }
}

// ── FakeMetrics ───────────────────────────────────────────────────────────────

/// Recording metrics fake for assertions in tests.
pub struct FakeMetrics {
    pub cross_tenant_denied_count: Mutex<u64>,
    pub read_outcomes: Mutex<Vec<ReadOutcome>>,
    pub deps: Mutex<Vec<(Dep, DepOp, Outcome)>>,
    pub destroy_failed_total: Mutex<u64>,
    pub outbox_purge_failed_total: Mutex<u64>,
    pub read_retries: Mutex<Vec<ReadRetryOutcome>>,
    pub list_type_invariant_violation_total: Mutex<u64>,
    pub audit_publish_failed_total: Mutex<u64>,
    pub secret_unreadable_total: Mutex<u64>,
}

impl FakeMetrics {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
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

    /// Number of failed best-effort `destroy` calls recorded.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn destroy_failed_total(&self) -> u64 {
        *self.destroy_failed_total.lock().expect("lock")
    }

    /// Number of failed outbox purge deliveries recorded.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn outbox_purge_failed_total(&self) -> u64 {
        *self.outbox_purge_failed_total.lock().expect("lock")
    }

    /// Every recorded read-retry outcome, in order.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn read_retries(&self) -> Vec<ReadRetryOutcome> {
        self.read_retries.lock().expect("lock").clone()
    }

    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn list_type_invariant_violation_total(&self) -> u64 {
        *self
            .list_type_invariant_violation_total
            .lock()
            .expect("lock")
    }
}

impl FakeMetrics {
    /// Number of failed audit publications recorded.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn audit_publish_failed_total(&self) -> u64 {
        *self.audit_publish_failed_total.lock().expect("lock")
    }

    /// Number of permanently unreadable secret reads recorded.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn secret_unreadable_total(&self) -> u64 {
        *self.secret_unreadable_total.lock().expect("lock")
    }
}

impl Default for FakeMetrics {
    fn default() -> Self {
        Self {
            cross_tenant_denied_count: Mutex::new(0),
            read_outcomes: Mutex::new(Vec::new()),
            deps: Mutex::new(Vec::new()),
            destroy_failed_total: Mutex::new(0),
            outbox_purge_failed_total: Mutex::new(0),
            read_retries: Mutex::new(Vec::new()),
            list_type_invariant_violation_total: Mutex::new(0),
            audit_publish_failed_total: Mutex::new(0),
            secret_unreadable_total: Mutex::new(0),
        }
    }
}

impl CredStoreMetricsPort for FakeMetrics {
    fn read_outcome(&self, outcome: ReadOutcome) {
        self.read_outcomes.lock().expect("lock").push(outcome);
    }
    fn walkup_depth(&self, _depth: u64) {}
    fn dependency(&self, dep: Dep, op: DepOp, outcome: Outcome, _secs: f64) {
        self.deps.lock().expect("lock").push((dep, op, outcome));
    }
    fn cross_tenant_denied(&self) {
        *self.cross_tenant_denied_count.lock().expect("lock") += 1;
    }
    fn destroy_failed(&self) {
        *self.destroy_failed_total.lock().expect("lock") += 1;
    }
    fn outbox_purge_failed(&self) {
        *self.outbox_purge_failed_total.lock().expect("lock") += 1;
    }
    fn read_retry(&self, outcome: ReadRetryOutcome) {
        self.read_retries.lock().expect("lock").push(outcome);
    }
    fn list_type_invariant_violation(&self) {
        *self
            .list_type_invariant_violation_total
            .lock()
            .expect("lock") += 1;
    }
    fn audit_publish_failed(&self) {
        *self.audit_publish_failed_total.lock().expect("lock") += 1;
    }
    fn secret_unreadable(&self) {
        *self.secret_unreadable_total.lock().expect("lock") += 1;
    }
}

// ── RecordingAudit ────────────────────────────────────────────────────────────

/// [`AuditSink`] fake that keeps every event it is given.
#[derive(Default)]
pub struct RecordingAudit {
    events: Mutex<Vec<AuditEvent>>,
}

impl RecordingAudit {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Every recorded event, in order.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn events(&self) -> Vec<AuditEvent> {
        self.events.lock().expect("lock").clone()
    }
}

#[async_trait]
impl AuditSink for RecordingAudit {
    async fn record(&self, event: AuditEvent) {
        self.events.lock().expect("lock").push(event);
    }
}
