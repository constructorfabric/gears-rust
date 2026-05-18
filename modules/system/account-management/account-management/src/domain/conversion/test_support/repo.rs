//! In-memory [`FakeConversionRepo`] covering the
//! [`crate::domain::conversion::repo::ConversionRepo`] trait contract.
//!
//! Production semantics this fake mirrors:
//!
//! * Partial-unique-index `ux_conversion_requests_pending`: at most one
//!   `pending` row per `tenant_id` whose `deleted_at IS NULL`. A second
//!   `insert_pending` against the same tenant surfaces
//!   [`DomainError::PendingExists`] carrying the existing pending row's
//!   id; resolved (`approved` / `cancelled` / `rejected` / `expired`)
//!   and soft-deleted prior rows do NOT block a new pending insert.
//! * Atomic guarded transitions: every `transition_pending_to_*` flips
//!   only when the row's current status is `pending`; otherwise
//!   returns [`DomainError::AlreadyResolved`]. Missing rows return
//!   [`DomainError::NotFound`] with the request id.
//! * Soft-delete handling: every read / list method excludes rows with
//!   `deleted_at IS NOT NULL`, matching the SQL impl.
//!
//! State is stored behind `Arc<Mutex<…>>` so the fake is `Clone + Send +
//! Sync` and can be shared across tasks the way `FakeTenantRepo` is.

#![allow(
    dead_code,
    reason = "test-support fake; not every public helper has a caller yet, later phases add service-level test sites"
)]
#![allow(
    clippy::must_use_candidate,
    reason = "test-support fake; every constructor/getter is intended for ad-hoc test wiring, the compiler nag is noise"
)]
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    reason = "test-support fake; doc-equivalent error/panic semantics live on the trait this impl mirrors"
)]
#![allow(
    clippy::new_without_default,
    reason = "test-support fake: explicit `new()` is the canonical entry point; a Default impl would only obscure that"
)]
#![allow(
    clippy::module_name_repetitions,
    reason = "FakeConversionRepo follows the FakeTenantRepo naming pattern"
)]
#![allow(
    clippy::expect_used,
    reason = "test-support fake; mutex `lock().expect(\"lock\")` is the canonical pattern, panics on poisoned mutex are acceptable in fakes"
)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use modkit_security::AccessScope;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::conversion::model::{
    ConversionPagination, ConversionRequest, ConversionStatus, NewConversionRequest, TargetMode,
};
use crate::domain::conversion::repo::{ApplyConversionApprovalInput, ConversionRepo};
use crate::domain::error::DomainError;
use crate::domain::tenant::closure::ClosureRow;
use crate::domain::tenant::model::TenantStatus;
use crate::domain::tenant::test_support::FakeTenantRepo;

/// In-memory state shared behind `Arc<Mutex<…>>`.
///
/// `pending_by_tenant` is the derived index that mirrors the production
/// partial-unique on `(tenant_id) WHERE status = pending AND deleted_at
/// IS NULL`. Updating the index in lockstep with `rows` is the fake's
/// single source of truth for the at-most-one-pending invariant — a
/// second `insert_pending` against a tenant whose `request_id` is
/// already in the map surfaces [`DomainError::PendingExists`] without
/// scanning every row.
struct State {
    rows: HashMap<Uuid, ConversionRequest>,
    pending_by_tenant: HashMap<Uuid, Uuid>,
    /// Ids that `query_expired` still returns (mimicking the scan
    /// observing the row before the concurrent FK-cascade /
    /// retention sweep), but that every subsequent transition
    /// lookup treats as missing — surfacing `DomainError::NotFound`
    /// from `lookup_pending_mut`. Used by the expire-pending
    /// idempotent-skip test to simulate the row vanishing between
    /// `query_expired` and `transition_pending_to_expired` without
    /// a real concurrent mutator.
    ///
    /// Note: affects ALL four `lookup_pending_mut`-routed
    /// transitions (approve / cancel / reject / expire), not just
    /// expire. A test that flags an id with `mark_vanished` and
    /// then drives `cancel` against the service will observe the
    /// same `NotFound` surface.
    vanished_ids: HashSet<Uuid>,
    /// Single-shot per-row errors injected via
    /// [`FakeConversionRepo::inject_lookup_error`]. Consumed by
    /// `lookup_pending_mut` via `remove(&id)`; the per-row
    /// transition then surfaces the error verbatim. Used by the
    /// escalation-warn boundary tests to drive a non-`NotFound`,
    /// non-`AlreadyResolved` per-row failure (which routes through
    /// the `Err(other)` arm of `expire_pending` and increments
    /// `failed`) without spinning up a real DB-fault.
    ///
    /// `DomainError` does not implement `Clone` (it carries
    /// `Option<BoxError>` in some variants), so this map uses
    /// `remove` to take ownership on first lookup. Tests that need
    /// the error to fire on more than one transition call should
    /// re-inject between calls.
    injected_errors: HashMap<Uuid, DomainError>,
    /// Captured `AccessScope` values for every repo method invocation
    /// (most recent last). Used by service-layer tests to assert
    /// that mutating call sites pass the documented scope to the
    /// repo (typically `AccessScope::allow_all()` since
    /// `conversion_requests` is `Scopable(no_tenant, no_resource,
    /// no_owner, no_type)` and the repo runs at `allow_all`).
    ///
    /// Without this, a regression that accidentally forwards a
    /// narrowed `AccessScope` from the service into a repo method
    /// would silently produce a `WHERE false` in `SeaORM` (no rows
    /// matched, mutation becomes no-op) but pass any
    /// `FakeConversionRepo`-backed test because the fake currently
    /// ignores `_scope`. Capturing the scope here lets the tests
    /// pin the forwarding contract.
    captured_scopes: Vec<AccessScope>,
}

impl State {
    fn new() -> Self {
        Self {
            rows: HashMap::new(),
            pending_by_tenant: HashMap::new(),
            vanished_ids: HashSet::new(),
            injected_errors: HashMap::new(),
            captured_scopes: Vec::new(),
        }
    }

    /// Reseed the derived `pending_by_tenant` index from `rows`. Used
    /// by [`FakeConversionRepo::with_seed`] when the test seeds rows
    /// directly via the constructor.
    ///
    /// Panics on duplicate live-pending rows for the same tenant — the
    /// fake mirrors the partial-unique invariant
    /// `ux_conversion_requests_pending` (`WHERE status = 0 AND
    /// deleted_at IS NULL`), so a fixture with two such rows would be
    /// nondeterministic (`HashMap` iteration order picks the winner).
    /// Failing fast at seed time keeps the fake's coverage of the
    /// uniqueness contract honest.
    fn rebuild_pending_index(&mut self) {
        self.pending_by_tenant.clear();
        for row in self.rows.values() {
            if matches!(row.status, ConversionStatus::Pending) && row.deleted_at.is_none() {
                let prior = self.pending_by_tenant.insert(row.tenant_id, row.id);
                assert!(
                    prior.is_none(),
                    "FakeConversionRepo seed has two live-pending rows for tenant {:?} \
                     ({:?} and {:?}); production's partial-unique index would reject this",
                    row.tenant_id,
                    prior.unwrap(),
                    row.id,
                );
            }
        }
    }
}

/// Cloneable test repo that satisfies [`ConversionRepo`].
///
/// `Clone` only clones the `Arc`, so multiple cloned handles share the
/// same `State`. This matches the production `Arc<dyn ConversionRepo>`
/// shape used by the service layer in later phases.
#[derive(Clone)]
pub struct FakeConversionRepo {
    inner: Arc<Mutex<State>>,
    /// Optional cross-fake handle wired in by service-level tests that
    /// exercise the dual-consent apply seam. The fake's
    /// [`ConversionRepo::apply_conversion_approval`] body needs to
    /// flip `tenants.self_managed` and rewrite `tenant_closure.barrier`
    /// in lockstep with the request transition; without this handle
    /// the apply path returns
    /// [`DomainError::Internal`]. Tests that only exercise the
    /// non-apply paths leave it unset.
    tenant_repo: Option<Arc<FakeTenantRepo>>,
}

impl FakeConversionRepo {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(State::new())),
            tenant_repo: None,
        }
    }

    /// Build a fake pre-populated with `rows`. The derived
    /// `pending_by_tenant` index is recomputed from the seed so the
    /// invariant remains: at most one pending row per tenant.
    pub fn with_seed(rows: Vec<ConversionRequest>) -> Self {
        let repo = Self::new();
        {
            let mut state = repo.inner.lock().expect("lock");
            for row in rows {
                state.rows.insert(row.id, row);
            }
            state.rebuild_pending_index();
        }
        repo
    }

    /// Wire the cross-fake [`FakeTenantRepo`] handle used by
    /// [`ConversionRepo::apply_conversion_approval`]. Service-level
    /// approve tests call this before invoking
    /// [`crate::domain::conversion::service::ConversionService::approve`]
    /// so the apply path can flip `tenants.self_managed` and rewrite
    /// closure-row barriers in lockstep with the request transition.
    #[must_use]
    pub fn with_tenant_repo(mut self, repo: Arc<FakeTenantRepo>) -> Self {
        self.tenant_repo = Some(repo);
        self
    }

    /// Mark `request_id` as having "vanished" between scan and
    /// transition. `query_expired` still returns the row (mirroring
    /// the SQL scan that saw it before the concurrent delete), but
    /// every subsequent `lookup_pending_mut` call returns
    /// [`DomainError::NotFound`].
    ///
    /// # Blast radius
    ///
    /// `lookup_pending_mut` is the shared helper for ALL four
    /// pending-row transitions (`__transition_pending_to_approved_test_only` /
    /// `_cancelled` / `_rejected` / `_expired`), so flagging an id
    /// affects every transition seam — not just expire. Tests
    /// driving a single seam should still seed only the rows they
    /// need; flagging a row that is also in another seam's working
    /// set will produce `NotFound` there too. Useful beyond expire
    /// but easy to misuse.
    ///
    /// # Precedence over `inject_lookup_error`
    ///
    /// `lookup_pending_mut` checks `vanished_ids` BEFORE
    /// `injected_errors`. If the same `request_id` is registered
    /// with both `mark_vanished` and `inject_lookup_error`, the
    /// vanished branch fires first and the injected error is
    /// silently retained (never consumed). Do not mix the two
    /// seams on the same id unless that precedence is exactly what
    /// the test wants.
    pub fn mark_vanished(&self, request_id: Uuid) {
        let mut state = self.inner.lock().expect("lock");
        state.vanished_ids.insert(request_id);
    }

    /// Inject a single-shot per-row error for the next
    /// `lookup_pending_mut` call on `request_id`. The next
    /// transition that routes through that helper consumes the
    /// error and returns it verbatim; subsequent calls observe the
    /// row's actual state. Tests that need persistent failure
    /// re-inject between transition calls.
    ///
    /// Used by escalation-warn boundary tests to drive the
    /// `Err(other)` arm of `expire_pending` (which increments the
    /// local `failed` counter) with a non-`NotFound`,
    /// non-`AlreadyResolved` error shape — exactly the case the
    /// `mark_vanished` seam cannot reach because `NotFound` is
    /// classified as an idempotent skip there.
    ///
    /// # Blast radius
    ///
    /// Same as `mark_vanished` — affects ALL four pending-row
    /// transitions. Same misuse caveat applies.
    ///
    /// # Precedence vs `mark_vanished`
    ///
    /// `mark_vanished` wins if both are set on the same id; see
    /// the precedence note on [`Self::mark_vanished`].
    pub fn inject_lookup_error(&self, request_id: Uuid, error: DomainError) {
        let mut state = self.inner.lock().expect("lock");
        state.injected_errors.insert(request_id, error);
    }

    /// Snapshot the `AccessScope` history captured by the
    /// transition methods (`transition_pending_to_*`). Each entry
    /// is the scope the service passed to the repo on that call,
    /// in chronological order.
    ///
    /// Tests that need to pin the documented `AccessScope::allow_all()`
    /// repo-bypass contract (the entity is `Scopable(no_tenant,
    /// no_resource, no_owner, no_type)` so the repo runs at
    /// `allow_all`) read this and assert `last == allow_all()` or
    /// iterate the full history for multi-call scenarios.
    pub fn captured_scopes(&self) -> Vec<AccessScope> {
        let state = self.inner.lock().expect("lock");
        state.captured_scopes.clone()
    }

    /// Direct row count for assertions; bypasses soft-delete filtering
    /// so tests can assert that `soft_delete_resolved_older_than` left
    /// the row in place but stamped `deleted_at`.
    pub fn snapshot_all(&self) -> Vec<ConversionRequest> {
        self.inner
            .lock()
            .expect("lock")
            .rows
            .values()
            .cloned()
            .collect()
    }

    /// Return the request id associated with a tenant's current pending
    /// row, if any. Reads the derived index directly so tests can pin
    /// the production "one pending per tenant" invariant.
    pub fn pending_request_id_for(&self, tenant_id: Uuid) -> Option<Uuid> {
        self.inner
            .lock()
            .expect("lock")
            .pending_by_tenant
            .get(&tenant_id)
            .copied()
    }
}

/// Materialize a [`ConversionRequest`] from a [`NewConversionRequest`]
/// using the supplied `requested_at`. Mirrors the
/// [`crate::infra::storage::repo_impl::conversion`] insert path:
/// status = `Pending`, every resolved-actor column is `None`,
/// `deleted_at` is `None`.
fn materialize_pending(
    new: &NewConversionRequest,
    requested_at: OffsetDateTime,
) -> ConversionRequest {
    ConversionRequest {
        id: new.id,
        tenant_id: new.tenant_id,
        parent_id: new.parent_id,
        child_tenant_name: new.child_tenant_name.clone(),
        initiator_side: new.initiator_side,
        target_mode: new.target_mode,
        status: ConversionStatus::Pending,
        requested_by: new.requested_by,
        approved_by: None,
        cancelled_by: None,
        rejected_by: None,
        requested_at,
        resolved_at: None,
        expires_at: new.expires_at,
        deleted_at: None,
    }
}

#[async_trait]
impl ConversionRepo for FakeConversionRepo {
    async fn insert_pending(
        &self,
        _scope: &AccessScope,
        new: &NewConversionRequest,
    ) -> Result<ConversionRequest, DomainError> {
        let mut state = self.inner.lock().expect("lock");
        // Mirror the partial-unique-index fence on
        // `ux_conversion_requests_pending`: exactly one pending row per
        // tenant. A resolved or soft-deleted prior row does NOT block
        // a new pending insert because the index excludes those rows.
        if let Some(existing_id) = state.pending_by_tenant.get(&new.tenant_id).copied() {
            return Err(DomainError::PendingExists {
                request_id: existing_id.to_string(),
            });
        }
        // Mirror the PK constraint: caller-supplied duplicate
        // `request_id` is rejected. Hard error here because the
        // service layer is supposed to allocate fresh ids per
        // request — a collision means caller bug, not contention.
        if state.rows.contains_key(&new.id) {
            return Err(DomainError::Internal {
                diagnostic: format!(
                    "fake insert_pending: duplicate request_id {} (caller bug)",
                    new.id
                ),
                cause: None,
            });
        }
        // `requested_at` rides on the input (set by the service's
        // `now_fn`), not on the fake's wall-clock, so unit tests can pin
        // it deterministically and assertions on `requested_at` round-
        // trip exactly.
        let row = materialize_pending(new, new.requested_at);
        state.pending_by_tenant.insert(new.tenant_id, new.id);
        state.rows.insert(new.id, row.clone());
        Ok(row)
    }

    async fn find_by_id(
        &self,
        _scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<ConversionRequest>, DomainError> {
        let state = self.inner.lock().expect("lock");
        Ok(state
            .rows
            .get(&id)
            .filter(|r| r.deleted_at.is_none())
            .cloned())
    }

    async fn find_pending_for_tenant(
        &self,
        _scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Option<ConversionRequest>, DomainError> {
        let state = self.inner.lock().expect("lock");
        Ok(state
            .pending_by_tenant
            .get(&tenant_id)
            .and_then(|id| state.rows.get(id))
            .filter(|r| r.deleted_at.is_none())
            .cloned())
    }

    async fn __transition_pending_to_approved_test_only(
        &self,
        scope: &AccessScope,
        request_id: Uuid,
        approved_by: Uuid,
        resolved_at: OffsetDateTime,
    ) -> Result<ConversionRequest, DomainError> {
        let mut state = self.inner.lock().expect("lock");
        state.captured_scopes.push(scope.clone());
        let row = lookup_pending_mut(&mut state, request_id)?;
        row.status = ConversionStatus::Approved;
        row.approved_by = Some(approved_by);
        row.resolved_at = Some(resolved_at);
        let updated = row.clone();
        state.pending_by_tenant.remove(&updated.tenant_id);
        Ok(updated)
    }

    // @cpt-begin:cpt-cf-account-management-algo-managed-self-managed-modes-dual-consent-apply:p1:inst-algo-dual-consent-apply-fake
    // @cpt-begin:cpt-cf-account-management-dod-managed-self-managed-modes-mixed-mode-tree-consistency:p1:inst-dod-mixed-mode-tree-consistency-fake
    async fn apply_conversion_approval(
        &self,
        scope: &AccessScope,
        input: ApplyConversionApprovalInput,
    ) -> Result<ConversionRequest, DomainError> {
        // Capture under the same `inner` lock as the simpler
        // `transition_pending_to_*` methods so the scope-history
        // contract documented on `State::captured_scopes` covers
        // the approve seam too. Without this push the approve path
        // would be silently absent from `captured_scopes()`,
        // breaking scope-regression detection for
        // `ConversionService::approve`.
        {
            let mut state = self.inner.lock().expect("lock");
            state.captured_scopes.push(scope.clone());
        }
        let tenant_repo = self
            .tenant_repo
            .clone()
            .ok_or_else(|| DomainError::Internal {
                diagnostic: "FakeConversionRepo::apply_conversion_approval invoked without a \
                             cross-fake FakeTenantRepo handle; call \
                             FakeConversionRepo::with_tenant_repo(...) first"
                    .to_owned(),
                cause: None,
            })?;

        // Snapshot the request + tenant + parent BEFORE running any
        // type re-eval so the recheck branches mirror the production
        // SQL impl: missing -> NotFound, non-pending -> AlreadyResolved,
        // non-active -> Validation. Mirrors the in-TX re-load step
        // documented on the trait method.
        let req_snapshot = {
            let state = self.inner.lock().expect("lock");
            state
                .rows
                .get(&input.request_id)
                .filter(|r| r.deleted_at.is_none())
                .cloned()
        };
        let req = req_snapshot.ok_or_else(|| DomainError::ConversionRequestNotFound {
            detail: format!("conversion request {} not found", input.request_id),
            resource: input.request_id.to_string(),
        })?;
        if !matches!(req.status, ConversionStatus::Pending) {
            return Err(DomainError::AlreadyResolved);
        }

        // Cross-check input against reloaded row — mirrors the
        // production `apply_conversion_approval` contract.
        if req.tenant_id != input.target_tenant_id {
            return Err(DomainError::Internal {
                diagnostic: format!(
                    "fake apply_conversion_approval: input.target_tenant_id ({}) does \
                     not match the reloaded request row's tenant_id ({})",
                    input.target_tenant_id, req.tenant_id
                ),
                cause: None,
            });
        }
        if req.target_mode != input.target_mode {
            return Err(DomainError::Internal {
                diagnostic: format!(
                    "fake apply_conversion_approval: input.target_mode ({}) does not \
                     match the reloaded request row's target_mode ({})",
                    input.target_mode.as_str(),
                    req.target_mode.as_str()
                ),
                cause: None,
            });
        }

        let tenant_snapshot = {
            let state = tenant_repo.state.lock().expect("lock");
            state.tenants.get(&input.target_tenant_id).cloned()
        };
        let tenant = tenant_snapshot.ok_or_else(|| DomainError::NotFound {
            detail: format!("tenant {} not found", input.target_tenant_id),
            resource: input.target_tenant_id.to_string(),
        })?;
        if !matches!(tenant.status, TenantStatus::Active) {
            return Err(DomainError::Validation {
                detail: format!(
                    "tenant {} is not active (status={})",
                    tenant.id,
                    tenant.status.as_str()
                ),
            });
        }
        // TOCTOU guard mirroring the production
        // `repo_impl/conversion.rs::apply_conversion_approval`.
        // Type compatibility is enforced by `ConversionService::approve`
        // outside the apply; the fake verifies the snapshot the
        // service observed still matches and surfaces a peer retype
        // as `Validation` so concurrency regressions show up here too.
        if tenant.tenant_type_uuid != input.expected_tenant_type_uuid {
            return Err(DomainError::Validation {
                detail: format!(
                    "tenant {} type changed under TX (expected {}, observed {})",
                    tenant.id, input.expected_tenant_type_uuid, tenant.tenant_type_uuid,
                ),
            });
        }
        let parent_id = req.parent_id.ok_or_else(|| DomainError::Internal {
            diagnostic: format!(
                "conversion {}: parent_id missing on pending row; root-tenant guard \
                 should have rejected this earlier",
                req.id
            ),
            cause: None,
        })?;
        let parent_snapshot = {
            let state = tenant_repo.state.lock().expect("lock");
            state.tenants.get(&parent_id).cloned()
        };
        let parent = parent_snapshot.ok_or_else(|| DomainError::Internal {
            diagnostic: format!(
                "conversion {}: parent tenant {parent_id} disappeared between request and \
                 approve",
                req.id
            ),
            cause: None,
        })?;
        if parent.tenant_type_uuid != input.expected_parent_tenant_type_uuid {
            return Err(DomainError::Validation {
                detail: format!(
                    "parent tenant {} type changed under TX (expected {}, observed {})",
                    parent.id, input.expected_parent_tenant_type_uuid, parent.tenant_type_uuid,
                ),
            });
        }

        // All async guards passed. The remaining work — pre-write
        // re-check + tenant flip + closure rewrite + request stamp —
        // MUST appear atomic to any peer reader (mirrors a single SQL
        // TX in the production impl). Hold BOTH `tenant_repo.state`
        // and `self.inner` for the entire critical section, in that
        // order, and re-validate the state we snapshotted earlier
        // before applying any writes — a peer `cancel` / `reject` /
        // `expire_pending` (or a peer `update_tenant_mutable`) that
        // committed between the snapshot and now MUST surface as
        // `AlreadyResolved` / `Validation` and leave both pieces of
        // state untouched, never produce a half-applied tenant flip
        // with a still-`Pending` request.
        //
        // Lock ordering: `tenant_repo.state` first, then `self.inner`.
        // No other path in this fake takes both — see the lock-site
        // census at the top of the file. `std::sync::Mutex` is held
        // synchronously across this block; the async type-checker
        // (which is the only `.await` in this method) ran above
        // BEFORE acquiring either lock, so we never await with a
        // sync mutex held.
        let new_self_managed = matches!(input.target_mode, TargetMode::SelfManaged);
        let updated_req = {
            let mut tenant_state = tenant_repo.state.lock().expect("lock");
            let mut state = self.inner.lock().expect("lock");

            // Pre-write re-check on the request row. A peer cancel /
            // reject / expire that committed between phase-1 snapshot
            // and now flipped status away from Pending; production's
            // SERIALIZABLE TX would surface this as `AlreadyResolved`
            // at line 556 of `repo_impl/conversion.rs`. Mirror the
            // contract here so concurrency tests against the fake
            // exhibit the same observable behaviour.
            let req_row =
                state
                    .rows
                    .get(&input.request_id)
                    .ok_or_else(|| DomainError::Internal {
                        diagnostic: format!(
                            "conversion {}: row vanished between guards and transition",
                            input.request_id
                        ),
                        cause: None,
                    })?;
            if !matches!(req_row.status, ConversionStatus::Pending) {
                return Err(DomainError::AlreadyResolved);
            }
            if req_row.deleted_at.is_some() {
                return Err(DomainError::AlreadyResolved);
            }

            // Pre-write re-check on the tenant row — symmetrical to
            // the production `tenants.status = Active` re-check at
            // line 634. A peer `update_tenant_mutable` that
            // committed between phase-3 snapshot and now would have
            // flipped the tenant out of `Active`; we MUST NOT apply
            // the closure rewrite on top of a non-active row.
            let tenant_row = tenant_state
                .tenants
                .get(&input.target_tenant_id)
                .ok_or_else(|| DomainError::NotFound {
                    detail: format!("tenant {} not found", input.target_tenant_id),
                    resource: input.target_tenant_id.to_string(),
                })?;
            if !matches!(tenant_row.status, TenantStatus::Active) {
                let observed = tenant_row.status.as_str();
                return Err(DomainError::Validation {
                    detail: format!(
                        "tenant {} is not active (status={observed})",
                        input.target_tenant_id
                    ),
                });
            }

            // Tenant flip + closure barrier rewrite. Same logic as
            // before, just inside the combined lock.
            if let Some(t) = tenant_state.tenants.get_mut(&input.target_tenant_id) {
                t.self_managed = new_self_managed;
            }
            let self_managed_map: HashMap<Uuid, bool> = tenant_state
                .tenants
                .values()
                .map(|t| (t.id, t.self_managed))
                .collect();
            let parent_map: HashMap<Uuid, Option<Uuid>> = tenant_state
                .tenants
                .values()
                .map(|t| (t.id, t.parent_id))
                .collect();
            let target = input.target_tenant_id;
            let mut updated: Vec<ClosureRow> = Vec::with_capacity(tenant_state.closure.len());
            for row in tenant_state.closure.iter().cloned() {
                if row.ancestor_id == row.descendant_id {
                    updated.push(row);
                    continue;
                }
                if !strict_path_crosses(&parent_map, row.ancestor_id, row.descendant_id, target) {
                    updated.push(row);
                    continue;
                }
                let new_barrier = i16::from(strict_path_has_self_managed(
                    &parent_map,
                    &self_managed_map,
                    row.ancestor_id,
                    row.descendant_id,
                ));
                updated.push(ClosureRow {
                    barrier: new_barrier,
                    ..row
                });
            }
            tenant_state.closure = updated;

            // Request transition. `req_row.status == Pending` was just
            // re-validated above under the same lock, so the unconditional
            // stamp here is safe: no peer can race in between the check
            // and the write while we hold `state`.
            let row = state.rows.get_mut(&input.request_id).expect(
                "row presence re-validated above under the same lock -- invariant violation",
            );
            row.status = ConversionStatus::Approved;
            row.approved_by = Some(input.approver_uuid);
            row.resolved_at = Some(input.resolved_at);
            let snap = row.clone();
            state.pending_by_tenant.remove(&snap.tenant_id);
            snap
        };
        Ok(updated_req)
    }
    // @cpt-end:cpt-cf-account-management-dod-managed-self-managed-modes-mixed-mode-tree-consistency:p1:inst-dod-mixed-mode-tree-consistency-fake
    // @cpt-end:cpt-cf-account-management-algo-managed-self-managed-modes-dual-consent-apply:p1:inst-algo-dual-consent-apply-fake

    async fn transition_pending_to_cancelled(
        &self,
        scope: &AccessScope,
        request_id: Uuid,
        cancelled_by: Uuid,
        resolved_at: OffsetDateTime,
    ) -> Result<ConversionRequest, DomainError> {
        let mut state = self.inner.lock().expect("lock");
        state.captured_scopes.push(scope.clone());
        let row = lookup_pending_mut(&mut state, request_id)?;
        row.status = ConversionStatus::Cancelled;
        row.cancelled_by = Some(cancelled_by);
        row.resolved_at = Some(resolved_at);
        let updated = row.clone();
        state.pending_by_tenant.remove(&updated.tenant_id);
        Ok(updated)
    }

    async fn transition_pending_to_rejected(
        &self,
        scope: &AccessScope,
        request_id: Uuid,
        rejected_by: Uuid,
        resolved_at: OffsetDateTime,
    ) -> Result<ConversionRequest, DomainError> {
        let mut state = self.inner.lock().expect("lock");
        state.captured_scopes.push(scope.clone());
        let row = lookup_pending_mut(&mut state, request_id)?;
        row.status = ConversionStatus::Rejected;
        row.rejected_by = Some(rejected_by);
        row.resolved_at = Some(resolved_at);
        let updated = row.clone();
        state.pending_by_tenant.remove(&updated.tenant_id);
        Ok(updated)
    }

    async fn transition_pending_to_expired(
        &self,
        scope: &AccessScope,
        request_id: Uuid,
        resolved_at: OffsetDateTime,
    ) -> Result<ConversionRequest, DomainError> {
        let mut state = self.inner.lock().expect("lock");
        state.captured_scopes.push(scope.clone());
        let row = lookup_pending_mut(&mut state, request_id)?;
        row.status = ConversionStatus::Expired;
        row.resolved_at = Some(resolved_at);
        let updated = row.clone();
        state.pending_by_tenant.remove(&updated.tenant_id);
        Ok(updated)
    }

    async fn list_own_for_tenant(
        &self,
        _scope: &AccessScope,
        tenant_id: Uuid,
        status_filter: Option<ConversionStatus>,
        pagination: ConversionPagination,
    ) -> Result<Vec<ConversionRequest>, DomainError> {
        let state = self.inner.lock().expect("lock");
        let rows = collect_filtered(&state, status_filter, |r| r.tenant_id == tenant_id);
        Ok(paginate(rows, pagination))
    }

    async fn list_inbound_for_parent(
        &self,
        _scope: &AccessScope,
        parent_id: Uuid,
        status_filter: Option<ConversionStatus>,
        pagination: ConversionPagination,
    ) -> Result<Vec<ConversionRequest>, DomainError> {
        let state = self.inner.lock().expect("lock");
        let rows = collect_filtered(&state, status_filter, |r| r.parent_id == Some(parent_id));
        Ok(paginate(rows, pagination))
    }

    async fn count_own_for_tenant(
        &self,
        _scope: &AccessScope,
        tenant_id: Uuid,
        status_filter: Option<ConversionStatus>,
    ) -> Result<u64, DomainError> {
        let state = self.inner.lock().expect("lock");
        let total = collect_filtered(&state, status_filter, |r| r.tenant_id == tenant_id).len();
        Ok(u64::try_from(total).unwrap_or(u64::MAX))
    }

    async fn count_inbound_for_parent(
        &self,
        _scope: &AccessScope,
        parent_id: Uuid,
        status_filter: Option<ConversionStatus>,
    ) -> Result<u64, DomainError> {
        let state = self.inner.lock().expect("lock");
        let total =
            collect_filtered(&state, status_filter, |r| r.parent_id == Some(parent_id)).len();
        Ok(u64::try_from(total).unwrap_or(u64::MAX))
    }

    async fn query_expired(
        &self,
        _scope: &AccessScope,
        cutoff: OffsetDateTime,
        batch_size: u32,
    ) -> Result<Vec<ConversionRequest>, DomainError> {
        let state = self.inner.lock().expect("lock");
        let mut rows: Vec<ConversionRequest> = state
            .rows
            .values()
            .filter(|r| matches!(r.status, ConversionStatus::Pending))
            .filter(|r| r.deleted_at.is_none())
            .filter(|r| r.expires_at <= cutoff)
            .cloned()
            .collect();
        // Stable ordering matches the SQL impl:
        // `ORDER BY expires_at ASC, id ASC`.
        rows.sort_by(|a, b| {
            a.expires_at
                .cmp(&b.expires_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        let take = usize::try_from(batch_size).unwrap_or(usize::MAX);
        rows.truncate(take);
        Ok(rows)
    }

    async fn soft_delete_resolved_older_than(
        &self,
        _scope: &AccessScope,
        cutoff: OffsetDateTime,
        now: OffsetDateTime,
        batch_size: u32,
    ) -> Result<u64, DomainError> {
        let mut state = self.inner.lock().expect("lock");
        // Two-pass: identify candidate ids first (sorted, capped), then
        // mutate. Mirrors the SQL impl which selects-by-cutoff then
        // updates the captured ids.
        let mut candidates: Vec<(Uuid, OffsetDateTime)> = state
            .rows
            .values()
            .filter(|r| !matches!(r.status, ConversionStatus::Pending))
            .filter(|r| r.deleted_at.is_none())
            .filter_map(|r| r.resolved_at.map(|ra| (r.id, ra)))
            // Strict `<` to match the trait contract / repo impl
            // (`resolved_at < cutoff`); using `<=` here would soft-
            // delete a row resolved exactly at the boundary in tests
            // while the real repo retains it, causing edge-case
            // retention assertions to drift between the fake and
            // production paths.
            .filter(|(_, ra)| *ra < cutoff)
            .collect();
        candidates.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        let take = usize::try_from(batch_size).unwrap_or(usize::MAX);
        candidates.truncate(take);
        let mut affected: u64 = 0;
        for (id, _) in candidates {
            if let Some(row) = state.rows.get_mut(&id) {
                row.deleted_at = Some(now);
                affected = affected.saturating_add(1);
            }
        }
        Ok(affected)
    }
}

/// Look up `request_id` and return a mutable reference to it, returning
/// the canonical typed errors for missing / already-resolved rows.
/// Mirrors the SQL impl's `rows_affected == 0` re-read disambiguation.
fn lookup_pending_mut(
    state: &mut State,
    request_id: Uuid,
) -> Result<&mut ConversionRequest, DomainError> {
    // Fault-injection #1: rows flagged as vanished (via
    // `FakeConversionRepo::mark_vanished`) report `NotFound` here so
    // tests can simulate the row being hard-deleted between
    // `query_expired` and `transition_pending_to_expired` without
    // racing a real mutator. Mirrors the production scenario where
    // a parent tenant gets hard-deleted (FK cascade) mid-tick.
    if state.vanished_ids.contains(&request_id) {
        return Err(DomainError::ConversionRequestNotFound {
            detail: format!("conversion request {request_id} not found"),
            resource: request_id.to_string(),
        });
    }
    // Fault-injection #2: per-row arbitrary errors injected via
    // `FakeConversionRepo::inject_lookup_error`. `take`-by-`remove`
    // is single-shot so each `inject_lookup_error` call fires
    // exactly once; tests that need persistent failure re-inject
    // between transition calls. Used by escalation-warn boundary
    // tests to drive the `Err(other)` arm of `expire_pending`
    // (which increments the local `failed` counter) with a
    // non-`NotFound`, non-`AlreadyResolved` error shape.
    if let Some(err) = state.injected_errors.remove(&request_id) {
        return Err(err);
    }
    let row =
        state
            .rows
            .get_mut(&request_id)
            .ok_or_else(|| DomainError::ConversionRequestNotFound {
                detail: format!("conversion request {request_id} not found"),
                resource: request_id.to_string(),
            })?;
    if row.deleted_at.is_some() {
        return Err(DomainError::ConversionRequestNotFound {
            detail: format!("conversion request {request_id} not found (soft-deleted)"),
            resource: request_id.to_string(),
        });
    }
    if !matches!(row.status, ConversionStatus::Pending) {
        return Err(DomainError::AlreadyResolved);
    }
    Ok(row)
}

/// Apply the listing predicate + soft-delete + status filter and sort
/// the survivors using the documented `(requested_at DESC, id ASC)`
/// stable order so cursor re-reads are deterministic.
fn collect_filtered<P>(
    state: &State,
    status_filter: Option<ConversionStatus>,
    predicate: P,
) -> Vec<ConversionRequest>
where
    P: Fn(&ConversionRequest) -> bool,
{
    let mut rows: Vec<ConversionRequest> = state
        .rows
        .values()
        .filter(|r| r.deleted_at.is_none())
        .filter(|r| predicate(r))
        .filter(|r| status_filter.is_none_or(|s| r.status == s))
        .cloned()
        .collect();
    rows.sort_by(|a, b| {
        b.requested_at
            .cmp(&a.requested_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    rows
}

fn paginate(
    rows: Vec<ConversionRequest>,
    pagination: ConversionPagination,
) -> Vec<ConversionRequest> {
    let skip = usize::try_from(pagination.skip).unwrap_or(usize::MAX);
    let take = usize::try_from(pagination.top).unwrap_or(usize::MAX);
    rows.into_iter().skip(skip).take(take).collect()
}

/// Walk the strict `(ancestor, descendant]` path in the parent map and
/// return `true` iff the `target` tenant appears on it. The path
/// excludes the ancestor itself but includes the descendant. Cycle-safe
/// via a depth cap matching the AM hierarchy budget.
fn strict_path_crosses(
    parent_map: &HashMap<Uuid, Option<Uuid>>,
    ancestor: Uuid,
    descendant: Uuid,
    target: Uuid,
) -> bool {
    let mut current = Some(descendant);
    let mut hops = 0_usize;
    while let Some(node) = current {
        if hops > 1024 {
            return false;
        }
        if node == ancestor {
            // Reached the ancestor without finding `target` on the
            // strict path — ancestor is excluded by the rule.
            return false;
        }
        if node == target {
            return true;
        }
        current = parent_map.get(&node).copied().flatten();
        hops += 1;
    }
    false
}

/// Walk the strict `(ancestor, descendant]` path and return `true` iff
/// any tenant on it has `self_managed = true` in the snapshot. Mirrors
/// the canonical barrier rule from DESIGN section 3.1.
fn strict_path_has_self_managed(
    parent_map: &HashMap<Uuid, Option<Uuid>>,
    self_managed_map: &HashMap<Uuid, bool>,
    ancestor: Uuid,
    descendant: Uuid,
) -> bool {
    let mut current = Some(descendant);
    let mut hops = 0_usize;
    while let Some(node) = current {
        if hops > 1024 {
            return false;
        }
        if node == ancestor {
            return false;
        }
        if self_managed_map.get(&node).copied().unwrap_or(false) {
            return true;
        }
        current = parent_map.get(&node).copied().flatten();
        hops += 1;
    }
    false
}
