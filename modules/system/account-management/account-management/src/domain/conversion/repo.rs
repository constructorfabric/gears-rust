//! `ConversionRequest` repository contract.
//!
//! [`ConversionRepo`] is the sole storage seam the conversion domain
//! layer touches. It abstracts the `SeaORM`-backed implementation
//! (`crate::infra::storage::repo_impl::conversion`) so
//! [`crate::domain::conversion::service::ConversionService`] can be
//! unit-tested against a pure in-memory fake
//! (`crate::domain::conversion::test_support::FakeConversionRepo`).
//!
//! Trait-method shape notes:
//!
//! * Every write path that participates in a larger saga (closure
//!   re-materialization on approval, `tenants.self_managed` flip) is
//!   expressed as a single repo method that performs only its
//!   `conversion_requests` write. Saga-level wiring (TX boundary, closure
//!   updates, audit emission) lives in the service layer that arrives
//!   later in this plan.
//! * The `transition_pending_to_*` methods perform an atomic guarded
//!   `UPDATE` that fences on `status = 0 AND deleted_at IS NULL`. On
//!   zero rows updated the implementation re-reads the current row to
//!   distinguish [`DomainError::NotFound`] from
//!   [`DomainError::AlreadyResolved`].
//! * `query_expired` and `soft_delete_resolved_older_than` carry the
//!   background reaper / retention contract; both are batched and
//!   tolerate empty result sets.

use async_trait::async_trait;
use modkit_security::AccessScope;
use time::OffsetDateTime;
use uuid::Uuid;

use modkit_macros::domain_model;

use crate::domain::conversion::model::{
    ConversionPagination, ConversionRequest, ConversionStatus, NewConversionRequest, TargetMode,
};
use crate::domain::error::DomainError;

// @cpt-begin:cpt-cf-account-management-algo-managed-self-managed-modes-dual-consent-apply:p1:inst-algo-dual-consent-apply-input
/// Repo-level input to [`ConversionRepo::apply_conversion_approval`].
///
/// Carries the minimum the repo needs to recompose the apply
/// transaction without re-resolving anything from the service layer:
/// the request id under approve, the converting tenant's id and the
/// target mode (both pulled from the pending row by the service before
/// dispatching), the approver UUID stamped on the resolved row, and
/// the `tenant_type_uuid` values the service already observed on the
/// converting tenant and its parent when it ran the pre-apply type
/// compatibility check.
///
/// The repo re-loads the pending row, the tenant snapshot, and the
/// affected closure rows from the same transaction — the values
/// passed in via this input are NOT trusted and only narrow the
/// re-load. The `expected_*_tenant_type_uuid` pair serves a different
/// role: it is the TX-side TOCTOU guard for the type compatibility
/// check the service ran outside the TX. The repo verifies the
/// reloaded `tenants.tenant_type_uuid` for both endpoints still
/// matches what the service saw and aborts with
/// [`DomainError::Validation`] otherwise. The single seam is
/// documented on the trait method itself.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct ApplyConversionApprovalInput {
    pub request_id: Uuid,
    pub target_tenant_id: Uuid,
    pub target_mode: TargetMode,
    /// `tenant_type_uuid` of the converting tenant as observed by the
    /// service when it ran the pre-apply
    /// [`crate::domain::tenant_type::TenantTypeChecker::check_parent_child`]
    /// barrier. The repo MUST verify the reloaded
    /// `tenants.tenant_type_uuid` still matches this value inside the
    /// apply TX and abort with [`DomainError::Validation`] otherwise.
    /// A peer that flipped this tenant's type between the service's
    /// check and the TX would otherwise leave the apply running
    /// against a stale pairing; surfacing the race as `Validation`
    /// keeps the conversion request recoverable instead of approving
    /// a now-incompatible parent / child pairing.
    pub expected_tenant_type_uuid: Uuid,
    /// `tenant_type_uuid` of the parent tenant as observed by the
    /// service. Same TOCTOU guard semantics as
    /// `expected_tenant_type_uuid` — both endpoints' types must be
    /// stable between the service's check and the apply TX.
    pub expected_parent_tenant_type_uuid: Uuid,
    /// Counterparty actor UUID stamped on the approved row.
    ///
    /// TODO(cyberfabric-core#1813-followup): when the conversion REST
    /// surface lands, the handler MUST source this from
    /// `SecurityContext::actor_uuid()` (the platform-AuthN-validated
    /// caller identity). The repo intentionally does NOT cross-check
    /// this UUID against a registry — without an external actor-type
    /// registry the repo cannot independently verify the value, so a
    /// buggy handler passing the wrong UUID would persist a wrong
    /// `approved_by` actor permanently. This is a service / REST-layer
    /// concern, not a repo concern; the repo trusts what the service
    /// supplies after the dual-consent role guard runs.
    pub approver_uuid: Uuid,
    pub resolved_at: OffsetDateTime,
}
// @cpt-end:cpt-cf-account-management-algo-managed-self-managed-modes-dual-consent-apply:p1:inst-algo-dual-consent-apply-input

/// Read / write boundary for the `conversion_requests` table.
///
/// Every method owns its own short-lived transaction unless the method
/// docs state otherwise. Caller-facing methods accept an [`AccessScope`]
/// parameter that the implementation forwards to `modkit_db`'s secure
/// query builders.
///
/// # Caller contract on `scope`
///
/// The `conversion_requests` entity is declared
/// `Scopable(tenant_col = "tenant_id", resource_col = "id", no_owner,
/// no_type)`, so `OWNER_TENANT_ID` resolves to the `tenant_id` column
/// and `RESOURCE_ID` resolves to the request's own `id`. The secure-
/// extension layer compiles a narrowed scope of shape
/// `InTenantSubtree(OWNER_TENANT_ID, root, respect_barriers = b)` into
/// `tenant_id IN (SELECT descendant_id FROM tenant_closure
///   WHERE ancestor_id = :root [AND barrier = 0 if b])`; the analogous
/// shape on `RESOURCE_ID` clamps `conversion_requests.id` for future
/// identity-based policies the REST PR may emit.
///
/// **Service-side posture:** the conversion service builds a caller-
/// bound scope per URL-bound side (see
/// [`super::service::ConversionService`] internals):
///
/// * Child-side caller: `AccessScope::for_tenant(child_id)` — clamps
///   `tenant_id = child_id`.
/// * Parent-side caller / parent-side inbound listings:
///   `InTenantSubtree(OWNER_TENANT_ID, parent_id, respect_barriers =
///   false)` — clamps `tenant_id` to the parent's closure descendants
///   with barrier penetration so parent-side counterparty actions on a
///   self-managed child stay visible (the dual-consent flows are the
///   case where barrier penetration is correct: the request lives under
///   the parent's URL authority even when the converting child is
///   invisible to a barrier-respecting tenant scope).
///
/// Cross-tenant authorization is layered: the URL-coherence gate
/// [`super::service::ConversionCaller`] runs at the service-layer
/// boundary, `tenant_repo.find_by_id(scope, caller_owned_tenant)`
/// fences the caller-owned tenant under the incoming `AccessScope`,
/// and the repo-level row clamp documented above adds the third line
/// of defence. INSERTs always use `scope_unchecked` — Scopable
/// INSERT-time clamps are not the right model for inserts.
#[async_trait]
pub trait ConversionRepo: Send + Sync {
    // ---- Inserts -------------------------------------------------------

    /// Insert a `pending` conversion-request row.
    ///
    /// The implementation MUST translate the partial unique-index
    /// collision on `ux_conversion_requests_pending` into
    /// [`DomainError::PendingExists`] carrying the existing pending
    /// row's id. The id is re-read inside the same TX before returning
    /// so the caller does not have to issue a follow-up `SELECT`.
    async fn insert_pending(
        &self,
        scope: &AccessScope,
        new: &NewConversionRequest,
    ) -> Result<ConversionRequest, DomainError>;

    // ---- Reads ---------------------------------------------------------

    /// Load a single conversion request by id. Returns `Ok(None)` when
    /// the row does not exist or is outside the supplied `scope`.
    async fn find_by_id(
        &self,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<ConversionRequest>, DomainError>;

    /// Load the unique pending request for a tenant, if any. Returns
    /// `Ok(None)` when no pending row exists for the tenant.
    async fn find_pending_for_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Option<ConversionRequest>, DomainError>;

    // ---- Atomic guarded transitions ------------------------------------

    /// **Test-only** atomic guarded `UPDATE`: stamp `status =
    /// approved`, `approved_by`, and `resolved_at`. Filter is
    /// `WHERE id = :request_id AND status = 0 AND deleted_at IS NULL`.
    ///
    /// # Why this method is dangerous outside tests
    ///
    /// Calling this path in production code bypasses the
    /// counterparty type re-evaluation, the `tenants.self_managed`
    /// flip, and the closure-barrier rewrite the dual-consent apply
    /// algorithm requires. A consumer that flips the request row
    /// to `Approved` without those companion writes leaves
    /// `tenants.self_managed` and `tenant_closure.barrier`
    /// inconsistent with the new request status — the integrity
    /// checker would surface the divergence after the fact, but the
    /// user-visible mode would already have lied.
    ///
    /// Production callers MUST use
    /// [`Self::apply_conversion_approval`] (the single-TX seam that
    /// performs every load-bearing write). The leading double
    /// underscore + `_test_only` suffix is a grep-discoverable
    /// signal at every call site that this path is reserved for
    /// the unit-test seams that need to drive the pending-row
    /// transition in isolation; `#[doc(hidden)]` keeps it out of
    /// the rendered rustdoc surface.
    ///
    /// On zero rows updated the implementation MUST re-read the
    /// current row to distinguish [`DomainError::NotFound`] from
    /// [`DomainError::AlreadyResolved`].
    #[doc(hidden)]
    async fn __transition_pending_to_approved_test_only(
        &self,
        scope: &AccessScope,
        request_id: Uuid,
        approved_by: Uuid,
        resolved_at: OffsetDateTime,
    ) -> Result<ConversionRequest, DomainError>;

    // @cpt-begin:cpt-cf-account-management-algo-managed-self-managed-modes-dual-consent-apply:p1:inst-algo-dual-consent-apply-trait
    // @cpt-begin:cpt-cf-account-management-dod-managed-self-managed-modes-dual-consent-apply:p1:inst-dod-dual-consent-apply-trait
    /// Atomic dual-consent apply seam. Runs inside ONE database
    /// transaction owned by the repo and performs every load-bearing
    /// write the conversion-approval needs — request reload, side
    /// recheck, status recheck, TOCTOU type-stability guard,
    /// `tenants.self_managed` flip, closure-barrier rewrite for every
    /// affected `tenant_closure` row, and the request transition to
    /// `Approved` (with `approved_by` + `resolved_at`).
    ///
    /// Type compatibility (`allowed_parent_types`) is enforced at the
    /// service layer BEFORE this seam opens its TX — the repo no
    /// longer carries a `TenantTypeChecker` dependency. What runs
    /// inside the TX is the TOCTOU guard: the reloaded
    /// `tenants.tenant_type_uuid` for both endpoints MUST still match
    /// the values the service supplied via
    /// [`ApplyConversionApprovalInput::expected_tenant_type_uuid`] /
    /// [`ApplyConversionApprovalInput::expected_parent_tenant_type_uuid`].
    /// A mismatch surfaces as [`DomainError::Validation`] — the
    /// service caller retries the approve flow after the type re-eval
    /// runs again on fresh tenant rows.
    ///
    /// Order inside the TX is fixed: reload the pending row, reload
    /// both tenant rows and assert their status + type stability,
    /// flip `tenants.self_managed` (with a defence-in-depth
    /// `tenant_type_uuid = expected` predicate in the WHERE clause),
    /// rewrite closure barriers, and finally stamp the request
    /// transition. Any failure aborts the TX so the pending row, the
    /// `tenants.self_managed` value, and every closure barrier remain
    /// unchanged. On success the returned [`ConversionRequest`]
    /// carries the post-transition snapshot.
    ///
    /// # Errors
    ///
    /// * [`DomainError::NotFound`] — `request_id` does not resolve
    ///   to a live conversion-request row.
    /// * [`DomainError::AlreadyResolved`] — the row is no longer
    ///   `Pending`.
    /// * [`DomainError::Validation`] — the converting tenant or the
    ///   parent tenant is no longer `Active`, OR either tenant's
    ///   `tenant_type_uuid` no longer matches the value the service
    ///   observed when it ran the pre-apply type compatibility
    ///   check (TOCTOU guard).
    /// * Any DB error from the surrounding transaction, lifted into
    ///   the canonical `DomainError` via the storage classifier.
    async fn apply_conversion_approval(
        &self,
        scope: &AccessScope,
        input: ApplyConversionApprovalInput,
    ) -> Result<ConversionRequest, DomainError>;
    // @cpt-end:cpt-cf-account-management-dod-managed-self-managed-modes-dual-consent-apply:p1:inst-dod-dual-consent-apply-trait
    // @cpt-end:cpt-cf-account-management-algo-managed-self-managed-modes-dual-consent-apply:p1:inst-algo-dual-consent-apply-trait

    /// Atomic guarded `UPDATE`: stamp `status = cancelled`,
    /// `cancelled_by`, and `resolved_at`. Same fence and same
    /// re-read-on-zero-rows behaviour as
    /// [`Self::__transition_pending_to_approved_test_only`].
    async fn transition_pending_to_cancelled(
        &self,
        scope: &AccessScope,
        request_id: Uuid,
        cancelled_by: Uuid,
        resolved_at: OffsetDateTime,
    ) -> Result<ConversionRequest, DomainError>;

    /// Atomic guarded `UPDATE`: stamp `status = rejected`,
    /// `rejected_by`, and `resolved_at`. Same fence and same
    /// re-read-on-zero-rows behaviour as
    /// [`Self::__transition_pending_to_approved_test_only`].
    async fn transition_pending_to_rejected(
        &self,
        scope: &AccessScope,
        request_id: Uuid,
        rejected_by: Uuid,
        resolved_at: OffsetDateTime,
    ) -> Result<ConversionRequest, DomainError>;

    /// Atomic guarded `UPDATE`: stamp `status = expired` and
    /// `resolved_at`. No `*_by` UUID is stamped because the transition
    /// is system-driven (`actor = system` per the audit envelope).
    /// Same fence and same re-read-on-zero-rows behaviour as
    /// [`Self::__transition_pending_to_approved_test_only`].
    async fn transition_pending_to_expired(
        &self,
        scope: &AccessScope,
        request_id: Uuid,
        resolved_at: OffsetDateTime,
    ) -> Result<ConversionRequest, DomainError>;

    // ---- Listings ------------------------------------------------------

    /// List conversion requests owned by `tenant_id` (the converting
    /// tenant itself). When `status_filter` is `Some`, only rows with
    /// the matching status are returned; soft-deleted rows are always
    /// excluded.
    ///
    /// Order is stable (newest-first by `(requested_at, id)`) so cursor
    /// re-reads are deterministic.
    async fn list_own_for_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        status_filter: Option<ConversionStatus>,
        pagination: ConversionPagination,
    ) -> Result<Vec<ConversionRequest>, DomainError>;

    /// List conversion requests inbound to `parent_id` (the parent of
    /// the converting tenant). When `status_filter` is `Some`, only
    /// rows with the matching status are returned; soft-deleted rows
    /// are always excluded.
    async fn list_inbound_for_parent(
        &self,
        scope: &AccessScope,
        parent_id: Uuid,
        status_filter: Option<ConversionStatus>,
        pagination: ConversionPagination,
    ) -> Result<Vec<ConversionRequest>, DomainError>;

    /// Count of rows that would be returned by
    /// [`Self::list_own_for_tenant`] under the same `(tenant_id,
    /// status_filter)` filter, ignoring pagination. Used by the
    /// service-layer pagination contract so `OffsetPage.total`
    /// reflects the underlying row count and not the current page
    /// size. Soft-deleted rows are excluded by the same predicate.
    async fn count_own_for_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        status_filter: Option<ConversionStatus>,
    ) -> Result<u64, DomainError>;

    /// Count sibling for [`Self::list_inbound_for_parent`]. Same
    /// predicate, no pagination.
    async fn count_inbound_for_parent(
        &self,
        scope: &AccessScope,
        parent_id: Uuid,
        status_filter: Option<ConversionStatus>,
    ) -> Result<u64, DomainError>;

    // ---- Background reaper / retention ---------------------------------

    /// Return up to `batch_size` `pending` rows where `expires_at <=
    /// cutoff AND deleted_at IS NULL`, ordered by `expires_at ASC` for
    /// fair sweep. Used by the conversion-expiry reaper to discover
    /// rows due for the `pending -> expired` transition.
    async fn query_expired(
        &self,
        scope: &AccessScope,
        cutoff: OffsetDateTime,
        batch_size: u32,
    ) -> Result<Vec<ConversionRequest>, DomainError>;

    /// Stamp `deleted_at = :now` on resolved rows where `resolved_at <
    /// :cutoff AND deleted_at IS NULL`. Returns the row count.
    ///
    /// AM does not run a separate hard-delete pass on
    /// `conversion_requests`; the FK on `conversion_requests.tenant_id`
    /// is `ON DELETE CASCADE`, so the existing tenant retention sweep
    /// reclaims the underlying rows when the owning tenant is
    /// hard-deleted. Soft-delete is the only retention step this trait
    /// exposes for `conversion_requests`.
    async fn soft_delete_resolved_older_than(
        &self,
        scope: &AccessScope,
        cutoff: OffsetDateTime,
        now: OffsetDateTime,
        batch_size: u32,
    ) -> Result<u64, DomainError>;
}
