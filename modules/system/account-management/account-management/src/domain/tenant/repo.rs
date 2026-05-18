//! Tenant repository contract.
//!
//! `TenantRepo` is the sole storage-seam the domain layer touches. It
//! abstracts the SeaORM-backed implementation so `TenantService` can be
//! unit-tested against a pure in-memory fake.
//!
//! Trait-method shape notes:
//!
//! * Every write path that changes closure rows is expressed as a single
//!   repo method that performs the `tenants` + `tenant_closure` writes in
//!   one transaction. The service never opens a transaction itself.
//! * The `activate_tenant` method corresponds to saga step 3 from
//!   DESIGN §3.3 `seq-create-child`: flip the tenant from `provisioning`
//!   to `active` AND insert the closure rows passed by the service.
//! * `compensate_provisioning` is the clean-failure compensation path;
//!   closure cleanup is not required because no closure rows are ever
//!   written while the tenant is in `provisioning`.
//! * `update_tenant_mutable` only accepts the patchable fields (name +
//!   status) and rewrites `tenant_closure.descendant_status` atomically
//!   when `status` changes.

use std::time::Duration;

use async_trait::async_trait;
use modkit_security::AccessScope;
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use account_management_sdk::UpdateTenantRequest;
use modkit_odata::{ODataQuery, Page};

use crate::domain::error::DomainError;
use crate::domain::tenant::closure::ClosureRow;
use crate::domain::tenant::integrity::{IntegrityCategory, Violation};
use crate::domain::tenant::model::{ChildCountFilter, NewTenant, TenantModel, TenantStatus};
use crate::domain::tenant::retention::{
    HardDeleteEligibility, HardDeleteOutcome, TenantProvisioningRow, TenantRetentionRow,
};

/// Read / write boundary for the `tenants` + `tenant_closure` tables.
///
/// Every method owns its own short-lived transaction unless the method
/// docs state otherwise. Caller-facing methods accept an [`AccessScope`]
/// parameter that the implementation forwards to `modkit_db`'s secure
/// query builders.
///
/// # Caller contract on `scope`
///
/// Both `tenants` and `tenant_closure` entities are declared
/// `no_tenant, no_resource, no_owner, no_type`. On these declarations
/// `Scopable::IS_UNRESTRICTED` is `false` and every constraint
/// property resolves to `None`, which means:
///
/// * `scope_with(allow_all())` → no-op (no `WHERE` clause added).
/// * `scope_with(<narrowed>)` → `deny_all()` (`WHERE false`) for reads
///   / mutations, and `ScopeError::Denied` for INSERTs.
///
/// # Subtree clamp via `InTenantSubtree`
///
/// The [`InTenantSubtree`](modkit_security::ScopeFilter::in_tenant_subtree)
/// predicate (cyberware-rust#1813) compiles to a JOIN on
/// `tenant_closure`. AM-side, the trait does NOT itself wire that
/// predicate — the entity declares `Scopable(no_tenant, no_resource,
/// no_owner, no_type)` so the scope filter has no resolvable property
/// to clamp on. Authorization on `tenants` is therefore enforced one
/// layer up via:
///
/// * the PDP gate at the service layer (which produces the
///   `InTenantSubtree` constraint the caller embedded in its scope),
/// * and the URL-bound tenant id the REST handler trusts after the
///   `AuthN` layer verifies the caller is bound to it.
///
/// For background callers (retention reaper, integrity check, etc.)
/// that operate as `actor=system`, [`AccessScope::allow_all`] is the
/// correct posture and the trait calls forward it through to the
/// storage layer unchanged. Caller-driven reads of paginated children
/// (`list_children`) already engage `InTenantSubtree` through the
/// closure-table JOIN built by the secure-ORM when the caller passes
/// a narrowed scope; the `tenants` entity itself stays
/// scope-property-less.
#[async_trait]
pub trait TenantRepo: Send + Sync {
    // ---- Read operations -----------------------------------------------

    /// Load a single tenant by id, including SDK-invisible `Provisioning`
    /// rows (so the service can distinguish "not-found" from "not-visible").
    ///
    /// Returns `Ok(None)` when no row exists or the row is outside the
    /// supplied `scope`.
    async fn find_by_id(
        &self,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<TenantModel>, DomainError>;

    /// Batch sibling of [`Self::find_by_id`]: return every row whose id
    /// is in `ids` and that is visible under the supplied `scope`. The
    /// caller-supplied id slice is deduplicated by the implementation;
    /// missing ids do not surface as errors. Order of the returned
    /// vector is unspecified — callers that need a positional mapping
    /// MUST build a `HashMap<Uuid, TenantModel>` from the result. Used
    /// by listings that resolve cross-row metadata (e.g. the
    /// conversion parent listing's live `child_tenant_name` lookup) so
    /// they avoid an N+1 round-trip pattern.
    ///
    /// # Soft-delete semantics — DELIBERATE asymmetry vs. `find_by_id`
    ///
    /// `find_many` returns only live rows (`deleted_at IS NULL`);
    /// `find_by_id` does not filter by deletion. This is intentional:
    /// `find_by_id`
    /// is consumed by paths that need to disambiguate `NotFound` from
    /// `Found-but-Deleted` (e.g. integrity check, conversion approve's
    /// status precondition), while `find_many` is consumed by cross-
    /// row metadata listings where surfacing a deleted tenant's name
    /// would leak post-deletion state across a barrier. Callers that
    /// need both behaviours should consult the trait method whose
    /// docstring matches their semantics — do not paper over the
    /// difference at the call site.
    ///
    /// # Batch-size ceiling
    ///
    /// The implementation lowers `ids` into a single SQL `IN (...)`
    /// clause, which costs one bind parameter per id. Postgres caps
    /// prepared-statement parameters at 65535, so callers MUST cap the
    /// caller-supplied slice well below that limit (`SQLite` is
    /// effectively unbounded but pays the same per-id round-trip cost
    /// at very large fan-outs). Today every caller bounds its slice
    /// from a paginated upstream listing whose `$top` is small (under
    /// the module-config `listing.max_top` ceiling enforced by
    /// `paginate_odata`'s `LimitCfg`), so the ceiling is implicit;
    /// new callers MUST keep this invariant.
    async fn find_many(
        &self,
        scope: &AccessScope,
        ids: &[Uuid],
    ) -> Result<Vec<TenantModel>, DomainError>;

    /// Direct-children list, paginated through `paginate_odata`.
    /// `parent_id` is the path-scoped parent (always `AND`-ed with
    /// `tenants.parent_id`); `query` carries `$filter`, `$orderby`,
    /// `$top`, `$cursor` over the SDK-declared
    /// [`account_management_sdk::TenantInfoFilterField`] surface.
    ///
    /// `Provisioning` rows are excluded at the query layer. When
    /// `query.filter` does NOT reference the `status` column, the
    /// implementation `AND`-s `status IN (Active, Suspended)` so soft-
    /// deleted rows stay hidden by default — callers wanting them pass
    /// `$filter=status eq 'deleted'` explicitly (string form matching
    /// the [`account_management_sdk::TenantStatus`] serde rename).
    ///
    /// Ordering defaults to `created_at ASC` (the cursor tiebreaker)
    /// when `$orderby` is absent, keeping cursor re-reads stable.
    async fn list_children(
        &self,
        scope: &AccessScope,
        parent_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<TenantModel>, DomainError>;

    // ---- Write operations ----------------------------------------------

    /// Saga step 1: insert a new tenant row with `status = Provisioning`.
    ///
    /// Runs in its own short TX. The implementation MUST re-read the
    /// parent row in the same TX and reject the insert unless the
    /// parent is still `Active`; otherwise a concurrent soft-delete
    /// could commit a deleted parent while a new child is being
    /// provisioned. No closure rows are written — the
    /// provisioning-exclusion invariant (DESIGN §3.1) forbids any
    /// closure entry while the tenant is in `provisioning`.
    async fn insert_provisioning(
        &self,
        scope: &AccessScope,
        tenant: &NewTenant,
    ) -> Result<TenantModel, DomainError>;

    /// Saga step 3: flip the tenant from `Provisioning` to `Active`,
    /// insert the supplied closure rows, and persist the optional
    /// plugin-private metadata blob in one transaction.
    ///
    /// The `closure_rows` slice MUST contain the self-row plus one row per
    /// strict ancestor along the `parent_id` chain (built by
    /// [`crate::domain::tenant::closure::build_activation_rows`]). Any
    /// other composition violates the coverage / self-row invariants.
    ///
    /// `idp_metadata` is the opaque blob returned by the `IdP` plugin
    /// from [`account_management_sdk::IdpProvisionResult::metadata`]; AM
    /// upserts it into `tenant_idp_metadata` and replays it on every
    /// subsequent `IdP` call via [`Self::find_idp_metadata`] /
    /// [`account_management_sdk::IdpTenantContext::metadata`]. `None`
    /// means the plugin reported no per-tenant state.
    async fn activate_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        closure_rows: &[ClosureRow],
        idp_metadata: Option<&Value>,
    ) -> Result<TenantModel, DomainError>;

    /// Load the plugin-private metadata blob AM persisted at
    /// `activate_tenant` time. Returns `None` when no row exists for
    /// `tenant_id` (plugin returned no state, or the tenant was
    /// provisioned before this column existed) OR when the row's
    /// `metadata` column is SQL NULL.
    ///
    /// AM does NOT interpret the JSON shape; the plugin owns it
    /// end-to-end. Callers forward the value verbatim into
    /// [`account_management_sdk::IdpTenantContext::metadata`] on every
    /// subsequent `IdP` call for this tenant.
    async fn find_idp_metadata(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Option<Value>, DomainError>;

    /// Upsert plugin-private metadata for `tenant_id` outside the
    /// activation SERIALIZABLE TX so the row survives a
    /// `finalize_provisioning` failure mid-saga. Called by the
    /// create-child saga and platform-bootstrap saga immediately after
    /// a successful `provision_tenant` so the provisioning reaper can
    /// rebuild a `IdpDeprovisionTenantRequest` carrying the plugin's
    /// per-tenant state even when no activation TX ever committed.
    ///
    /// `idp_metadata = None` is the documented "plugin owns no per-
    /// tenant state" path — the upsert still writes a row with SQL
    /// NULL so `find_idp_metadata` can later distinguish "never
    /// called" from "called with no payload" (mirrors the in-TX
    /// `activate_tenant` invariant).
    ///
    /// Implementations MUST upsert (`ON CONFLICT (tenant_id) DO
    /// UPDATE`): the create-child saga calls this BEFORE
    /// `activate_tenant`, and the activation path performs its own
    /// idempotent metadata write inside the SERIALIZABLE TX. A bare
    /// INSERT would crash on the unique-primary-key constraint when
    /// activation re-runs against an already-persisted row.
    async fn upsert_idp_metadata(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        idp_metadata: Option<&Value>,
    ) -> Result<(), DomainError>;

    /// Saga / reaper compensation: delete a `Provisioning` row that
    /// never reached activation. Guards on `status = Provisioning` to
    /// avoid racing an unrelated row. No closure cleanup is required.
    ///
    /// `expected_claimed_by` is the claim-fence selector and MUST be
    /// honored by the implementation:
    ///
    /// * `Some(worker_id)` — reaper-compensation path. The DELETE MUST
    ///   filter on `claimed_by = worker_id` so a peer reaper that
    ///   re-claimed the row after a `RETENTION_CLAIM_TTL`-busting
    ///   `IdP` round-trip does not get its work erased by this worker.
    /// * `None` — saga-compensation path (`create_tenant` after
    ///   `IdP` `CleanFailure` / `UnsupportedOperation`). The DELETE
    ///   MUST filter on `claimed_by IS NULL` so a reaper that already
    ///   claimed the row mid-IdP-call retains exclusive ownership of
    ///   the compensation work.
    ///
    /// Implementations MUST also fence on `terminal_failure_at IS
    /// NULL` so a peer that already classified the row as
    /// `IdpDeprovisionFailure::Terminal` and parked it for operator
    /// action is not silently erased.
    ///
    /// On a fence-mismatch the implementation MUST return
    /// [`DomainError::Conflict`] (not silently `Ok`) so the caller
    /// can route the row to its `compensate_failed` / lost-claim
    /// metric instead of treating the cleanup as successful.
    async fn compensate_provisioning(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        expected_claimed_by: Option<Uuid>,
    ) -> Result<(), DomainError>;

    /// Apply a mutable-fields-only patch (`name` only — status
    /// transitions go through [`Self::set_status`]).
    ///
    /// # Status-transition guards
    ///
    /// The implementation MUST reject:
    ///
    /// * **Current row in `Deleted`** — already in the deletion
    ///   pipeline; further mutation is forbidden. Returns
    ///   [`DomainError::Conflict`].
    /// * **Current row in `Provisioning`** — saga step 3 hasn't
    ///   activated the tenant; mutable patches are not part of the
    ///   activation contract. Returns [`DomainError::Conflict`].
    ///
    /// The current-row checks run after every SERIALIZABLE retry so
    /// a soft-delete committing between the original attempt and the
    /// retry cannot resurrect the row through a mutable patch.
    async fn update_tenant_mutable(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        patch: &UpdateTenantRequest,
    ) -> Result<TenantModel, DomainError>;

    /// Flip the row between `Active` and `Suspended`. Rewrites
    /// `tenant_closure.descendant_status` for every row where
    /// `descendant_id = tenant_id` in the same SERIALIZABLE
    /// transaction per DESIGN §3.1 *Closure status denormalization
    /// invariant*.
    ///
    /// Same-to-same (`new_status == current.status`) is an idempotent
    /// no-op: the implementation returns the current row without
    /// emitting an UPDATE, leaving `updated_at` unchanged.
    ///
    /// The implementation MUST reject (under SERIALIZABLE retry):
    ///
    /// * **Current row in `Deleted`** — terminal lifecycle state.
    ///   Returns [`DomainError::Conflict`].
    /// * **Current row in `Provisioning`** — saga has not activated
    ///   the tenant. Returns [`DomainError::Conflict`].
    /// * **`new_status` is `Deleted` or `Provisioning`** — both are
    ///   reachable only through the dedicated soft-delete /
    ///   provisioning flows. Returns [`DomainError::Conflict`].
    async fn set_status(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        new_status: TenantStatus,
        now: OffsetDateTime,
    ) -> Result<TenantModel, DomainError>;

    /// Return the closure-input ancestor chain for a new child whose
    /// parent is `parent_id`: `[parent_id, grandparent, ..., root]` in
    /// nearest-first order. The chain **includes `parent_id` itself**
    /// because `build_activation_rows` requires one closure row per
    /// `(ancestor, child)` pair, and `(parent_id, child)` is one of
    /// those pairs.
    ///
    /// The function is named "through parent" rather than "of parent"
    /// to spell out that the seed is part of the returned chain — the
    /// usual graph-theory interpretation of "strict ancestors" would
    /// exclude it.
    async fn load_ancestor_chain_through_parent(
        &self,
        scope: &AccessScope,
        parent_id: Uuid,
    ) -> Result<Vec<TenantModel>, DomainError>;

    // ---- Phase 3: retention + reaper + hard-delete --------------------

    /// Scan retention-due rows for the hard-delete pipeline.
    async fn scan_retention_due(
        &self,
        scope: &AccessScope,
        now: OffsetDateTime,
        default_retention: Duration,
        limit: usize,
    ) -> Result<Vec<TenantRetentionRow>, DomainError>;

    /// Clear a hard-delete scanner claim for a row that was not reclaimed.
    async fn clear_retention_claim(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        worker_id: Uuid,
    ) -> Result<(), DomainError>;

    /// Scan rows in `status = Provisioning` with `created_at <=
    /// older_than` AND atomically claim them for the calling worker.
    /// Used by the provisioning reaper; mirrors the
    /// `scan_retention_due` claim pattern so two replicas cannot
    /// invoke `IdpPluginClient::deprovision_tenant` for the
    /// same row inside one `RETENTION_CLAIM_TTL` window.
    ///
    /// `now` is used to compute the stale-claim cutoff so a worker
    /// that crashed mid-process eventually releases its rows for
    /// peer takeover.
    ///
    /// Returned rows carry the worker UUID stamped during the claim
    /// UPDATE; callers MUST pass it back into
    /// [`Self::clear_retention_claim`] after per-row processing.
    async fn scan_stuck_provisioning(
        &self,
        scope: &AccessScope,
        now: OffsetDateTime,
        older_than: OffsetDateTime,
        limit: usize,
    ) -> Result<Vec<TenantProvisioningRow>, DomainError>;

    /// Count direct children under `parent_id`.
    ///
    /// See [`ChildCountFilter`] for the variant semantics.
    /// `Provisioning` children are *deliberately* counted in both
    /// modes.
    async fn count_children(
        &self,
        scope: &AccessScope,
        parent_id: Uuid,
        filter: ChildCountFilter,
    ) -> Result<u64, DomainError>;

    /// Flip the tenant from its current SDK-visible state to
    /// `Deleted`, stamp `deleted_at = now` (which also starts the
    /// retention timer — eligibility for hard-delete becomes
    /// `deleted_at + retention_window`), and rewrite
    /// `tenant_closure.descendant_status` in the same transaction.
    ///
    /// **Idempotent.** Calling on a row that is already in `Deleted`
    /// status returns the existing tombstone without re-stamping
    /// `deleted_at` (the retention deadline is preserved) and without
    /// re-running the closure rewrite. The contract is enforced under
    /// SERIALIZABLE isolation so two racing DELETEs on the same
    /// `Active` row both terminate with the tombstone — the loser's
    /// retry observes the post-flip state and returns `Ok` instead of
    /// `Conflict`. `Provisioning` rows are rejected with `Conflict`:
    /// they are the reaper's responsibility, not soft-delete.
    async fn schedule_deletion(
        &self,
        scope: &AccessScope,
        id: Uuid,
        now: OffsetDateTime,
        retention: Option<Duration>,
    ) -> Result<TenantModel, DomainError>;

    /// Read-only preflight that verifies a row is eligible for
    /// `hard_delete_one` BEFORE the retention pipeline runs any
    /// external (cascade-hook / IdP-deprovision) side effect.
    ///
    /// Without this gate, a row that is in fact deferred (e.g. parent
    /// with a live child, status drifted, claim lost) would still
    /// trigger cascade hooks and an irreversible
    /// `IdpPluginClient::deprovision_tenant` call before
    /// `hard_delete_one` returned its non-`Cleaned` outcome — leaving
    /// `IdP`-side state torn down while AM keeps the row.
    ///
    /// The check is intentionally **read-only** (no row-lock). A
    /// concurrent peer can theoretically change the state between
    /// preflight and `hard_delete_one`, but in well-formed
    /// deployments this is unreachable: `schedule_deletion` rejects
    /// soft-delete on parents with live children under SERIALIZABLE,
    /// and `create_tenant` rejects under a `Deleted` parent. The race
    /// is observable only from legacy/corrupt state. If it does fire,
    /// `hard_delete_one`'s in-tx defense-in-depth still rejects, and
    /// the retention pipeline retries on the next tick — by which
    /// point the `IdP` plugin maps a re-call to
    /// `IdpDeprovisionFailure::NotFound`, which the retention loop
    /// classifies as success-equivalent and continues with the local
    /// teardown. (For the precise outcome label and the way the
    /// loop folds `NotFound` and `UnsupportedOperation` into the
    /// `is_cleaned`-bearing buckets, see the `process_single_hard_delete`
    /// state machine in `service/retention.rs`.)
    ///
    /// Implementations MUST verify:
    /// * row exists, `status == Deleted`, `deleted_at` stamped (else
    ///   `NotEligible`);
    /// * `claimed_by == Some(claimed_by)` (else `NotEligible` — claim
    ///   was lost between scan and finalize);
    /// * no row in `tenants` names `id` as parent (else
    ///   `DeferredChildPresent` — leaf-first scheduling will pick the
    ///   child first on a subsequent tick).
    async fn check_hard_delete_eligibility(
        &self,
        scope: &AccessScope,
        id: Uuid,
        claimed_by: Uuid,
    ) -> Result<HardDeleteEligibility, DomainError>;

    /// Transactional hard-delete of a single tenant, fenced by the
    /// caller's `claimed_by` worker token. The implementation MUST
    /// re-check `tenants.claimed_by == Some(claimed_by)` inside the
    /// SERIALIZABLE transaction; if the claim was lost (peer reaper
    /// took over after `RETENTION_CLAIM_TTL` expired during this
    /// worker's hooks/IdP window), the method MUST return
    /// [`HardDeleteOutcome::NotEligible`] without writing. This
    /// closes the duplicate-cascade-hooks / duplicate-`IdP`-deprovision
    /// race that opens whenever the hooks + `IdP` step exceeds the
    /// claim TTL.
    async fn hard_delete_one(
        &self,
        scope: &AccessScope,
        id: Uuid,
        claimed_by: Uuid,
    ) -> Result<HardDeleteOutcome, DomainError>;

    /// Stamp `terminal_failure_at = now` on a `Provisioning` row that
    /// the `IdP` plugin has classified as
    /// [`account_management_sdk::IdpDeprovisionFailure::Terminal`]. The
    /// SDK contract treats this as non-recoverable and operator-
    /// action-required; the marker keeps the row out of the
    /// `scan_stuck_provisioning` retry loop until an operator
    /// intervenes (manual hard-delete or column clear).
    ///
    /// The implementation **MUST** fence the UPDATE on both the
    /// `claimed_by` worker token and `status = Provisioning` so a
    /// peer's claim or a parallel finalizer that flipped the row
    /// to `Active` cannot have its work overridden by the marker.
    /// Returns `true` iff the row was actually marked; `false`
    /// indicates the claim was lost or the row no longer matches the
    /// fence (caller treats as no-op for idempotency — the row will
    /// either be marked by whoever still holds the claim, or has
    /// already moved past Provisioning).
    async fn mark_provisioning_terminal_failure(
        &self,
        scope: &AccessScope,
        id: Uuid,
        claimed_by: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError>;

    /// Stamp `terminal_failure_at = now` on a `Deleted` row whose
    /// retention-pipeline cleanup the service classified as
    /// non-recoverable: a `HookError::Terminal` (or panicking) cascade
    /// hook, or a `IdpDeprovisionFailure::Terminal` from the `IdP`
    /// plugin during `hard_delete_batch`. Symmetric to
    /// [`Self::mark_provisioning_terminal_failure`] for the
    /// reaper-side `Provisioning` path; the marker keeps the row out
    /// of the `scan_retention_due` retry loop until an operator
    /// intervenes (manual hard-delete or `terminal_failure_at`
    /// clear).
    ///
    /// The implementation **MUST** fence the UPDATE on
    /// `claimed_by`, `status = Deleted`, and
    /// `terminal_failure_at IS NULL` so a peer's claim or a
    /// concurrent finalizer / re-mark cannot have its work
    /// overridden. Returns `true` iff the row was actually marked;
    /// `false` indicates the claim was lost or the row no longer
    /// matches the fence (caller treats as no-op for idempotency —
    /// the row is either being marked by the live claim holder or
    /// has already been parked / hard-deleted).
    async fn mark_retention_terminal_failure(
        &self,
        scope: &AccessScope,
        id: Uuid,
        claimed_by: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError>;

    /// Return `true` iff a `tenant_closure` row exists with
    /// `ancestor_id = ancestor` and `descendant_id = descendant`.
    async fn is_descendant(
        &self,
        scope: &AccessScope,
        ancestor: Uuid,
        descendant: Uuid,
    ) -> Result<bool, DomainError>;

    // ---- Hierarchy-integrity check -----------------------------------

    /// Run the Rust-side hierarchy-integrity check and return the flat
    /// list of violations the classifier pipeline produced.
    ///
    /// The implementation runs a **three-transaction lifecycle** (see
    /// `crate::infra::storage::integrity::lock`): a short committed
    /// `acquire` transaction inserts the singleton `integrity_check_runs`
    /// gate row (so concurrent contenders see it immediately and
    /// surface [`DomainError::IntegrityCheckInProgress`] instead of
    /// queueing on an uncommitted PK); a separate `REPEATABLE READ`
    /// snapshot transaction loads `tenants` + `tenant_closure` (no
    /// writes — purely read-only); and a final committed `release`
    /// transaction deletes the gate row. The snapshot tx is therefore
    /// safe under concurrent SI conflicts because it never writes.
    ///
    /// The returned `Vec<(IntegrityCategory, Violation)>` is the flat
    /// shape pinned by this trait; the service layer rebuckets it into
    /// an [`crate::domain::tenant::integrity::IntegrityReport`] before
    /// emitting per-category metrics.
    ///
    /// # Errors
    ///
    /// * [`DomainError::IntegrityCheckInProgress`] when another worker
    ///   holds the gate.
    /// * Any DB error from the snapshot SELECTs or the gate INSERT/DELETE,
    ///   funnelled through the canonical
    ///   [`From<modkit_db::DbError> for DomainError`] ladder.
    async fn run_integrity_check(
        &self,
        scope: &AccessScope,
    ) -> Result<Vec<(IntegrityCategory, Violation)>, DomainError>;

    /// Repair the derivable closure violations observable for `scope`
    /// and return per-category counts of rows touched.
    ///
    /// "Derivable" categories are those whose correct closure shape
    /// is fully determined by `tenants` + the `parent_id` walk
    /// (closure is the denormalisation, `tenants` is authoritative):
    ///
    /// * [`IntegrityCategory::MissingClosureSelfRow`] — INSERT
    ///   `(id, id, 0, status)` for tenants whose self-row is absent.
    /// * [`IntegrityCategory::ClosureCoverageGap`] — INSERT
    ///   `(ancestor, descendant, derived_barrier, status)` for missing
    ///   strict ancestors.
    /// * [`IntegrityCategory::BarrierColumnDivergence`] — UPDATE
    ///   `barrier` to the parent-walk-derived value.
    /// * [`IntegrityCategory::DescendantStatusDivergence`] — UPDATE
    ///   `descendant_status` to `tenants.status` for every row
    ///   pointing at the affected tenant.
    /// * [`IntegrityCategory::StaleClosureRow`] — DELETE rows whose
    ///   ancestry is not derivable from any in-snapshot parent walk
    ///   (missing endpoint OR ancestry not in walk).
    ///
    /// The remaining five categories (`OrphanedChild`,
    /// `BrokenParentReference`, `DepthMismatch`, `Cycle`,
    /// `RootCountAnomaly`) indicate corruption in `tenants` itself
    /// and are surfaced through
    /// [`crate::domain::tenant::integrity::RepairReport::deferred_per_category`]
    /// for operator triage — this method does NOT touch them.
    ///
    /// **Closure-only invariant**: this method NEVER writes to the
    /// `tenants` table. The pure-Rust planner in
    /// `infra/storage/integrity/repair.rs` operates over a read-only
    /// snapshot of `tenants` and emits writes targeted exclusively at
    /// `tenant_closure`.
    ///
    /// **Single-flight gate sharing**: repair acquires the same
    /// `integrity_check_runs` singleton PK as
    /// [`Self::run_integrity_check`]. Concurrent check-and-repair is
    /// meaningless (the repair would invalidate any check report
    /// produced in parallel), so they serialise on the same gate; a
    /// contender on either side surfaces
    /// [`DomainError::IntegrityCheckInProgress`].
    ///
    /// **Idempotency**: a clean snapshot returns
    /// [`RepairReport::empty`](crate::domain::tenant::integrity::RepairReport::empty)
    /// with every per-category count zero. Re-running repair on the
    /// post-repair state is a no-op — exercised by the planner-side
    /// `repair_then_repair_is_noop` test.
    ///
    /// Runs in one `SERIALIZABLE` transaction with retry — the
    /// post-snapshot apply pass MUST observe the same MVCC view the
    /// planner derived its corrections from, so concurrent saga
    /// writes that race the apply boundary surface as 40001 abort and
    /// re-plan on retry rather than producing a stale-write conflict.
    ///
    /// # Errors
    ///
    /// * [`DomainError::IntegrityCheckInProgress`] when another
    ///   worker holds the gate.
    /// * Any DB error from the snapshot SELECTs or the apply DML,
    ///   funnelled through the canonical
    ///   [`From<modkit_db::DbError> for DomainError`] ladder.
    async fn repair_derivable_closure_violations(
        &self,
        scope: &AccessScope,
    ) -> Result<crate::domain::tenant::integrity::RepairReport, DomainError>;

    // ---- Convenience helpers used by the service ----------------------

    /// Return `true` iff the tenant exists and its status is `Active`.
    async fn parent_is_active(
        &self,
        scope: &AccessScope,
        parent_id: Uuid,
    ) -> Result<bool, DomainError> {
        match self.find_by_id(scope, parent_id).await? {
            Some(t) => Ok(matches!(t.status, TenantStatus::Active)),
            None => Ok(false),
        }
    }
}
