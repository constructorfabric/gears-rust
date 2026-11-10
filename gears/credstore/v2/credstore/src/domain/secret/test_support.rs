// Updated: 2026-10-06 by Constructor Tech
//! Shared test infrastructure for domain-layer unit tests (ADR-0006).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, EqPredicate, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use credstore_sdk::{
    CredStorePluginClientV2, CredStorePluginDestroyerV2, CredStorePluginError, DestroySelector,
    OwnerId, SecretRef, SecretValue, SharingMode, StoreKey, TenantId, ValueVersion,
};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_security::{AccessScope, PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::audit::{AuditEvent, AuditSink};
pub use crate::domain::ports::metrics::NoopMetrics;
use crate::domain::ports::metrics::{
    CleanupOp, CredStoreMetricsPort, Dep, DepOp, Outcome, ReadOutcome, ReadRetryOutcome, VerifyOp,
    VerifyOutcome,
};
use crate::domain::ports::plugin::PluginSelector;
use crate::domain::resolver::TenantDirectory;
use crate::domain::secret::cleanup::{DeadContent, cleanup_tasks};
use crate::domain::secret::model::{
    CleanupDebt, CleanupTask, DeleteVerification, HealFlags, HealedCreates, IntentCommit,
    NewDeclaredSecret, NewSecret, SecretRow, SecretStatus, WriteAttempt, WriteVerification,
};
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
    /// Tenants `is_ancestor` reports as descendants of every asked ancestor.
    descendants: Vec<Uuid>,
    /// Every `is_ancestor` fails as a tenant-resolver outage.
    is_ancestor_fails: bool,
}

impl FakeDir {
    #[must_use]
    pub fn new(chain: Vec<Uuid>) -> Self {
        Self {
            chain,
            descendants: Vec::new(),
            is_ancestor_fails: false,
        }
    }

    /// Single-tenant chain (only self).
    #[must_use]
    pub fn single(id: Uuid) -> Self {
        Self::new(vec![id])
    }

    /// `is_ancestor` answers true for these tenants (barriers are ignored,
    /// as the adapter asks).
    #[must_use]
    pub fn with_descendants(mut self, descendants: Vec<Uuid>) -> Self {
        self.descendants = descendants;
        self
    }

    /// `is_ancestor` fails like a tenant-resolver outage.
    #[must_use]
    pub fn with_failing_is_ancestor(mut self) -> Self {
        self.is_ancestor_fails = true;
        self
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

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        _ancestor: TenantId,
        descendant: TenantId,
    ) -> Result<bool, DomainError> {
        if self.is_ancestor_fails {
            return Err(DomainError::ServiceUnavailable {
                detail: "tenant resolver unavailable".to_owned(),
                retry_after: None,
                cause: None,
            });
        }
        Ok(self.descendants.contains(&descendant.0))
    }
}

// ── FakePlugin ────────────────────────────────────────────────────────────────

/// Key: `(tenant_id, record_id)` (ADR-0006).
type PluginKey = (Uuid, Uuid);

fn plugin_key(key: &StoreKey) -> PluginKey {
    (key.tenant_id.0, key.record_id)
}

/// Injected `get` fault for one key - see [`FakePlugin::reject_get_for`] /
/// [`FakePlugin::fail_get_for`]. Used by the secret-selecting list concurrency
/// tests, which need a *specific* winner's read (not just "the next `get`
/// call", nondeterministic once reads run concurrently) to fail.
#[derive(Clone, Copy)]
enum FakeGetFault {
    /// The store refused the plugin's own credentials
    /// (`CredStorePluginError::CredentialsRejected`): 503 without a hint.
    Rejected,
    /// A generic backend outage (`CredStorePluginError::Unavailable`) - a
    /// non-`NotFound` error that fails the whole request.
    Error,
    /// A permanent plugin failure (`CredStorePluginError::Internal`): a version
    /// that exists but can never be read.
    Internal,
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
/// [`FakePlugin::without_destroy`] models a backend that does not. Fault-injection
/// hooks cover the write-protocol tests, and every `destroy`/`delete_key` call is recorded.
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
    /// When set, every `get` returns
    /// [`CredStorePluginError::CredentialsRejected`], modelling a store that
    /// refuses the plugin's own credentials.
    get_rejected: bool,
    /// Per-key injected `get` faults (secret-selecting list concurrency tests).
    get_faults: Mutex<HashMap<PluginKey, FakeGetFault>>,
    /// Per-key injected read latency in milliseconds (secret-selecting list
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
    fn build(destroy_supported: bool, get_rejected: bool) -> Self {
        Self {
            store: Mutex::new(HashMap::new()),
            destroy_supported,
            put_failures: Mutex::new(0),
            put_persist_then_fail: Mutex::new(0),
            destroy_failures: Mutex::new(0),
            delete_key_failures: Mutex::new(0),
            not_found_gets: Mutex::new(0),
            get_rejected,
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

    /// A plugin that does not declare `destroy`: `destroyer()` is
    /// `None`, and any `destroy` call it still receives is recorded as a
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

    /// Plugin whose every `get` fails with `CredentialsRejected` - models a
    /// store refusing the plugin's own credentials.
    #[must_use]
    pub fn with_get_rejected() -> Arc<Self> {
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

    /// Always fail `get` for `key` with `CredentialsRejected` - the store
    /// refusing the plugin's own credentials, a request failure (503 without
    /// a hint). Unlike [`Self::fail_next_gets_with_not_found`] (a global counter
    /// over the *next* call, nondeterministic once reads run concurrently),
    /// this targets one specific winner.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn reject_get_for(&self, key: &StoreKey) {
        self.get_faults
            .lock()
            .expect("lock")
            .insert(plugin_key(key), FakeGetFault::Rejected);
    }

    /// Always fail `get` for `key` with a simulated backend outage
    /// (`CredStorePluginError::Unavailable`) - a non-`NotFound` error that
    /// fails the whole request, targeted at one specific winner (see
    /// [`Self::reject_get_for`]).
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
    /// `CredStorePluginError::Internal` (a lost key or an entry the plugin did not
    /// write), targeted at one specific winner (see [`Self::reject_get_for`]).
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn internal_get_for(&self, key: &StoreKey) {
        self.get_faults
            .lock()
            .expect("lock")
            .insert(plugin_key(key), FakeGetFault::Internal);
    }

    /// Make every future `get` for `key` sleep `ms` milliseconds, with the
    /// sleep tracked by [`Self::max_in_flight`] - for asserting the
    /// secret-selecting list's bounded-concurrency fan-out.
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
    ) -> Result<ValueVersion, CredStorePluginError> {
        if Self::take_one(&self.put_failures) {
            return Err(CredStorePluginError::internal(
                "simulated backend put failure",
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
            return Err(CredStorePluginError::unavailable(
                "simulated lost put ack",
                None,
            ));
        }
        Ok(version)
    }

    async fn get(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
        version: &ValueVersion,
    ) -> Result<Option<SecretValue>, CredStorePluginError> {
        self.get_calls.fetch_add(1, Ordering::SeqCst);
        if self.get_rejected {
            return Err(CredStorePluginError::credentials_rejected(
                "simulated rejected credentials",
            ));
        }
        let k = plugin_key(key);
        if let Some(fault) = self.get_faults.lock().expect("lock").get(&k).copied() {
            return match fault {
                FakeGetFault::Rejected => Err(CredStorePluginError::credentials_rejected(
                    "simulated rejected credentials",
                )),
                FakeGetFault::Error => Err(CredStorePluginError::unavailable(
                    "simulated backend get failure",
                    None,
                )),
                FakeGetFault::Internal => Err(CredStorePluginError::internal(
                    "simulated permanent get failure",
                )),
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
    ) -> Result<(), CredStorePluginError> {
        self.delete_key_log.lock().expect("lock").push(key.clone());
        if Self::take_one(&self.delete_key_failures) {
            return Err(CredStorePluginError::internal(
                "simulated backend delete_key failure",
            ));
        }
        self.store.lock().expect("lock").remove(&plugin_key(key));
        Ok(())
    }

    fn destroyer(&self) -> Option<&dyn CredStorePluginDestroyerV2> {
        self.destroy_supported.then_some(self)
    }
}

#[async_trait]
impl CredStorePluginDestroyerV2 for FakePlugin {
    async fn destroy(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
        selector: DestroySelector,
    ) -> Result<(), CredStorePluginError> {
        self.destroy_log
            .lock()
            .expect("lock")
            .push((key.clone(), selector.clone()));
        if !self.destroy_supported {
            return Err(CredStorePluginError::internal(
                "destroy called on a plugin that does not support it",
            ));
        }
        if Self::take_one(&self.destroy_failures) {
            return Err(CredStorePluginError::unavailable(
                "simulated backend destroy failure",
                None,
            ));
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

/// One held write intent. `expired` stands for `lease_until < now()`.
#[derive(Debug, Clone)]
struct FakeIntent {
    attempt_id: Uuid,
    key: StoreKey,
    reference: String,
    expired: bool,
}

/// In-memory [`SecretRepo`] replicating the real (transactional) semantics of
/// each method, for domain-service unit tests.
///
/// `scope_allows` controls the result of [`SecretRepo::scope_includes_tenant`].
pub struct FakeSecretRepo {
    rows: Mutex<Vec<SecretRow>>,
    /// Every cleanup task a repo "transaction" ever recorded as a debt, in
    /// order. A task is appended in the same step that applies the row change
    /// (or intent deletion) that caused it, and never otherwise - the fake's
    /// model of "recorded in the same transaction". Executing and deleting a
    /// debt does not remove it from this log.
    recorded: Mutex<Vec<CleanupTask>>,
    /// The debt rows currently held (`credstore_store_cleanup`).
    debts: Mutex<Vec<CleanupDebt>>,
    /// The write intents currently held (`credstore_write_intents`).
    intents: Mutex<Vec<FakeIntent>>,
    /// Every attempt id `begin_write_intent` ever accepted, in order.
    begun: Mutex<Vec<Uuid>>,
    /// When `> 0`, the next `begin_write_intent` fails (tx0 unavailable).
    begin_intent_failures: Mutex<usize>,
    /// When `> 0`, the next `heal_failed_creates` fails.
    heal_failures: Mutex<usize>,
    /// When `> 0`, a heal "runs just before" each of the next commits
    /// (`insert_active`/`switch_value`): every held intent expires and is
    /// deleted, so the commit finds its own intent gone. The concurrent heal
    /// records no debt (the writer whose intent was healed rolls back and
    /// answers 503, recording nothing).
    heal_before_commit: Mutex<usize>,
    /// One-shot: a concurrent delete (that records nothing itself) removes
    /// this row just before the next commit.
    vanish_before_commit: Mutex<Option<Uuid>>,
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
    /// One-shot: the next `resolve_for_get` returns the stored row with this
    /// id instead of its own answer — a concurrent change making the
    /// re-resolution land on another row than `resolve_candidates` saw.
    pending_redirect: Mutex<Option<Uuid>>,
    /// When `> 0`, the next `insert_active` applies its change and THEN fails
    /// with a simulated internal error: an ambiguous commit that did happen.
    insert_active_fail_after_commit: Mutex<usize>,
    /// Same for `switch_value`.
    switch_value_fail_after_commit: Mutex<usize>,
    /// When `> 0`, the next `delete_by_id` fails before touching rows: an
    /// ambiguous commit that did not happen.
    delete_failures: Mutex<usize>,
    /// When `> 0`, the next `delete_by_id` deletes the row and THEN fails: an
    /// ambiguous commit that did happen.
    delete_fail_after_commit: Mutex<usize>,
    /// When `> 0`, the next `verify_*` call fails (the verification
    /// transaction itself cannot be written).
    verify_failures: Mutex<usize>,
    /// How many `verify_*` calls were made.
    verifications: Mutex<usize>,
    /// When set, `delete_by_id` returns an error — simulating a DB failure.
    fail_delete: bool,
    /// When `> 0`, the next `delete_by_id` call reports `NotFound`
    /// regardless of actual row state — models a row vanishing concurrently
    /// between the caller's precheck and this call.
    force_delete_by_id_not_found: Mutex<usize>,
    /// When `> 0`, the next `verify_delete` call that finds the row reports
    /// `NotFound`: the row is there but the scope does not admit it.
    force_verify_delete_not_found: Mutex<usize>,
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
            recorded: Mutex::new(Vec::new()),
            debts: Mutex::new(Vec::new()),
            intents: Mutex::new(Vec::new()),
            begun: Mutex::new(Vec::new()),
            begin_intent_failures: Mutex::new(0),
            heal_failures: Mutex::new(0),
            heal_before_commit: Mutex::new(0),
            vanish_before_commit: Mutex::new(None),
            scope_allows: true,
            insert_active_failures: Mutex::new(0),
            insert_active_conflicts: Mutex::new(0),
            switch_value_failures: Mutex::new(0),
            force_switch_value_none: Mutex::new(0),
            pending_switch: Mutex::new(None),
            pending_vanish: Mutex::new(None),
            pending_redirect: Mutex::new(None),
            insert_active_fail_after_commit: Mutex::new(0),
            switch_value_fail_after_commit: Mutex::new(0),
            delete_failures: Mutex::new(0),
            delete_fail_after_commit: Mutex::new(0),
            verify_failures: Mutex::new(0),
            verifications: Mutex::new(0),
            fail_delete: false,
            force_delete_by_id_not_found: Mutex::new(0),
            force_verify_delete_not_found: Mutex::new(0),
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

    /// Arrange for the next `n` `insert_active` calls to apply their change
    /// and then fail: an ambiguous commit that did happen.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn fail_next_insert_active_after_commit(&self, n: usize) {
        *self.insert_active_fail_after_commit.lock().expect("lock") += n;
    }

    /// Arrange for the next `n` `switch_value` calls to apply their change
    /// and then fail: an ambiguous commit that did happen.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn fail_next_switch_value_after_commit(&self, n: usize) {
        *self.switch_value_fail_after_commit.lock().expect("lock") += n;
    }

    /// Arrange for the next `n` `delete_by_id` calls to fail without
    /// touching rows: an ambiguous commit that did not happen.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn fail_next_delete_by_id(&self, n: usize) {
        *self.delete_failures.lock().expect("lock") += n;
    }

    /// Arrange for the next `n` `delete_by_id` calls to delete the row and
    /// then fail: an ambiguous commit that did happen.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn fail_next_delete_by_id_after_commit(&self, n: usize) {
        *self.delete_fail_after_commit.lock().expect("lock") += n;
    }

    /// Arrange for the next `n` verification transactions to fail.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn fail_next_verifications(&self, n: usize) {
        *self.verify_failures.lock().expect("lock") += n;
    }

    /// How many verification transactions were run.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn verification_count(&self) -> usize {
        *self.verifications.lock().expect("lock")
    }

    /// Counts a verification and applies the injected failure, if any.
    fn begin_verification(&self) -> Result<(), DomainError> {
        *self.verifications.lock().expect("lock") += 1;
        if Self::take_failure(&self.verify_failures) {
            return Err(DomainError::internal("simulated verification failure"));
        }
        Ok(())
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

    /// One-shot: the next `resolve_for_get` answers with the stored row
    /// `row_id` whatever it would have resolved to.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn redirect_next_resolve_for_get(&self, row_id: Uuid) {
        *self.pending_redirect.lock().expect("lock") = Some(row_id);
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

    /// Arrange for the next `n` `verify_delete` calls that find the row to
    /// report `NotFound`: the row exists but the scope does not admit it.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn force_next_verify_delete_not_found(&self, n: usize) {
        *self.force_verify_delete_not_found.lock().expect("lock") += n;
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

    /// Every key purge recorded so far (by a delete, a lost write, an
    /// intent-lost step or a failed-create heal), in order, including those
    /// already executed.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn purged_keys(&self) -> Vec<StoreKey> {
        self.recorded
            .lock()
            .expect("lock")
            .iter()
            .filter_map(|t| match t {
                CleanupTask::Purge(key) => Some(key.clone()),
                CleanupTask::Destroy { .. } => None,
            })
            .collect()
    }

    /// Every cleanup task (purge or destroy) recorded as a debt so far, in
    /// order, including those already executed.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn recorded_tasks(&self) -> Vec<CleanupTask> {
        self.recorded.lock().expect("lock").clone()
    }

    /// The tasks of the debt rows still held (recorded and not yet deleted
    /// after a successful execution), in order.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn pending_tasks(&self) -> Vec<CleanupTask> {
        self.debts
            .lock()
            .expect("lock")
            .iter()
            .map(|d| d.task.clone())
            .collect()
    }

    /// Seeds a debt row, as an earlier request left it (recorded, executed
    /// by nobody).
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn seed_debt(&self, task: CleanupTask) {
        self.record(vec![task]);
    }

    /// Seeds a write intent of `reference` under `key`, as a writer that
    /// crashed left it; `expired` stands for `lease_until < now()`.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn seed_intent(&self, key: &StoreKey, reference: &str, expired: bool) -> Uuid {
        let attempt_id = Uuid::new_v4();
        self.intents.lock().expect("lock").push(FakeIntent {
            attempt_id,
            key: key.clone(),
            reference: reference.to_owned(),
            expired,
        });
        attempt_id
    }

    /// The write intents currently held, as `(attempt_id, key)`.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn intents(&self) -> Vec<(Uuid, StoreKey)> {
        self.intents
            .lock()
            .expect("lock")
            .iter()
            .map(|i| (i.attempt_id, i.key.clone()))
            .collect()
    }

    /// Every attempt id `begin_write_intent` ever accepted, in order.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn begun_attempts(&self) -> Vec<Uuid> {
        self.begun.lock().expect("lock").clone()
    }

    /// Every held intent's lease is over (`lease_until < now()`).
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn expire_intents(&self) {
        for intent in &mut *self.intents.lock().expect("lock") {
            intent.expired = true;
        }
    }

    /// Arrange for the next `n` `begin_write_intent` calls to fail (tx0
    /// cannot reach the database).
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn fail_next_begin_write_intent(&self, n: usize) {
        *self.begin_intent_failures.lock().expect("lock") += n;
    }

    /// Arrange for the next `n` `heal_failed_creates` calls to fail.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn fail_next_heal_failed_creates(&self, n: usize) {
        *self.heal_failures.lock().expect("lock") += n;
    }

    /// Models another request healing just before each of the next `n`
    /// commits (`insert_active`/`switch_value`): the writer's intent is
    /// expired and deleted before its commit transaction runs, so the commit
    /// finds it gone.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn heal_intents_before_next_commits(&self, n: usize) {
        *self.heal_before_commit.lock().expect("lock") += n;
    }

    /// One-shot: a concurrent delete (which records nothing itself, so the
    /// writer's own debts stay distinguishable) removes `row_id` just
    /// before the next commit - the late-writer scenario.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn delete_row_before_next_commit(&self, row_id: Uuid) {
        *self.vanish_before_commit.lock().expect("lock") = Some(row_id);
    }

    /// Records `tasks` as debt rows (the same step as the change that made
    /// the store content dead) and returns them.
    fn record(&self, tasks: Vec<CleanupTask>) -> Vec<CleanupDebt> {
        self.recorded
            .lock()
            .expect("lock")
            .extend(tasks.iter().cloned());
        let debts: Vec<CleanupDebt> = tasks
            .into_iter()
            .map(|task| CleanupDebt {
                id: Uuid::new_v4(),
                task,
            })
            .collect();
        self.debts
            .lock()
            .expect("lock")
            .extend(debts.iter().cloned());
        debts
    }

    /// The heal flags of the record `(tenant, id)`: pending debts, expired
    /// intents.
    fn heal_flags(&self, tenant: TenantId, id: Uuid) -> HealFlags {
        let key = StoreKey::new(tenant, id);
        HealFlags {
            debts: self
                .debts
                .lock()
                .expect("lock")
                .iter()
                .any(|d| d.task.key() == &key),
            expired_intents: self
                .intents
                .lock()
                .expect("lock")
                .iter()
                .any(|i| i.expired && i.key == key),
        }
    }

    /// `row` with the heal flags a row-reading SQL would return beside it.
    fn with_flags(&self, mut row: SecretRow) -> SecretRow {
        row.heal = self.heal_flags(row.tenant_id, row.id);
        row
    }

    fn row_exists(&self, record_id: Uuid) -> bool {
        self.rows
            .lock()
            .expect("lock")
            .iter()
            .any(|r| r.id == record_id)
    }

    /// What a write that lost (or lost its intent) leaves as debts: the
    /// row-existence fact, fed to the one cleanup rule
    /// ([`cleanup_tasks`], `DeadContent::LostWrite`) like the SQL repo.
    fn lost_write_tasks(
        &self,
        key: &StoreKey,
        version: &ValueVersion,
        destroy_supported: bool,
    ) -> Vec<CleanupTask> {
        cleanup_tasks(
            DeadContent::LostWrite {
                key,
                version,
                row_exists: self.row_exists(key.record_id),
            },
            destroy_supported,
        )
    }

    /// Deletes the attempt's intent; `true` iff it existed.
    fn retire_intent(&self, attempt_id: Uuid) -> bool {
        let mut intents = self.intents.lock().expect("lock");
        let before = intents.len();
        intents.retain(|i| i.attempt_id != attempt_id);
        intents.len() < before
    }

    /// The state change of `insert_active`'s transaction (from retiring the
    /// intent on), shared with the verification that runs it again.
    fn apply_insert_active(&self, new: &NewSecret, attempt: &WriteAttempt) -> IntentCommit<()> {
        if !self.retire_intent(attempt.attempt_id) {
            return IntentCommit::IntentLost;
        }
        // The create lost: the intent deletion committed, with the `purge` debt
        // for the attempt's fresh key in the same transaction.
        let conflict = Self::take_failure(&self.insert_active_conflicts) || {
            let rows = self.rows.lock().expect("lock");
            rows.iter().any(|r| {
                r.tenant_id == new.tenant_id
                    && r.reference == new.reference.as_ref()
                    && match new.sharing {
                        SharingMode::Private => {
                            r.sharing == SharingMode::Private && r.owner_id == new.owner_id
                        }
                        _ => r.sharing != SharingMode::Private,
                    }
            })
        };
        if conflict {
            let tasks = cleanup_tasks(
                DeadContent::LostWrite {
                    key: &attempt.key,
                    version: &new.value_version,
                    row_exists: false,
                },
                attempt.destroy_supported,
            );
            let debts = self.record(tasks);
            return IntentCommit::Lost { debts };
        }
        self.rows.lock().expect("lock").push(SecretRow {
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
            heal: HealFlags::default(),
        });
        IntentCommit::Committed {
            value: (),
            debts: Vec::new(),
            healed: 0,
        }
    }

    /// The state change of `switch_value`'s transaction (from retiring the
    /// intent on), shared with the verification that runs it again.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors SecretRepo::switch_value"
    )]
    fn apply_switch_value(
        &self,
        id: Uuid,
        expected_version: i64,
        sharing: SharingMode,
        fallback: crate::domain::secret::model::Fallback,
        expires_at: Option<OffsetDateTime>,
        new_value_version: &ValueVersion,
        attempt: &WriteAttempt,
    ) -> IntentCommit<SecretRow> {
        if !self.retire_intent(attempt.attempt_id) {
            return IntentCommit::IntentLost;
        }
        let forced_loss = Self::take_failure(&self.force_switch_value_none);
        let switched = if forced_loss {
            None
        } else {
            let mut rows = self.rows.lock().expect("lock");
            rows.iter_mut()
                .find(|r| {
                    r.id == id
                        && matches!(r.status, SecretStatus::Active | SecretStatus::Declared)
                        && r.version == expected_version
                })
                .map(|row| {
                    row.value_version = Some(new_value_version.clone());
                    row.sharing = sharing;
                    row.fallback = fallback;
                    row.expires_at = expires_at;
                    row.status = SecretStatus::Active;
                    row.version += 1;
                    row.updated_at = OffsetDateTime::now_utc();
                    row.clone()
                })
        };
        let Some(row) = switched else {
            // A definite loss: the intent deletion committed, with the debt
            // for this attempt's version in the same transaction.
            let tasks =
                self.lost_write_tasks(&attempt.key, new_value_version, attempt.destroy_supported);
            let debts = self.record(tasks);
            return IntentCommit::Lost { debts };
        };
        let tasks = cleanup_tasks(
            DeadContent::Superseded {
                key: &attempt.key,
                new: new_value_version,
            },
            attempt.destroy_supported,
        );
        let debts = self.record(tasks);
        // Heal the record's expired intents, when the row read reported some.
        let healed = if attempt.heal_expired_intents {
            let mut intents = self.intents.lock().expect("lock");
            let before = intents.len();
            intents.retain(|i| !(i.expired && i.key == attempt.key));
            (before - intents.len()) as u64
        } else {
            0
        };
        IntentCommit::Committed {
            value: row,
            debts,
            healed,
        }
    }

    /// The state change of `delete_by_id`'s transaction.
    fn apply_delete(
        &self,
        key: &StoreKey,
        expected_version: Option<i64>,
    ) -> Result<Vec<CleanupDebt>, DomainError> {
        let mut rows = self.rows.lock().expect("lock");
        let idx = rows
            .iter()
            .position(|r| r.id == key.record_id && expected_version.is_none_or(|v| r.version == v));
        let Some(idx) = idx else {
            return Err(DomainError::NotFound);
        };
        rows.remove(idx);
        drop(rows);
        // The row delete and the purge debt are one transaction.
        Ok(self.record(cleanup_tasks(DeadContent::DeadKey { key }, false)))
    }

    /// Concurrent activity scheduled to land just before a commit.
    fn before_commit(&self) {
        let vanish = self.vanish_before_commit.lock().expect("lock").take();
        if let Some(row_id) = vanish {
            self.rows.lock().expect("lock").retain(|r| r.id != row_id);
        }
        if Self::take_failure(&self.heal_before_commit) {
            self.expire_intents();
            self.intents.lock().expect("lock").clear();
        }
    }

    fn take_failure(counter: &Mutex<usize>) -> bool {
        let mut remaining = counter.lock().expect("lock");
        if *remaining > 0 {
            *remaining -= 1;
            true
        } else {
            false
        }
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

    /// Resolution-eligible (ADR-0008, Resolution): `active` (expired or not —
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
        let redirect = self.pending_redirect.lock().expect("lock").take();
        let result = match redirect {
            Some(id) => rows.iter().find(|r| r.id == id).cloned(),
            None => best.cloned(),
        }
        .map(|r| self.with_flags(r));
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

    async fn resolve_non_private(
        &self,
        req_tenant: TenantId,
        key: &SecretRef,
        chain: &[Uuid],
    ) -> Result<Option<SecretRow>, DomainError> {
        let rows = self.rows.lock().expect("lock");
        let pos = |t: Uuid| chain.iter().position(|c| *c == t).unwrap_or(usize::MAX);
        let best = rows
            .iter()
            .filter(|r| {
                Self::resolution_eligible(r)
                    && r.reference == key.as_ref()
                    && chain.contains(&r.tenant_id.0)
                    && match r.sharing {
                        SharingMode::Private => false,
                        SharingMode::Tenant => r.tenant_id == req_tenant,
                        SharingMode::Shared => true,
                    }
            })
            .min_by_key(|r| pos(r.tenant_id.0));
        Ok(best.cloned().map(|r| self.with_flags(r)))
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
        Ok(best.cloned().map(|r| self.with_flags(r)))
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
        Ok(row.cloned().map(|r| self.with_flags(r)))
    }

    async fn scope_includes_tenant(
        &self,
        _scope: &AccessScope,
        _tenant: Uuid,
    ) -> Result<bool, DomainError> {
        Ok(self.scope_allows)
    }

    async fn list_tenants_with_other_type(
        &self,
        reference: &SecretRef,
        requested_type: Uuid,
        exclude_tenant: TenantId,
        after: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<Uuid>, DomainError> {
        let rows = self.rows.lock().expect("lock");
        let tenants: std::collections::BTreeSet<Uuid> = rows
            .iter()
            .filter(|r| r.reference == reference.as_ref())
            .filter(|r| r.secret_type_uuid != requested_type)
            .filter(|r| r.sharing != SharingMode::Private)
            .filter(|r| r.tenant_id != exclude_tenant)
            .map(|r| r.tenant_id.0)
            .filter(|t| after.is_none_or(|a| *t > a))
            .collect();
        Ok(tenants
            .into_iter()
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .collect())
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

    async fn begin_write_intent(
        &self,
        attempt: &WriteAttempt,
        _lease: Duration,
    ) -> Result<(), DomainError> {
        if Self::take_failure(&self.begin_intent_failures) {
            return Err(DomainError::internal(
                "simulated begin_write_intent failure",
            ));
        }
        self.intents.lock().expect("lock").push(FakeIntent {
            attempt_id: attempt.attempt_id,
            key: attempt.key.clone(),
            reference: attempt.reference.clone(),
            expired: false,
        });
        self.begun.lock().expect("lock").push(attempt.attempt_id);
        Ok(())
    }

    async fn insert_active(
        &self,
        _scope: &AccessScope,
        new: &NewSecret,
        attempt: &WriteAttempt,
    ) -> Result<IntentCommit<()>, DomainError> {
        // A failure models an ambiguous tx1: nothing is applied here, the
        // intent stays.
        self.before_commit();
        if Self::take_failure(&self.insert_active_failures) {
            return Err(DomainError::internal("simulated insert_active failure"));
        }
        let out = self.apply_insert_active(new, attempt);
        if Self::take_failure(&self.insert_active_fail_after_commit) {
            return Err(DomainError::internal(
                "simulated insert_active failure after the commit",
            ));
        }
        Ok(out)
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
            heal: HealFlags::default(),
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
        attempt: &WriteAttempt,
    ) -> Result<IntentCommit<SecretRow>, DomainError> {
        // A failure models an ambiguous tx1: nothing is applied here, the
        // intent stays.
        self.before_commit();
        if Self::take_failure(&self.switch_value_failures) {
            return Err(DomainError::internal("simulated switch_value failure"));
        }
        let out = self.apply_switch_value(
            id,
            expected_version,
            sharing,
            fallback,
            expires_at,
            &new_value_version,
            attempt,
        );
        if Self::take_failure(&self.switch_value_fail_after_commit) {
            return Err(DomainError::internal(
                "simulated switch_value failure after the commit",
            ));
        }
        Ok(out)
    }

    async fn heal_failed_creates(
        &self,
        tenant: TenantId,
        reference: &SecretRef,
    ) -> Result<HealedCreates, DomainError> {
        if Self::take_failure(&self.heal_failures) {
            return Err(DomainError::internal(
                "simulated heal_failed_creates failure",
            ));
        }
        // Expired intents of the reference whose record has no row.
        let healed: Vec<FakeIntent> = {
            let mut intents = self.intents.lock().expect("lock");
            let mut taken = Vec::new();
            let mut kept = Vec::new();
            for intent in intents.drain(..) {
                if intent.expired
                    && intent.key.tenant_id == tenant
                    && intent.reference == reference.as_ref()
                    && !self.row_exists(intent.key.record_id)
                {
                    taken.push(intent);
                } else {
                    kept.push(intent);
                }
            }
            *intents = kept;
            taken
        };
        let mut keys: Vec<StoreKey> = Vec::new();
        for intent in &healed {
            if !keys.contains(&intent.key) {
                keys.push(intent.key.clone());
            }
        }
        let debts = self.record(
            keys.iter()
                .flat_map(|key| cleanup_tasks(DeadContent::DeadKey { key }, false))
                .collect(),
        );
        Ok(HealedCreates {
            intents: healed.len() as u64,
            debts,
        })
    }

    async fn pending_debts(&self, key: &StoreKey) -> Result<Vec<CleanupDebt>, DomainError> {
        Ok(self
            .debts
            .lock()
            .expect("lock")
            .iter()
            .filter(|d| d.task.key() == key)
            .cloned()
            .collect())
    }

    async fn delete_debt(&self, id: Uuid) -> Result<(), DomainError> {
        self.debts.lock().expect("lock").retain(|d| d.id != id);
        Ok(())
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

    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors SecretRepo::remove_value's one-CAS-with-every-field-it-may-update shape"
    )]
    async fn remove_value(
        &self,
        _scope: &AccessScope,
        id: Uuid,
        expected_version: Option<i64>,
        sharing: SharingMode,
        fallback: crate::domain::secret::model::Fallback,
        expires_at: Option<OffsetDateTime>,
        destroy_supported: bool,
    ) -> Result<Option<(SecretRow, Vec<CleanupDebt>)>, DomainError> {
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
        let row = row.clone();
        drop(rows);
        // The destroy debts are recorded by the same transaction as the CAS.
        let key = row.store_key();
        let tasks = match &old_value_version {
            Some(old) => cleanup_tasks(DeadContent::Removed { key: &key, old }, destroy_supported),
            None => Vec::new(),
        };
        let debts = self.record(tasks);
        Ok(Some((row, debts)))
    }

    async fn delete_by_id(
        &self,
        _scope: &AccessScope,
        key: &StoreKey,
        expected_version: Option<i64>,
    ) -> Result<Vec<CleanupDebt>, DomainError> {
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
        if Self::take_failure(&self.delete_failures) {
            return Err(DomainError::internal("simulated delete failure"));
        }
        let debts = self.apply_delete(key, expected_version)?;
        if Self::take_failure(&self.delete_fail_after_commit) {
            return Err(DomainError::internal(
                "simulated delete failure after the commit",
            ));
        }
        Ok(debts)
    }

    async fn verify_insert_active(
        &self,
        _scope: &AccessScope,
        new: &NewSecret,
        attempt: &WriteAttempt,
    ) -> Result<WriteVerification<()>, DomainError> {
        self.begin_verification()?;
        // The locking read of the own intent.
        let intent_present = self
            .intents
            .lock()
            .expect("lock")
            .iter()
            .any(|i| i.attempt_id == attempt.attempt_id);
        if intent_present {
            return Ok(WriteVerification::Retried(
                self.apply_insert_active(new, attempt),
            ));
        }
        let row = self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .find(|r| r.id == attempt.key.record_id)
            .cloned();
        if let Some(row) = row.filter(|r| r.value_version.as_ref() == Some(&new.value_version)) {
            return Ok(WriteVerification::Committed { row });
        }
        let tasks =
            self.lost_write_tasks(&attempt.key, &new.value_version, attempt.destroy_supported);
        Ok(WriteVerification::NotApplied {
            debts: self.record(tasks),
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors SecretRepo::verify_switch_value"
    )]
    async fn verify_switch_value(
        &self,
        _scope: &AccessScope,
        id: Uuid,
        expected_version: i64,
        sharing: SharingMode,
        fallback: crate::domain::secret::model::Fallback,
        expires_at: Option<OffsetDateTime>,
        new_value_version: ValueVersion,
        attempt: &WriteAttempt,
    ) -> Result<WriteVerification<SecretRow>, DomainError> {
        self.begin_verification()?;
        let intent_present = self
            .intents
            .lock()
            .expect("lock")
            .iter()
            .any(|i| i.attempt_id == attempt.attempt_id);
        if intent_present {
            return Ok(WriteVerification::Retried(self.apply_switch_value(
                id,
                expected_version,
                sharing,
                fallback,
                expires_at,
                &new_value_version,
                attempt,
            )));
        }
        let row = self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .find(|r| r.id == attempt.key.record_id)
            .cloned();
        if let Some(row) = row.filter(|r| r.value_version.as_ref() == Some(&new_value_version)) {
            return Ok(WriteVerification::Committed { row });
        }
        let tasks =
            self.lost_write_tasks(&attempt.key, &new_value_version, attempt.destroy_supported);
        Ok(WriteVerification::NotApplied {
            debts: self.record(tasks),
        })
    }

    async fn verify_delete(
        &self,
        _scope: &AccessScope,
        key: &StoreKey,
        expected_version: Option<i64>,
    ) -> Result<DeleteVerification, DomainError> {
        self.begin_verification()?;
        let row = self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .find(|r| r.id == key.record_id)
            .cloned();
        let Some(row) = row else {
            return Ok(DeleteVerification::Committed);
        };
        if Self::take_failure(&self.force_verify_delete_not_found) {
            return Ok(DeleteVerification::NotFound);
        }
        if expected_version.is_some_and(|v| row.version != v) {
            return Ok(DeleteVerification::PreconditionFailed);
        }
        Ok(DeleteVerification::Retried {
            debts: self.apply_delete(key, expected_version)?,
        })
    }
}

// ── FakeMetrics ───────────────────────────────────────────────────────────────

/// Recording metrics fake for assertions in tests.
pub struct FakeMetrics {
    pub cross_tenant_denied_count: Mutex<u64>,
    pub read_outcomes: Mutex<Vec<ReadOutcome>>,
    pub deps: Mutex<Vec<(Dep, DepOp, Outcome)>>,
    pub write_intents_healed_total: Mutex<u64>,
    pub store_cleanup_recorded: Mutex<Vec<CleanupOp>>,
    pub store_cleanup_failed: Mutex<Vec<CleanupOp>>,
    pub read_retries: Mutex<Vec<ReadRetryOutcome>>,
    pub commit_verifications: Mutex<Vec<(VerifyOp, VerifyOutcome)>>,
    pub audit_publish_failed_total: Mutex<u64>,
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

    /// Total expired write intents reported healed.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn write_intents_healed_total(&self) -> u64 {
        *self.write_intents_healed_total.lock().expect("lock")
    }

    /// Every recorded store-cleanup debt's op, in order.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn store_cleanup_recorded(&self) -> Vec<CleanupOp> {
        self.store_cleanup_recorded.lock().expect("lock").clone()
    }

    /// Every failed store-cleanup execution's op, in order.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn store_cleanup_failed(&self) -> Vec<CleanupOp> {
        self.store_cleanup_failed.lock().expect("lock").clone()
    }

    /// Every recorded commit verification `(op, outcome)`, in order.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn commit_verifications(&self) -> Vec<(VerifyOp, VerifyOutcome)> {
        self.commit_verifications.lock().expect("lock").clone()
    }

    /// Every recorded read-retry outcome, in order.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn read_retries(&self) -> Vec<ReadRetryOutcome> {
        self.read_retries.lock().expect("lock").clone()
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
}

impl Default for FakeMetrics {
    fn default() -> Self {
        Self {
            cross_tenant_denied_count: Mutex::new(0),
            read_outcomes: Mutex::new(Vec::new()),
            deps: Mutex::new(Vec::new()),
            write_intents_healed_total: Mutex::new(0),
            store_cleanup_recorded: Mutex::new(Vec::new()),
            store_cleanup_failed: Mutex::new(Vec::new()),
            read_retries: Mutex::new(Vec::new()),
            commit_verifications: Mutex::new(Vec::new()),
            audit_publish_failed_total: Mutex::new(0),
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
    fn write_intents_healed(&self, n: u64) {
        *self.write_intents_healed_total.lock().expect("lock") += n;
    }
    fn store_cleanup_recorded(&self, op: CleanupOp) {
        self.store_cleanup_recorded.lock().expect("lock").push(op);
    }
    fn store_cleanup_failed(&self, op: CleanupOp) {
        self.store_cleanup_failed.lock().expect("lock").push(op);
    }
    fn write_commit_verified(&self, op: VerifyOp, outcome: VerifyOutcome) {
        self.commit_verifications
            .lock()
            .expect("lock")
            .push((op, outcome));
    }
    fn read_retry(&self, outcome: ReadRetryOutcome) {
        self.read_retries.lock().expect("lock").push(outcome);
    }
    fn audit_publish_failed(&self) {
        *self.audit_publish_failed_total.lock().expect("lock") += 1;
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
