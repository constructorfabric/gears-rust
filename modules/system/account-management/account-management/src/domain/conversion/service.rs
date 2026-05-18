//! `ConversionService` — domain orchestrator for the dual-consent
//! `pending -> {cancelled, rejected, expired, approved}` lifecycle of a
//! [`ConversionRequest`].
//!
//! This phase implements five of the six service methods:
//! `request_conversion`, `cancel`, `reject`, `list_own_for_tenant`,
//! `list_inbound_for_parent`, and `soft_delete_resolved`. The
//! counterparty-side `approve` and the system-side `expire_pending`
//! reaper land in the next phase.
//!
//! The service depends only on the domain-level [`ConversionRepo`] and
//! [`TenantRepo`] traits. It never opens transactions itself — every
//! per-call short-lived TX is owned by the repo method
//! (`insert_pending`, `transition_pending_to_*`, etc.). The service's
//! sole responsibility is to compose guards, project parent-side rows
//! down to the minimal cross-barrier surface, and emit `am.events`
//! tracing for each successful transition.
//!
//! Test seam: a deterministic clock is injected via [`with_now_fn`].
//! Production wiring uses `OffsetDateTime::now_utc` by default; the
//! service-level unit tests pin a fixed instant so `expires_at`,
//! `resolved_at`, and the retention `cutoff` are reproducible.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use crate::domain::conversion::page::OffsetPage;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use modkit_macros::domain_model;
use modkit_security::{
    AccessScope, InTenantSubtreeScopeFilter, ScopeConstraint, ScopeFilter, SecurityContext,
    pep_properties,
};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::domain::conversion::model::{
    ConversionPagination, ConversionRequest, ConversionSide, ConversionStatus,
    NewConversionRequest, TargetMode,
};
use crate::domain::conversion::repo::{ApplyConversionApprovalInput, ConversionRepo};
use crate::domain::conversion::state_machine::validate_transition;
use crate::domain::error::DomainError;
use crate::domain::tenant::model::TenantStatus;
use crate::domain::tenant::repo::TenantRepo;
use crate::domain::tenant_type::TenantTypeChecker;

/// Shared clock seam. Produced by [`ConversionService::new`] from
/// `OffsetDateTime::now_utc` and overridable in tests via
/// [`ConversionService::with_now_fn`].
type NowFn = Arc<dyn Fn() -> OffsetDateTime + Send + Sync>;

/// Caller scope for every conversion-request operation that crosses
/// the dual-consent surface (`request_conversion`, `approve`, `cancel`,
/// `reject`). Carries both the side the caller acts on
/// (`Child` / `Parent`) AND the tenant the caller is authorized for
/// — child-side carries the converting tenant id, parent-side carries
/// the parent tenant id. The service uses these to enforce the
/// caller's URL-bound scope at the boundary so a misrouted call cannot
/// act on a request outside the caller's authority.
///
/// REST handlers MUST construct:
///   * `Self::child(tenant_id)` from the `/tenants/{tenant_id}/conversions` URL parameter
///   * `Self::parent(parent_tenant_id)` from the `/tenants/{parent_tenant_id}/child-conversions` URL parameter
///
/// They MUST NOT trust a client-supplied side label or scope id —
/// these IDs come from the URL path, which the platform `AuthN` layer
/// has already verified the caller is authorized for.
///
/// Today no SDK consumer wires this — the conversion-service handle is
/// published for the upcoming REST PR — so the service-layer contract
/// is the only authorization gate. When `feature-tenant-resolver-plugin`
/// plumbs `InTenantSubtree` (cyberfabric-core#1813), the storage scope
/// will narrow reads to the caller's subtree and this struct's
/// `scope_id` will continue to provide the column-level fence on
/// `request.tenant_id` / `request.parent_id`.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct ConversionCaller {
    side: ConversionSide,
    /// Tenant id the caller is authorized for. For `Child`, this is
    /// the converting tenant; for `Parent`, this is the parent tenant
    /// (i.e. `request.parent_id`). Kept as `Uuid` (not `Option`) since
    /// both sides MUST carry a scope post-codex-R5; the constructors
    /// are the only public path and they always populate it.
    scope_id: Uuid,
}

impl ConversionCaller {
    /// Build a child-side caller scoped to `child_tenant_id`. The
    /// service verifies that the request the caller acts on has a
    /// `tenant_id` matching this value; mismatches are routed
    /// through `require_caller_scope_or_not_found` and surface as
    /// [`DomainError::NotFound`] keyed on the request id so an
    /// out-of-scope caller cannot probe row existence through the
    /// error channel. For `request_conversion`, the service
    /// additionally verifies that `input.tenant_id` matches the
    /// resolved tenant before any state mutation.
    #[must_use]
    pub const fn child(child_tenant_id: Uuid) -> Self {
        Self {
            side: ConversionSide::Child,
            scope_id: child_tenant_id,
        }
    }

    /// Build a parent-side caller scoped to `parent_tenant_id`. The
    /// service verifies that the request the caller acts on has a
    /// `parent_id` matching this value; mismatches are routed
    /// through `require_caller_scope_or_not_found` and surface as
    /// [`DomainError::NotFound`] (see [`Self::child`] for the
    /// existence-leak rationale).
    #[must_use]
    pub const fn parent(parent_tenant_id: Uuid) -> Self {
        Self {
            side: ConversionSide::Parent,
            scope_id: parent_tenant_id,
        }
    }

    /// Lower the caller scope into the discriminator stored on the
    /// conversion-request row.
    #[must_use]
    pub const fn side(self) -> ConversionSide {
        self.side
    }

    /// Read the caller's scope id (child tenant id for `Child`,
    /// parent tenant id for `Parent`). Both variants always carry a
    /// concrete `Uuid` so this is non-`Option`.
    #[must_use]
    pub const fn scope_id(self) -> Uuid {
        self.scope_id
    }
}

/// Service-level input to [`ConversionService::request_conversion`].
///
/// Mirrors the dual-consent contract: the caller declares its scope
/// (`caller`) and may override the target mode the conversion will
/// land on. When `target_mode_override` is `None` the service
/// computes the target as the inverse of the tenant's current
/// `self_managed` flag — `Managed` becomes `SelfManaged` and vice
/// versa, which is the only legal "flip" shape per FEATURE
/// `managed-self-managed-modes` §2.
///
/// The actor UUID recorded on the resulting row is sourced from the
/// caller's [`SecurityContext::subject_id`] inside the service — see
/// the cancel / reject / approve methods, which followed the same
/// migration off explicit `*_by: Uuid` parameters onto the
/// platform-AuthN-validated `SecurityContext`.
#[domain_model]
#[derive(Debug, Clone)]
pub struct RequestConversionInput {
    pub tenant_id: Uuid,
    pub caller: ConversionCaller,
    pub target_mode_override: Option<TargetMode>,
}

/// Three-way status selector consumed by the service-level `list_*`
/// methods.
///
/// The original draft model used `Option<ConversionStatus>` where
/// `None` meant "no filter / show every row". That cannot express the
/// FEATURE-doc UX rule "child-scope listings default to only the
/// actionable Pending rows; resolved history is visible only on
/// explicit opt-in" — `None` collapses both intents to a single token.
/// This enum makes the three intents distinguishable on the wire:
///
/// * [`DefaultPending`] — no explicit status passed by the caller; the
///   service substitutes `Pending` so a child default-listing returns
///   actionable rows only and does not implicitly include history.
/// * [`Any`] — caller actively asked for "show me everything,
///   including resolved history".
/// * [`Status(s)`] — caller pinned a specific lifecycle status.
///
/// Lowering to the repo's `Option<ConversionStatus>` filter:
/// `DefaultPending → Some(Pending)`, `Any → None`,
/// `Status(s) → Some(s)`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConversionStatusSelector {
    /// No explicit status — service substitutes `Pending`.
    DefaultPending,
    /// Explicit "every lifecycle status, including resolved history".
    Any,
    /// Explicit single lifecycle status.
    Status(ConversionStatus),
}

impl ConversionStatusSelector {
    /// Lower this selector to the `Option<ConversionStatus>` token the
    /// repo layer consumes.
    #[must_use]
    pub const fn as_repo_filter(self) -> Option<ConversionStatus> {
        match self {
            Self::DefaultPending => Some(ConversionStatus::Pending),
            Self::Any => None,
            Self::Status(s) => Some(s),
        }
    }
}

/// Marker for how a conversion operation entered the service layer.
///
/// Drives the docstring + audit kind on every reaper / retention emit
/// (`actor_kind = "system"` vs the caller-supplied `actor_uuid`) and
/// keeps the URL-bound and system-driven entry points statically
/// distinct at the call site. The previous shape passed a raw
/// `AccessScope::allow_all()` for both, which obscured the intent — a
/// regression that wired the reaper to a caller-supplied scope, or a
/// REST handler that accidentally invoked a system-only path, would
/// not surface in code review until the scope filter zero-rowed a
/// production request.
///
/// `conversion_requests` is `Scopable(tenant_col = "tenant_id",
/// resource_col = "id", no_owner, no_type)` — system-driven sweeps
/// (`expire_pending` / `soft_delete_resolved`) wrap an
/// [`AccessScope::allow_all`] inner so the reaper / retention paths
/// see every row regardless of the URL-bound subtree clamp the
/// caller-facing surface applies. The discriminator is what binds
/// each call to the right audit envelope; the inner
/// `AccessScope` is forwarded to the repo's `query_expired` /
/// `transition_pending_to_expired` / `soft_delete_resolved_older_than`
/// methods verbatim. Caller-facing seams (`cancel` / `reject` /
/// `approve` / `list_*`) build their own derived `AccessScope` via
/// [`conversion_repo_scope`] / [`own_listing_repo_scope`] /
/// [`parent_inbound_repo_scope`] and do not consume this wrapper.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ConversionScope {
    inner: AccessScope,
    kind: ConversionScopeKind,
}

/// Discriminator on [`ConversionScope`].
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConversionScopeKind {
    /// REST handler constructed this scope from a URL binding plus a
    /// platform-AuthN-validated `AccessScope`. Used by
    /// `request_conversion` / `approve` / `cancel` / `reject` and the
    /// caller-facing `list_*` methods.
    UrlBinding,
    /// System-driven sweep — `expire_pending` reaper or
    /// `soft_delete_resolved` retention. The audit envelope's
    /// `actor_kind = "system"` flows from this discriminator.
    SystemSweep,
}

impl ConversionScope {
    /// Construct a URL-bound conversion scope from a caller-supplied
    /// [`AccessScope`]. The wrapped scope is the
    /// platform-AuthN-validated value produced by the REST handler;
    /// the service forwards it to every `TenantRepo` lookup and to
    /// `verify_caller_scope` at the PDP boundary.
    #[must_use]
    pub const fn from_url_binding(scope: AccessScope) -> Self {
        Self {
            inner: scope,
            kind: ConversionScopeKind::UrlBinding,
        }
    }

    /// Construct a system-driven conversion scope. Reserved for the
    /// reaper / retention background sweeps owned by
    /// [`ConversionService::expire_pending`] and
    /// [`ConversionService::soft_delete_resolved`]. The wrapped scope
    /// is [`AccessScope::allow_all`] so the system sweep can see
    /// every row regardless of any future `InTenantSubtree` scope
    /// columns; the discriminator is what binds the call to the
    /// system-driven audit envelope.
    #[must_use]
    pub fn system_sweep() -> Self {
        Self {
            inner: AccessScope::allow_all(),
            kind: ConversionScopeKind::SystemSweep,
        }
    }

    /// Read-only access to the wrapped [`AccessScope`]. Returned by
    /// reference so the existing repo signatures (`fn ...(scope:
    /// &AccessScope, ...)`) can be invoked without an intermediate
    /// clone.
    #[must_use]
    pub const fn as_access_scope(&self) -> &AccessScope {
        &self.inner
    }

    /// Read-only access to the discriminator. Service-internal code
    /// uses it to debug-assert that a system-driven seam was not
    /// invoked with a URL-bound scope and vice versa.
    #[must_use]
    pub const fn kind(&self) -> ConversionScopeKind {
        self.kind
    }
}

/// Pagination + status-filter shape consumed by the service-level
/// `list_*` methods. Mirrors the
/// `account_management_sdk::ListChildrenQuery` ergonomics so call sites
/// stay symmetric with the tenant CRUD surface, but kept AM-internal
/// here because the conversion REST shapes haven't landed yet.
///
/// Field visibility encodes the `top > 0` invariant AND the "set-once
/// at construction" posture for the whole query: every field is private
/// and constructed only via [`Self::default_pending`] / [`Self::any`] /
/// [`Self::with_status`] (each fallible) and read via the
/// [`Self::top`] / [`Self::skip`] / [`Self::status_filter`] accessors.
/// Mirrors the `IdpUserPagination` `TopMustBePositive` posture: a `top = 0`
/// listing collapses to an empty page even when the underlying filter
/// matches rows, which silently breaks any caller that uses pagination
/// to drive existence checks. Keeping `skip` and `status_filter`
/// private prevents an external `let mut q = ...; q.skip = 1_000_000;`
/// from mutating the value after construction and leaving the two
/// fields semantically inconsistent with the (already-private) `top`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListConversionsQuery {
    top: u32,
    skip: u32,
    status_filter: ConversionStatusSelector,
}

/// Validation errors reported by [`ListConversionsQuery`] constructors.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ListConversionsQueryError {
    /// `top` was zero; the listing contract treats `top` as a strict
    /// positive page size so a paginated read cannot silently collapse
    /// to an empty page.
    TopMustBePositive,
    /// `top` exceeded [`ListConversionsQuery::MAX_TOP`]. Mirrors the
    /// `IdpUserPagination::TopExceedsMax` ceiling so a misbehaving caller
    /// forwarding `top = u32::MAX` cannot exhaust the page-buffer
    /// allocation by widening a single listing past the documented
    /// per-page row-count cap.
    TopExceedsMax { requested: u32, max: u32 },
}

impl core::fmt::Display for ListConversionsQueryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TopMustBePositive => f.write_str("top must be at least 1"),
            Self::TopExceedsMax { requested, max } => {
                write!(f, "top {requested} exceeds max {max}")
            }
        }
    }
}

impl core::error::Error for ListConversionsQueryError {}

impl ListConversionsQuery {
    /// Upper bound enforced by every [`ListConversionsQuery`]
    /// constructor. Mirrors
    /// [`account_management_sdk::idp_user::IdpUserPagination::MAX_TOP`]
    /// so the conversion listing surface stays aligned with the
    /// platform's `OpenAPI Top.maximum` ceiling and a single
    /// listing request cannot exhaust the page-buffer allocation by
    /// requesting an unbounded `top`.
    pub const MAX_TOP: u32 = 200;

    /// Read-only access to the validated `top`. Always `>= 1` per the
    /// constructor invariants.
    #[must_use]
    pub const fn top(self) -> u32 {
        self.top
    }

    /// Read-only access to `skip`. Always set once at construction
    /// time so the value stays consistent with whatever validation the
    /// caller used to build the query.
    #[must_use]
    pub const fn skip(self) -> u32 {
        self.skip
    }

    /// Read-only access to the three-way status selector. Defaults to
    /// [`ConversionStatusSelector::DefaultPending`] when constructed
    /// via [`ListConversionsQuery::default_pending`].
    #[must_use]
    pub const fn status_filter(self) -> ConversionStatusSelector {
        self.status_filter
    }

    /// Build a query that returns only `Pending` rows. This is the
    /// "no explicit status filter" default for child-scope listings —
    /// resolved history (`Approved`/`Cancelled`/`Rejected`/`Expired`)
    /// is hidden until the caller explicitly opts in via
    /// [`Self::any`] or [`Self::with_status`].
    ///
    /// # Errors
    ///
    /// * [`ListConversionsQueryError::TopMustBePositive`] when
    ///   `top` is zero.
    /// * [`ListConversionsQueryError::TopExceedsMax`] when `top`
    ///   exceeds [`Self::MAX_TOP`] — guards against an unbounded
    ///   page-buffer allocation by mirroring the
    ///   `IdpUserPagination::TopExceedsMax` ceiling.
    pub const fn default_pending(top: u32, skip: u32) -> Result<Self, ListConversionsQueryError> {
        if top == 0 {
            return Err(ListConversionsQueryError::TopMustBePositive);
        }
        if top > Self::MAX_TOP {
            return Err(ListConversionsQueryError::TopExceedsMax {
                requested: top,
                max: Self::MAX_TOP,
            });
        }
        Ok(Self {
            top,
            skip,
            status_filter: ConversionStatusSelector::DefaultPending,
        })
    }

    /// Build a query that returns rows of every lifecycle status
    /// (no filter). Use this when the caller actively asks for
    /// resolved history alongside pending.
    ///
    /// # Errors
    ///
    /// * [`ListConversionsQueryError::TopMustBePositive`] when
    ///   `top` is zero.
    /// * [`ListConversionsQueryError::TopExceedsMax`] when `top`
    ///   exceeds [`Self::MAX_TOP`] — guards against an unbounded
    ///   page-buffer allocation by mirroring the
    ///   `IdpUserPagination::TopExceedsMax` ceiling.
    pub const fn any(top: u32, skip: u32) -> Result<Self, ListConversionsQueryError> {
        if top == 0 {
            return Err(ListConversionsQueryError::TopMustBePositive);
        }
        if top > Self::MAX_TOP {
            return Err(ListConversionsQueryError::TopExceedsMax {
                requested: top,
                max: Self::MAX_TOP,
            });
        }
        Ok(Self {
            top,
            skip,
            status_filter: ConversionStatusSelector::Any,
        })
    }

    /// Build a query that narrows to a specific lifecycle status.
    ///
    /// # Errors
    ///
    /// * [`ListConversionsQueryError::TopMustBePositive`] when
    ///   `top` is zero.
    /// * [`ListConversionsQueryError::TopExceedsMax`] when `top`
    ///   exceeds [`Self::MAX_TOP`] — guards against an unbounded
    ///   page-buffer allocation by mirroring the
    ///   `IdpUserPagination::TopExceedsMax` ceiling.
    pub const fn with_status(
        top: u32,
        skip: u32,
        status: ConversionStatus,
    ) -> Result<Self, ListConversionsQueryError> {
        if top == 0 {
            return Err(ListConversionsQueryError::TopMustBePositive);
        }
        if top > Self::MAX_TOP {
            return Err(ListConversionsQueryError::TopExceedsMax {
                requested: top,
                max: Self::MAX_TOP,
            });
        }
        Ok(Self {
            top,
            skip,
            status_filter: ConversionStatusSelector::Status(status),
        })
    }

    /// Lower into the repo-level pagination value.
    #[must_use]
    pub const fn pagination(self) -> ConversionPagination {
        ConversionPagination {
            top: self.top,
            skip: self.skip,
        }
    }

    /// Lower the status selector into the repo's
    /// `Option<ConversionStatus>` token.
    #[must_use]
    pub const fn repo_status_filter(self) -> Option<ConversionStatus> {
        self.status_filter.as_repo_filter()
    }
}

// @cpt-begin:cpt-cf-account-management-dod-managed-self-managed-modes-parent-side-minimal-surface:p1:inst-dod-parent-side-projection
/// Minimal cross-barrier projection of a [`ConversionRequest`] surfaced
/// to the parent side of the dual-consent pair.
///
/// Per the `Parent-Side Inbound-Discovery Minimal Surface` `DoD`, the
/// parent listing MUST NOT carry any child-subtree fields, descendant
/// counts, user records, or resource inventories. Every field below is
/// derivable from the conversion row itself or the converting tenant's
/// own row (`name`); no closure / metadata / inventory data leaks
/// across the parent-child barrier.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionRequestParentProjection {
    pub request_id: Uuid,
    pub tenant_id: Uuid,
    pub child_tenant_name: String,
    pub initiator_side: ConversionSide,
    pub target_mode: TargetMode,
    pub status: ConversionStatus,
    pub requested_by: Uuid,
    pub approved_by: Option<Uuid>,
    pub cancelled_by: Option<Uuid>,
    pub rejected_by: Option<Uuid>,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
    pub resolved_at: Option<OffsetDateTime>,
}
// @cpt-end:cpt-cf-account-management-dod-managed-self-managed-modes-parent-side-minimal-surface:p1:inst-dod-parent-side-projection

/// PEP descriptors for the conversion-request resource.
///
/// Mirrors the `pep::TENANT` / `pep::USER` / `pep::METADATA`
/// declarations on sibling services. The resource type name pins
/// [`account_management_sdk::gts::CONVERSION_REQUEST_RG_TYPE_CODE`];
/// the impl-side duplication is required because `ResourceType.name`
/// is a `&'static str` consumed at compile time.
pub(super) mod pep {
    use super::{ResourceType, pep_properties};

    /// Resource declaration for `ConversionRequest`. The compiled
    /// `AccessScope` is consumed by `ConversionService::authorize`
    /// for the allow/deny PEP gate plus the `InTenantSubtree`
    /// predicate the tenant-existence guards (caller-owned tenant
    /// resolve, parent / child Active prechecks) consult.
    ///
    /// Supported PEP properties:
    ///
    /// * `OWNER_TENANT_ID` — the tenant the request is acted upon
    ///   from the caller's side (child-tenant id for child-side
    ///   callers, parent-tenant id for parent-side callers). The
    ///   service forwards `tenant_id` (the URL-bound scope) here
    ///   so policies that gate by ownership see the right tenant.
    /// * `RESOURCE_ID` — set to the same tenant id (matches the
    ///   `tenants` entity's `resource_col = "id"` declaration so the
    ///   compiled subtree clamp on `tenants` resolves through this
    ///   property).
    pub const CONVERSION: ResourceType = ResourceType {
        name: "gts.cf.core.am.conversion_request.v1~",
        supported_properties: &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    };

    /// Action vocabulary. Each public conversion-service method
    /// PEP-gates on exactly one action; system-driven sweeps
    /// (`expire_pending`, `soft_delete_resolved`) do NOT pass
    /// through the PEP gate because they run under
    /// [`super::ConversionScope::system_sweep`].
    pub mod actions {
        pub const REQUEST: &str = "request";
        pub const CANCEL: &str = "cancel";
        pub const REJECT: &str = "reject";
        pub const APPROVE: &str = "approve";
        pub const LIST_OWN: &str = "list_own";
        pub const LIST_INBOUND: &str = "list_inbound";
    }
}

/// Central AM domain service for `ConversionRequest` lifecycle.
///
/// Construction mirrors `TenantService::new` — every dependency is
/// passed in as an `Arc<dyn ...>` so production wiring (`module.rs`)
/// and tests (`FakeConversionRepo` / `FakeTenantRepo`) share the same
/// constructor surface. The clock seam (`now_fn`) is overridable via
/// the [`Self::with_now_fn`] builder so service-level unit tests can
/// pin a fixed instant for the `expires_at` / `cutoff` assertions.
#[domain_model]
pub struct ConversionService {
    repo: Arc<dyn ConversionRepo>,
    tenant_repo: Arc<dyn TenantRepo>,
    /// Parent / child type-compatibility barrier. Owned by the service
    /// so the type re-evaluation runs at the domain layer, BEFORE the
    /// repo's apply transaction. The service hands the observed
    /// `tenant_type_uuid` values to the repo via
    /// [`ApplyConversionApprovalInput::expected_tenant_type_uuid`] /
    /// [`ApplyConversionApprovalInput::expected_parent_tenant_type_uuid`]
    /// as TX-side TOCTOU guards.
    tenant_type_checker: Arc<dyn TenantTypeChecker + Send + Sync>,
    /// PEP gate. Mirrors `TenantService` / `UserService` / `MetadataService`:
    /// every caller-facing conversion method PEP-gates via
    /// [`Self::authorize`] before any state read. The `PolicyEnforcer`
    /// is owned by-value (it is `Clone`); the module wiring clones it
    /// from the shared instance used by sibling services.
    enforcer: PolicyEnforcer,
    now_fn: NowFn,
    approval_ttl: StdDuration,
    resolved_retention: StdDuration,
    cleanup_interval: StdDuration,
    expire_batch_size: u32,
    retention_batch_size: u32,
}

// `ConversionRepo` mutating calls (`find_by_id` / `transition_pending_to_*`
// / `apply_conversion_approval`) receive a service-built `AccessScope`
// derived from the [`ConversionCaller`] via [`conversion_repo_scope`].
// `conversion_requests` declares `tenant_col = "tenant_id"` and
// `resource_col = "id"` (post-#1813), so the secure-extension layer
// materialises the caller-bound clamp at the database (`tenant_id =
// child_id` for a child-side caller, `tenant_id IN closure(parent_id)`
// with barrier penetration for a parent-side caller). The incoming
// `&AccessScope` argument from REST handlers is forwarded to the
// `TenantRepo` lookups and the `verify_caller_scope` PDP boundary; the
// conversion-row scope is derived from the URL-bound caller side so
// the repo's row-level enforcement stays consistent regardless of how
// the platform PDP currently shapes the caller's tenant scope.
// INSERT paths continue to call `scope_unchecked` — the Scopable
// INSERT-time clamp isn't the right model for inserts (the row is
// being created and cannot yet be filtered).

impl ConversionService {
    /// Default cleanup tick used when `with_cleanup_lifecycle` is not
    /// invoked (matches ADR-0003 §1: 60s).
    #[allow(
        clippy::duration_suboptimal_units,
        reason = "from_mins is unstable on workspace MSRV; keep from_secs"
    )]
    pub const DEFAULT_CLEANUP_INTERVAL: StdDuration = StdDuration::from_secs(60);
    /// Default per-tick caps used when `with_cleanup_lifecycle` is not
    /// invoked.
    pub const DEFAULT_EXPIRE_BATCH_SIZE: u32 = 256;
    pub const DEFAULT_RETENTION_BATCH_SIZE: u32 = 256;

    /// Construct a fully-wired service with the production clock
    /// (`OffsetDateTime::now_utc`).
    ///
    /// Cleanup-loop knobs (`cleanup_interval`, `expire_batch_size`,
    /// `retention_batch_size`) default to ADR-0003 §1 values; production
    /// wiring overrides them via [`Self::with_cleanup_lifecycle`] from
    /// `cfg.conversion`.
    #[must_use]
    pub fn new(
        repo: Arc<dyn ConversionRepo>,
        tenant_repo: Arc<dyn TenantRepo>,
        tenant_type_checker: Arc<dyn TenantTypeChecker + Send + Sync>,
        enforcer: PolicyEnforcer,
        approval_ttl: StdDuration,
        resolved_retention: StdDuration,
    ) -> Self {
        Self {
            repo,
            tenant_repo,
            tenant_type_checker,
            enforcer,
            now_fn: Arc::new(OffsetDateTime::now_utc),
            approval_ttl,
            resolved_retention,
            cleanup_interval: Self::DEFAULT_CLEANUP_INTERVAL,
            expire_batch_size: Self::DEFAULT_EXPIRE_BATCH_SIZE,
            retention_batch_size: Self::DEFAULT_RETENTION_BATCH_SIZE,
        }
    }

    /// PEP gate. Calls the platform-side `PolicyEnforcer`, returns the
    /// [`AccessScope`] caller-visibility fences forward through
    /// `TenantRepo` lookups.
    ///
    /// Mirrors `TenantService::authorize` / `UserService::authorize` /
    /// `MetadataService::authorize`:
    ///
    /// * `OWNER_TENANT_ID = tenant_id` — the URL-bound scope tenant
    ///   (child tenant for child-side callers, parent tenant for
    ///   parent-side callers).
    /// * `RESOURCE_ID = tenant_id` — matches `tenants.id` so the PDP-
    ///   emitted `InTenantSubtree` predicate clamps the `tenants` reads
    ///   in the caller-visibility fences to the caller's subtree.
    /// * `require_constraints(true)` — a PDP returning `decision: true,
    ///   constraints: []` fails closed via `CompileFailed →
    ///   CrossTenantDenied` rather than silently widening visibility.
    ///
    /// System-driven sweeps (`expire_pending`, `soft_delete_resolved`)
    /// do NOT call this method — they run under
    /// [`ConversionScope::system_sweep`].
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        tenant_id: Uuid,
    ) -> Result<AccessScope, DomainError> {
        let request = AccessRequest::new()
            .resource_property(pep_properties::OWNER_TENANT_ID, tenant_id)
            .resource_property(pep_properties::RESOURCE_ID, tenant_id)
            .require_constraints(true);
        let scope = self
            .enforcer
            .access_scope_with(ctx, &pep::CONVERSION, action, Some(tenant_id), &request)
            .await?;
        Ok(scope)
    }

    /// Override the wall-clock function used to compute `expires_at`
    /// and the retention cutoff. Mirrors `TenantService::with_*`
    /// builder methods used to plug optional collaborators after
    /// construction.
    #[must_use]
    pub fn with_now_fn(mut self, now_fn: NowFn) -> Self {
        self.now_fn = now_fn;
        self
    }

    /// Override the cleanup-loop knobs `cleanup_interval`,
    /// `expire_batch_size`, and `retention_batch_size`. Production
    /// wiring (`AccountManagementModule::init`) reads these from the
    /// `[conversion]` config section. Tests that do not invoke this
    /// builder pick up ADR-0003 §1 defaults.
    #[must_use]
    pub const fn with_cleanup_lifecycle(
        mut self,
        cleanup_interval: StdDuration,
        expire_batch_size: u32,
        retention_batch_size: u32,
    ) -> Self {
        self.cleanup_interval = cleanup_interval;
        self.expire_batch_size = expire_batch_size;
        self.retention_batch_size = retention_batch_size;
        self
    }

    /// Read-only access to the configured cleanup tick cadence.
    #[must_use]
    pub const fn cleanup_interval(&self) -> StdDuration {
        self.cleanup_interval
    }

    /// Read-only access to the configured per-tick expire batch cap.
    #[must_use]
    pub const fn expire_batch_size(&self) -> u32 {
        self.expire_batch_size
    }

    /// Read-only access to the configured per-tick retention sweep cap.
    #[must_use]
    pub const fn retention_batch_size(&self) -> u32 {
        self.retention_batch_size
    }

    /// Read-only access to the configured `approval_ttl`. Useful for
    /// callers that want to surface the TTL through the response
    /// envelope without re-reading config.
    #[must_use]
    pub const fn approval_ttl(&self) -> StdDuration {
        self.approval_ttl
    }

    /// Read-only access to the configured `resolved_retention`. The
    /// retention reaper consumes this when no override is supplied.
    #[must_use]
    pub const fn resolved_retention(&self) -> StdDuration {
        self.resolved_retention
    }

    /// Helper: snapshot the current wall-clock through the configured
    /// `now_fn`. Centralised so every `expires_at` / `resolved_at` /
    /// `cutoff` derivation reads from the same seam.
    fn now(&self) -> OffsetDateTime {
        (self.now_fn)()
    }

    /// Caller-visibility fence used by mutation methods (`cancel`,
    /// `reject`) before performing a state transition on a
    /// conversion row.
    ///
    /// The `ConversionRepo` runs at `AccessScope::allow_all()`
    /// because `conversion_requests` has no scope columns; without
    /// this fence, an internal caller that can mint a matching
    /// [`ConversionCaller`] could act on a request outside their
    /// [`AccessScope`] (the
    /// [`require_caller_scope_or_not_found`] check above only
    /// confirms URL-vs-row coherence, not the caller's authorization
    /// to that tenant). Mirroring the pattern used by `approve` /
    /// `list_*` methods, this helper resolves the caller-owned
    /// tenant (`row.tenant_id` for child callers,
    /// `row.parent_id` for parent callers) under the incoming
    /// `scope` and collapses every miss (out-of-scope, nonexistent,
    /// soft-deleted) into `NotFound` so the existence channel does
    /// not leak.
    ///
    /// # Errors
    ///
    /// * [`DomainError::NotFound`] when the caller-owned tenant does
    ///   not resolve under `scope`, or has been soft-deleted, or
    ///   (for a parent-side caller) the row's `parent_id` is `None`.
    /// * Any storage error surfaced by `tenant_repo.find_by_id`.
    async fn require_caller_tenant_visible(
        &self,
        scope: &AccessScope,
        caller: ConversionCaller,
        row: &ConversionRequest,
        op: &'static str,
    ) -> Result<(), DomainError> {
        let target = match caller.side() {
            ConversionSide::Child => row.tenant_id,
            // INVARIANT: every current caller (`cancel`, `reject`)
            // runs `require_caller_scope_or_not_found` BEFORE this
            // helper, which itself verifies
            // `caller.scope_id == row.parent_id` for parent-side
            // callers — so a `row.parent_id == None` row would have
            // already collapsed to `NotFound` there. The
            // `ok_or_else` arm below is defense-in-depth for any
            // future call site that invokes this helper without
            // running the URL-coherence gate first.
            ConversionSide::Parent => {
                row.parent_id
                    .ok_or_else(|| DomainError::ConversionRequestNotFound {
                        detail: format!(
                            "{op}: resource {} not found or not accessible to the caller",
                            row.id
                        ),
                        resource: row.id.to_string(),
                    })?
            }
        };
        let tenant = self
            .tenant_repo
            .find_by_id(scope, target)
            .await?
            .ok_or_else(|| DomainError::ConversionRequestNotFound {
                detail: format!(
                    "{op}: resource {} not found or not accessible to the caller",
                    row.id
                ),
                resource: row.id.to_string(),
            })?;
        // Soft-deleted tenant: collapse to `ConversionRequestNotFound`
        // so a row tied to a removed tenant cannot be mutated through
        // this seam — the caller is acting on a conversion request,
        // not the tenant directly.
        if matches!(tenant.status, TenantStatus::Deleted) {
            return Err(DomainError::ConversionRequestNotFound {
                detail: format!(
                    "{op}: resource {} not found or not accessible to the caller",
                    row.id
                ),
                resource: row.id.to_string(),
            });
        }
        Ok(())
    }

    // ----------------------------------------------------------------
    // request_conversion
    // ----------------------------------------------------------------

    /// Initiate a new conversion. Implements
    /// `cpt-cf-account-management-flow-managed-self-managed-modes-conversion-initiation`.
    ///
    /// Guard ordering (MUST match the FEATURE `DoD` for
    /// `single-pending-enforcement` and `root-tenant-conversion-refusal`):
    ///
    /// 1. Load the tenant via `tenant_repo.find_by_id`.
    /// 2. Reject the platform root (`parent_id IS NULL`) with
    ///    [`DomainError::RootTenantCannotConvert`].
    /// 3. Reject any non-`Active` status with
    ///    [`DomainError::Validation`].
    /// 4. Compose the [`NewConversionRequest`] (including the
    ///    `expires_at = now() + approval_ttl` derivation) and hand
    ///    off to `repo.insert_pending`. The repo-level partial-
    ///    unique-index collision returns
    ///    [`DomainError::PendingExists`] unchanged.
    ///
    /// # Errors
    ///
    /// * [`DomainError::NotFound`] when `tenant_id` does not resolve
    ///   to a tenant row.
    /// * [`DomainError::RootTenantCannotConvert`] when the resolved
    ///   tenant is the platform root.
    /// * [`DomainError::Validation`] when the resolved tenant is not
    ///   in [`TenantStatus::Active`].
    /// * [`DomainError::PendingExists`] when another `Pending` row
    ///   already exists for the tenant.
    // @cpt-begin:cpt-cf-account-management-flow-managed-self-managed-modes-conversion-initiation:p1:inst-flow-conversion-initiation-service
    #[allow(
        clippy::cognitive_complexity,
        reason = "flat guard sequence (PEP gate -> tenant load -> root-tenant refusal -> non-active reject -> type resolve -> insert_pending) is the security-critical ordering reviewers eyeball-check; extracting helpers would fragment the audit chain and obscure the @cpt-* CPT markers anchored to each step"
    )]
    pub async fn request_conversion(
        &self,
        ctx: &SecurityContext,
        input: RequestConversionInput,
    ) -> Result<ConversionRequest, DomainError> {
        // PEP gate FIRST: compiles the caller's `SecurityContext` into
        // an `AccessScope` (`InTenantSubtree` predicate rooted at the
        // caller's subtree). A denied caller surfaces as
        // `CrossTenantDenied` BEFORE any tenant lookup or row write.
        // Mirrors the production posture in `TenantService` /
        // `UserService` / `MetadataService`. The gate is keyed on the
        // caller's URL-bound tenant id (`caller.scope_id()`), not on
        // the conversion target — for a parent-side initiation the
        // parent IS the URL-bound tenant.
        let scope = self
            .authorize(ctx, pep::actions::REQUEST, input.caller.scope_id())
            .await?;
        let actor = ctx.subject_id();
        // `tenants` is `Scopable(no_tenant, no_resource, no_owner,
        // no_type)`, so the entity-level scope filter is a no-op in
        // production AND would collapse a parent-scoped caller's
        // visibility of a self-managed child (the child sits behind
        // the closure barrier and is invisible to the parent's
        // narrowed `AccessScope`). The parent-initiation flow
        // (`POST /tenants/{parent_id}/child-conversions`) MUST see
        // the child regardless of barrier so it can verify the
        // parent-child relationship via `require_caller_scope_or_not_found`
        // below. We therefore load the row at `allow_all` and rely
        // on the URL-binding scope-coherence check (and the role /
        // state matrix) for authorization.
        let tenant = self
            .tenant_repo
            .find_by_id(&AccessScope::allow_all(), input.tenant_id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                detail: format!("tenant {} not found", input.tenant_id),
                resource: input.tenant_id.to_string(),
            })?;
        // URL-vs-row coherence: a parent-side caller MUST be acting
        // on a tenant whose `parent_id` matches the caller's declared
        // scope. Runs BEFORE any tenant-shape guard (root-tenant
        // refusal, status precondition, type checks) so that an
        // out-of-scope caller cannot probe tenant topology through
        // the error-code channel. Mirrors the FEATURE doc's
        // parent-side initiation flow
        // (`/tenants/{parent_id}/child-conversions`).
        require_caller_scope_or_not_found(
            input.caller,
            "request_conversion",
            tenant.id,
            tenant.parent_id,
            tenant.id,
        )?;

        // Caller-visibility fence: resolve the caller-owned tenant
        // under the incoming `AccessScope` so an internal actor that
        // can mint a matching `ConversionCaller` cannot create a
        // conversion on a tenant outside its `AccessScope`. The
        // initial `find_by_id(allow_all, ...)` above is intentionally
        // a structural read (needed for parent_id / status / type
        // decisions on a row the PEP has already gated); without this
        // second `scope`-clamped lookup the prior
        // `require_caller_scope_or_not_found` only proves URL/row
        // coherence, not the caller's authorization to that tenant.
        // Mirrors the `tenant_repo.find_by_id(scope, ...)` pattern
        // already in `cancel` / `reject` / `approve`. An out-of-scope
        // / nonexistent / soft-deleted caller-owned tenant collapses
        // to `NotFound` here, BEFORE root/status/type guards leak
        // topology.
        let caller_owned_id = match input.caller.side() {
            ConversionSide::Child => tenant.id,
            // Defense-in-depth: `require_caller_scope_or_not_found`
            // above already rejects a parent-side caller against a
            // root row (`parent_id == None`), so this `ok_or_else`
            // is unreachable on the standard call path. Kept loud in
            // case a future caller invokes this seam without the
            // URL-coherence gate.
            ConversionSide::Parent => tenant.parent_id.ok_or_else(|| DomainError::NotFound {
                detail: format!(
                    "request_conversion: tenant {} not found or not accessible to the caller",
                    input.tenant_id
                ),
                resource: input.tenant_id.to_string(),
            })?,
        };
        let caller_owned = self
            .tenant_repo
            .find_by_id(&scope, caller_owned_id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                detail: format!(
                    "request_conversion: tenant {} not found or not accessible to the caller",
                    input.tenant_id
                ),
                resource: input.tenant_id.to_string(),
            })?;
        if matches!(caller_owned.status, TenantStatus::Deleted) {
            return Err(DomainError::NotFound {
                detail: format!(
                    "request_conversion: tenant {} not found or not accessible to the caller",
                    input.tenant_id
                ),
                resource: input.tenant_id.to_string(),
            });
        }

        // @cpt-begin:cpt-cf-account-management-algo-managed-self-managed-modes-root-tenant-conversion-refusal:p1:inst-algo-root-tenant-refusal
        // @cpt-begin:cpt-cf-account-management-dod-managed-self-managed-modes-root-tenant-non-convertibility:p1:inst-dod-root-tenant-non-convertibility
        // Root-tenant refusal runs AFTER the scope check above so an
        // out-of-scope caller cannot distinguish "this is the root"
        // from "you have no scope here". The platform root has
        // `parent_id == None` and cannot legally take a counterparty
        // (no parent on the other side of the dual-consent pair),
        // so the conversion is rejected here before any DB write.
        if tenant.parent_id.is_none() {
            return Err(DomainError::RootTenantCannotConvert);
        }
        // @cpt-end:cpt-cf-account-management-dod-managed-self-managed-modes-root-tenant-non-convertibility:p1:inst-dod-root-tenant-non-convertibility
        // @cpt-end:cpt-cf-account-management-algo-managed-self-managed-modes-root-tenant-conversion-refusal:p1:inst-algo-root-tenant-refusal

        // Status precondition: only `Active` tenants may convert.
        // `Provisioning` is mid-saga; `Suspended` and `Deleted`
        // freeze the lifecycle. Any non-`Active` status here is a
        // validation failure rather than a not-found because the row
        // exists and the caller can disambiguate from the
        // `attempted_status` token in the detail.
        if !matches!(tenant.status, TenantStatus::Active) {
            return Err(DomainError::Validation {
                detail: format!(
                    "tenant {} is not active (status={})",
                    tenant.id,
                    tenant.status.as_str()
                ),
            });
        }

        // Compute target mode: the conversion semantics are
        // "switch mode", so the default target is the strict
        // inverse of the tenant's `self_managed` bool — derived
        // directly from the bool to stay `#[non_exhaustive]`-safe
        // on `TargetMode` (any future variant added to the enum
        // would not be inferrable from a 2-valued bool).
        //
        // An explicit `target_mode_override` MUST match
        // `inverse_of_current` exactly. The earlier shape used a
        // `target_mode != current_mode` no-op guard which would
        // silently accept a future `TargetMode::X` override against
        // a `Managed` / `SelfManaged` tenant — the tenant has no
        // `X`-mode column on the schema, so the inverse-check is
        // the load-bearing validation. Reject any override that
        // is not the strict binary inverse with `Validation`
        // (envelope-consistent with the other initiation guards).
        let current_mode = if tenant.self_managed {
            TargetMode::SelfManaged
        } else {
            TargetMode::Managed
        };
        let inverse_of_current = if tenant.self_managed {
            TargetMode::Managed
        } else {
            TargetMode::SelfManaged
        };
        let target_mode = match input.target_mode_override {
            Some(requested) if requested == inverse_of_current => requested,
            Some(requested) => {
                return Err(DomainError::Validation {
                    detail: format!(
                        "target_mode_override={} is not the inverse of the tenant's current \
                         mode ({}); the only admissible override is {}",
                        requested.as_str(),
                        current_mode.as_str(),
                        inverse_of_current.as_str(),
                    ),
                });
            }
            None => inverse_of_current,
        };

        let now = self.now();
        // `OffsetDateTime + StdDuration` panics on arithmetic overflow.
        // `approval_ttl` is bounded by `ConversionConfig::MAX_APPROVAL_TTL_SECS`
        // (30d) at config-validation time so today the addition is
        // trivially safe — but `ConversionService::new` accepts an
        // arbitrary `StdDuration`, and any future relaxation of the
        // cap would crash the request-conversion path with a panic
        // instead of returning a clean envelope. `checked_add`
        // converts the panic into a recoverable `Internal`.
        let ttl =
            time::Duration::try_from(self.approval_ttl).map_err(|err| DomainError::Internal {
                diagnostic: format!(
                    "request_conversion: approval_ttl ({:?}) does not fit in time::Duration: {err}",
                    self.approval_ttl
                ),
                cause: None,
            })?;
        let expires_at = now.checked_add(ttl).ok_or_else(|| DomainError::Internal {
            diagnostic: format!(
                "request_conversion: now + approval_ttl overflows OffsetDateTime (now={now:?}, ttl={ttl:?})"
            ),
            cause: None,
        })?;

        let new = NewConversionRequest {
            id: Uuid::new_v4(),
            tenant_id: tenant.id,
            parent_id: tenant.parent_id,
            child_tenant_name: tenant.name.clone(),
            initiator_side: input.caller.side(),
            target_mode,
            requested_by: actor,
            requested_at: now,
            expires_at,
        };

        // @cpt-begin:cpt-cf-account-management-algo-managed-self-managed-modes-single-pending-enforcement:p1:inst-algo-single-pending-enforcement
        // The partial-unique-index collision on
        // `ux_conversion_requests_pending` is mapped by the repo to
        // [`DomainError::PendingExists { request_id }`]. Bubble it up
        // unchanged — the existing pending row's id is the caller's
        // hint to drive a cancel / reject before retrying.
        let inserted = self
            .repo
            .insert_pending(&AccessScope::allow_all(), &new)
            .await?;
        // @cpt-end:cpt-cf-account-management-algo-managed-self-managed-modes-single-pending-enforcement:p1:inst-algo-single-pending-enforcement

        // TODO(events): emit AM event when the platform event-bus
        // lands. Placeholder log marks the emission point with the
        // v1-stand-in cadence proven by `TenantService` for
        // `tenant_*` events.
        tracing::info!(
            target: "am.events",
            event = "conversion_requested",
            request_id = %inserted.id,
            tenant_id = %inserted.tenant_id,
            caller_side = input.caller.side().as_str(),
            actor_uuid = %actor,
            target_mode = inserted.target_mode.as_str(),
            outcome = "ok",
            "am conversion requested"
        );

        Ok(inserted)
    }
    // @cpt-end:cpt-cf-account-management-flow-managed-self-managed-modes-conversion-initiation:p1:inst-flow-conversion-initiation-service

    // ----------------------------------------------------------------
    // cancel
    // ----------------------------------------------------------------

    /// Cancel a pending conversion. Initiator-side action. Implements
    /// `cpt-cf-account-management-flow-managed-self-managed-modes-conversion-cancellation`.
    ///
    /// Guard ordering (MUST match `Dual-Consent Actor Discipline`
    /// `DoD`): load row -> status precondition (`Pending`) -> actor-
    /// side check (`caller_side == initiator_side`).
    ///
    /// # Errors
    ///
    /// * [`DomainError::NotFound`] — `request_id` does not resolve.
    /// * [`DomainError::AlreadyResolved`] — row is in any terminal
    ///   status (this MUST take precedence over the actor check).
    /// * [`DomainError::InvalidActorForTransition`] — caller side
    ///   does not match the initiator side.
    // @cpt-begin:cpt-cf-account-management-flow-managed-self-managed-modes-conversion-cancellation:p1:inst-flow-conversion-cancellation-service
    pub async fn cancel(
        &self,
        ctx: &SecurityContext,
        request_id: Uuid,
        caller: ConversionCaller,
    ) -> Result<ConversionRequest, DomainError> {
        // PEP gate FIRST: compile the caller's `SecurityContext` into
        // an `AccessScope` keyed on the URL-bound caller tenant. A
        // denied caller surfaces as `CrossTenantDenied` BEFORE any
        // row lookup or visibility leak through the error channel.
        let scope = self
            .authorize(ctx, pep::actions::CANCEL, caller.scope_id())
            .await?;
        let cancelled_by = ctx.subject_id();
        // `ConversionRepo` calls below pass a caller-bound scope built
        // by [`conversion_repo_scope`]; with the entity declaring
        // `tenant_col = "tenant_id"` + `resource_col = "id"`, the
        // secure-extension layer clamps `tenant_id = child_id` (child-
        // side) or `tenant_id IN closure(parent_id) AND barrier-ignored`
        // (parent-side counterparty / parent-initiated cancel of a
        // child that may sit behind the closure barrier). Visibility
        // on the caller-owned tenant is still verified via
        // `tenant_repo.find_by_id(scope, caller_owned_tenant_id)`
        // below — the row-level repo clamp is defence-in-depth on top
        // of that fence and the
        // `require_caller_scope_or_not_found` URL-coherence check
        // above.
        let repo_scope = conversion_repo_scope(caller);
        // @cpt-begin:cpt-cf-account-management-dod-managed-self-managed-modes-dual-consent-actor-discipline:p1:inst-dod-dual-consent-actor-discipline-cancel
        let row = self
            .repo
            .find_by_id(&repo_scope, request_id)
            .await?
            .ok_or_else(|| DomainError::ConversionRequestNotFound {
                detail: format!("conversion request {request_id} not found"),
                resource: request_id.to_string(),
            })?;

        // Parent-side scope verification BEFORE the state / role
        // matrix runs: a parent-side caller MUST be acting on a
        // request whose `parent_id` matches the caller's declared
        // scope. Surfaces `Validation` so a misrouted parent-side
        // call cannot leak `AlreadyResolved` / `InvalidActor` from a
        // request that isn't theirs to act on.
        require_caller_scope_or_not_found(
            caller,
            "cancel",
            row.tenant_id,
            row.parent_id,
            request_id,
        )?;

        // Caller-visibility fence: resolve the caller-owned tenant
        // under the PEP-compiled `AccessScope`. Without this, an
        // internal actor that can mint a matching `ConversionCaller`
        // could cancel a request on a tenant outside its
        // `AccessScope` because the repo runs at `allow_all` and the
        // `require_caller_scope_or_not_found` check above only
        // confirms URL coherence, not the caller's authorization
        // to that tenant. Mirrors the `tenant_repo.find_by_id(scope, ...)`
        // pattern in `approve` and in the listing methods. An
        // out-of-scope / nonexistent tenant collapses to `NotFound`
        // here, before the cancel mutation runs.
        self.require_caller_tenant_visible(&scope, caller, &row, "cancel")
            .await?;

        // Single guard: state-then-role validation lives in
        // `state_machine::validate_transition` so service-layer and
        // any future callers share one matrix. Returns `AlreadyResolved`
        // if the row is not pending (state precedes role per the
        // Dual-Consent Actor Discipline DoD), or
        // `InvalidActorForTransition` carrying `attempted_status =
        // "cancelled"` when `caller_side != initiator_side`.
        validate_transition(
            row.status,
            ConversionStatus::Cancelled,
            Some(caller.side()),
            row.initiator_side,
        )?;
        // @cpt-end:cpt-cf-account-management-dod-managed-self-managed-modes-dual-consent-actor-discipline:p1:inst-dod-dual-consent-actor-discipline-cancel

        let now = self.now();
        let updated = self
            .repo
            .transition_pending_to_cancelled(&repo_scope, request_id, cancelled_by, now)
            .await?;

        tracing::info!(
            target: "am.events",
            event = "conversion_cancelled",
            request_id = %updated.id,
            tenant_id = %updated.tenant_id,
            caller_side = caller.side().as_str(),
            actor_uuid = %cancelled_by,
            outcome = "ok",
            "am conversion cancelled"
        );

        Ok(updated)
    }
    // @cpt-end:cpt-cf-account-management-flow-managed-self-managed-modes-conversion-cancellation:p1:inst-flow-conversion-cancellation-service

    // ----------------------------------------------------------------
    // reject
    // ----------------------------------------------------------------

    /// Reject a pending conversion. Counterparty-side action.
    /// Implements
    /// `cpt-cf-account-management-flow-managed-self-managed-modes-conversion-rejection`.
    ///
    /// Guard ordering mirrors [`Self::cancel`] — status precondition
    /// precedes actor-side check — only the actor-side rule is the
    /// inverse: `caller_side != initiator_side`.
    ///
    /// # Errors
    ///
    /// * [`DomainError::NotFound`] — `request_id` does not resolve.
    /// * [`DomainError::AlreadyResolved`] — row is in any terminal
    ///   status.
    /// * [`DomainError::InvalidActorForTransition`] — caller side
    ///   matches the initiator side (initiator cannot reject their
    ///   own request; they cancel instead).
    // @cpt-begin:cpt-cf-account-management-flow-managed-self-managed-modes-conversion-rejection:p1:inst-flow-conversion-rejection-service
    pub async fn reject(
        &self,
        ctx: &SecurityContext,
        request_id: Uuid,
        caller: ConversionCaller,
    ) -> Result<ConversionRequest, DomainError> {
        // PEP gate FIRST — see `cancel` for the full rationale.
        let scope = self
            .authorize(ctx, pep::actions::REJECT, caller.scope_id())
            .await?;
        let rejected_by = ctx.subject_id();
        // See `cancel` for the rationale on the side-specific
        // `conversion_repo_scope` shape and the role of the
        // `require_caller_scope_or_not_found` URL-coherence gate /
        // `require_caller_tenant_visible` caller-tenant fence as
        // defence-in-depth above the repo-level clamp.
        let repo_scope = conversion_repo_scope(caller);
        // @cpt-begin:cpt-cf-account-management-dod-managed-self-managed-modes-dual-consent-actor-discipline:p1:inst-dod-dual-consent-actor-discipline-reject
        let row = self
            .repo
            .find_by_id(&repo_scope, request_id)
            .await?
            .ok_or_else(|| DomainError::ConversionRequestNotFound {
                detail: format!("conversion request {request_id} not found"),
                resource: request_id.to_string(),
            })?;

        // Parent-side scope verification BEFORE the state / role
        // matrix runs (see `cancel` for the rationale).
        require_caller_scope_or_not_found(
            caller,
            "reject",
            row.tenant_id,
            row.parent_id,
            request_id,
        )?;

        // Caller-visibility fence: resolve the caller-owned tenant
        // under the PEP-compiled `AccessScope`. See `cancel` for the
        // full rationale on why this is required alongside
        // `require_caller_scope_or_not_found` when the repo runs at
        // `allow_all`.
        self.require_caller_tenant_visible(&scope, caller, &row, "reject")
            .await?;

        // State-then-role validation: see `cancel` for the full
        // rationale. For reject, the role rule inverts: the caller
        // MUST be the counterparty (`caller_side != initiator_side`).
        validate_transition(
            row.status,
            ConversionStatus::Rejected,
            Some(caller.side()),
            row.initiator_side,
        )?;
        // @cpt-end:cpt-cf-account-management-dod-managed-self-managed-modes-dual-consent-actor-discipline:p1:inst-dod-dual-consent-actor-discipline-reject

        let now = self.now();
        let updated = self
            .repo
            .transition_pending_to_rejected(&repo_scope, request_id, rejected_by, now)
            .await?;

        tracing::info!(
            target: "am.events",
            event = "conversion_rejected",
            request_id = %updated.id,
            tenant_id = %updated.tenant_id,
            caller_side = caller.side().as_str(),
            actor_uuid = %rejected_by,
            outcome = "ok",
            "am conversion rejected"
        );

        Ok(updated)
    }
    // @cpt-end:cpt-cf-account-management-flow-managed-self-managed-modes-conversion-rejection:p1:inst-flow-conversion-rejection-service

    // ----------------------------------------------------------------
    // listings
    //
    // Caller-visibility fence is asymmetric vs the mutation surface:
    // the mutation methods (`cancel` / `reject` / `approve`) take a
    // `ConversionCaller` and call `require_caller_scope_or_not_found`
    // before any state read, so a misrouted call cannot probe row
    // existence through a distinguishable error code. The listing
    // methods take a bare `Uuid` and lean on the
    // `tenant_repo.find_by_id(scope, ...)` lookup at the top of each
    // implementation as the single existence gate — an out-of-scope /
    // nonexistent / soft-deleted tenant collapses to `NotFound`
    // there, before the conversion repo is touched.
    //
    // Conversion-repo scope: the listing methods build a derived
    // `AccessScope` (`own_listing_repo_scope` / `parent_inbound_repo_scope`)
    // and forward it to the conversion repo so the secure-extension
    // layer materialises the row-level clamp at the database
    // (`tenant_id = X` for own listings, `tenant_id IN closure(parent)
    // AND barrier-ignored` for parent listings). This is defence-in-
    // depth on top of the `tenant_repo.find_by_id(scope, ...)`
    // visibility fence and is independent of the caller's incoming
    // `&AccessScope`, mirroring the mutation-side `conversion_repo_scope`
    // contract. Do NOT restore a `ConversionCaller` scope check on
    // these paths — it would duplicate the gate and split the caller-
    // visibility surface between two layers, which is exactly what
    // `require_caller_scope_or_not_found` exists to prevent on the
    // mutation paths (single source of truth).
    // ----------------------------------------------------------------

    /// List conversion requests owned by `tenant_id` (the converting
    /// tenant itself). Returns the full [`ConversionRequest`] rows —
    /// the converting tenant has no cross-barrier projection rules
    /// because the request lives inside its own scope.
    ///
    /// # Errors
    ///
    /// * Any error surfaced by `repo.list_own_for_tenant`.
    pub async fn list_own_for_tenant(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        page_query: &ListConversionsQuery,
    ) -> Result<OffsetPage<ConversionRequest>, DomainError> {
        // PEP gate FIRST: compile the caller's `SecurityContext` into
        // an `AccessScope` keyed on the URL-bound `tenant_id`. A
        // denied caller surfaces as `CrossTenantDenied` BEFORE any
        // tenant lookup or listing.
        let scope = self
            .authorize(ctx, pep::actions::LIST_OWN, tenant_id)
            .await?;
        // Tenant-existence guard mirrors `list_inbound_for_parent`:
        // resolve `tenant_id` under the PEP-compiled `scope` so a
        // nonexistent / soft-deleted / out-of-scope tenant collapses
        // to `NotFound` rather than returning a misleading `200 /
        // empty` page. The lookup also serves as the caller-visibility
        // fence — without it, an out-of-scope caller could probe the
        // existence of conversion requests for tenants outside their
        // subtree by observing the page's `total`.
        //
        // `TenantRepo::find_by_id` deliberately returns soft-deleted
        // rows too (see its trait docstring), so reject `Deleted`
        // explicitly: a soft-deleted tenant must collapse to
        // `NotFound` from this listing's perspective, not return its
        // historical conversion rows.
        let tenant = self
            .tenant_repo
            .find_by_id(&scope, tenant_id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                detail: format!("tenant {tenant_id} not found"),
                resource: tenant_id.to_string(),
            })?;
        if matches!(tenant.status, TenantStatus::Deleted) {
            return Err(DomainError::NotFound {
                detail: format!("tenant {tenant_id} not found"),
                resource: tenant_id.to_string(),
            });
        }
        // `list_own_for_tenant` is the "tenant lists its own
        // conversions" surface — the converting tenant lives at the
        // root of its own subtree and the rows we want are precisely
        // those whose `tenant_id == tenant_id`. Equality (via
        // [`own_listing_repo_scope`]) is sharper than a subtree clamp
        // here: a tenant should NOT see its descendants' conversions
        // through this listing (those belong to the descendants' own
        // surface). Defence-in-depth on top of the
        // `tenant_repo.find_by_id(scope, tenant_id)` visibility fence
        // above.
        let repo_scope = own_listing_repo_scope(tenant_id);
        let items = self
            .repo
            .list_own_for_tenant(
                &repo_scope,
                tenant_id,
                page_query.repo_status_filter(),
                page_query.pagination(),
            )
            .await?;
        // `total` MUST reflect the count of all matching rows under the
        // same `(tenant_id, status_filter)` predicate, NOT the current
        // page size. The cheap `count_own_for_tenant` round-trip mirrors
        // the tenant-CRUD listing contract (see
        // `repo_impl::reads::list_children`) so cursor pagination
        // (`top` / `skip`) behaves correctly when `total > top`.
        //
        // TOCTOU note: `list` and `count` are TWO independent queries.
        // On Postgres each runs at READ COMMITTED so a row committed
        // between them can make `total` differ by one from the
        // snapshot the page reflects; on `SQLite` each is its own
        // autocommit. This is the SAME asymmetry that
        // `tenant-CRUD::list_children` accepts (DESIGN §3.6) and is
        // intentional — wrapping both in a SERIALIZABLE TX would cost
        // 40001-retry cycles for a read-only listing.
        let total = self
            .repo
            .count_own_for_tenant(&repo_scope, tenant_id, page_query.repo_status_filter())
            .await?;
        Ok(OffsetPage::new(
            items,
            page_query.top(),
            page_query.skip(),
            Some(total),
        ))
    }

    /// List conversion requests inbound to `parent_id` (the parent of
    /// each converting child). Projects each row down to the minimal
    /// cross-barrier surface ([`ConversionRequestParentProjection`])
    /// per `Parent-Side Inbound-Discovery Minimal Surface` `DoD`.
    ///
    /// The repo's `list_inbound_for_parent` already restricts to
    /// `parent_id == :parent_id` (i.e. direct children only); the
    /// service layer relies on that predicate and additionally
    /// resolves the live `child_tenant_name` from the converting
    /// tenant's row so a renamed child surfaces with the current
    /// name on the parent's listing.
    ///
    /// # Errors
    ///
    /// * Any error surfaced by `repo.list_inbound_for_parent`.
    /// * `tenant_repo.find_by_id` failures are tolerated per row —
    ///   on lookup miss the projection falls back to the
    ///   `child_tenant_name` snapshot stored on the conversion row
    ///   itself, which is always populated at request time.
    // @cpt-begin:cpt-cf-account-management-flow-managed-self-managed-modes-parent-child-conversions-discovery:p1:inst-flow-parent-side-discovery-service
    pub async fn list_inbound_for_parent(
        &self,
        ctx: &SecurityContext,
        parent_id: Uuid,
        page_query: &ListConversionsQuery,
    ) -> Result<OffsetPage<ConversionRequestParentProjection>, DomainError> {
        // PEP gate FIRST: compile the caller's `SecurityContext` into
        // an `AccessScope` keyed on the URL-bound `parent_id`. A
        // denied caller surfaces as `CrossTenantDenied` BEFORE any
        // parent lookup or listing.
        let scope = self
            .authorize(ctx, pep::actions::LIST_INBOUND, parent_id)
            .await?;
        // Parent-existence guard: `list_inbound_for_parent` filters
        // `tenant_closure.parent_id = :parent_id` and would silently
        // return an empty page for a nonexistent / soft-deleted /
        // hard-deleted parent. Resolve the parent tenant first so a
        // missing parent surfaces as `NotFound` (matching the REST
        // contract) instead of a misleading `200 / empty` response.
        // The lookup uses the PEP-compiled `scope` so an out-of-scope
        // parent_id collapses to `NotFound` as well. `TenantRepo::find_by_id`
        // returns soft-deleted rows too, so reject `Deleted`
        // explicitly — a soft-deleted parent must not surface
        // historical inbound rows.
        let parent = self
            .tenant_repo
            .find_by_id(&scope, parent_id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                detail: format!("tenant {parent_id} not found"),
                resource: parent_id.to_string(),
            })?;
        if matches!(parent.status, TenantStatus::Deleted) {
            return Err(DomainError::NotFound {
                detail: format!("tenant {parent_id} not found"),
                resource: parent_id.to_string(),
            });
        }

        // Parent-side inbound listing surfaces conversions targeting
        // self-managed children that sit behind the parent's closure
        // barrier — `parent_inbound_repo_scope` builds a subtree
        // clamp on `tenant_id` with `respect_barriers = false` so
        // those rows stay visible while still pinning the listing to
        // descendants of the URL-bound parent. See the helper's
        // docstring for the full barrier-penetration rationale.
        let repo_scope = parent_inbound_repo_scope(parent_id);
        let rows = self
            .repo
            .list_inbound_for_parent(
                &repo_scope,
                parent_id,
                page_query.repo_status_filter(),
                page_query.pagination(),
            )
            .await?;
        // See `list_own_for_tenant` for the rationale on splitting
        // `count` from `list` and the TOCTOU contract that both this
        // and the sibling listing share with `tenant-CRUD::list_children`.
        let total = self
            .repo
            .count_inbound_for_parent(&repo_scope, parent_id, page_query.repo_status_filter())
            .await?;

        // Live-name resolution: one batch lookup over the unique
        // tenant ids referenced by the page, instead of one
        // `find_by_id` round-trip per row. Build a positional map and
        // fall back to the snapshot captured at request time when a
        // row is missing (tenant soft-deleted, scope-invisible).
        let unique_ids: Vec<Uuid> = {
            let mut ids: Vec<Uuid> = rows.iter().map(|r| r.tenant_id).collect();
            ids.sort_unstable();
            ids.dedup();
            ids
        };
        let live_names: HashMap<Uuid, String> = if unique_ids.is_empty() {
            HashMap::new()
        } else {
            // Tolerate `find_many` failures — a transient DB error on
            // the names lookup MUST NOT shadow the conversion-row
            // listing the parent is asking about. The snapshot path
            // covers every row in that case. The error is surfaced on
            // `am.domain` (NOT `am.events` — that channel is
            // success-only by convention; routing errors there breaks
            // downstream consumers grouping by `event` count) so a
            // degraded listing (stale names) is not invisible to
            // operators monitoring the structured log.
            //
            // # Why `allow_all` and not the caller's `scope`
            //
            // The parent-side inbound listing surfaces conversions
            // owned by self-managed children which sit behind the
            // tenant closure barrier — the parent's `scope` cannot
            // read those child rows. Re-using `scope` here would
            // make `find_many` return an empty set for every
            // self-managed child, silently dropping every parent
            // listing back onto the stale snapshot name from the
            // request row (the converting child's name at the time
            // of `request_conversion`). That is the `[P2] Bypass the
            // tenant barrier when refreshing inbound child names`
            // codex finding the doc comment promised to avoid.
            //
            // The cross-barrier read is safe here because:
            //   * the rows-to-look-up set is constrained to
            //     `row.tenant_id` values already returned by the
            //     prior `list_inbound_for_parent` repo call, which
            //     is gated by the parent's `find_by_id(scope, ..)`
            //     upstream — so the caller already proved they may
            //     list these conversions.
            //   * the lookup returns only `name` (projected into a
            //     `String` via `(t.id, t.name)`), so no closure-
            //     barrier-protected attribute leaks; the name is
            //     already exposed on the public conversion-row
            //     projection (`child_tenant_name`).
            //   * the same widening rationale documented on
            //     `approve` (`scope` ignored, repo runs at
            //     `allow_all`) applies — barrier transparency for
            //     dual-consent operations is a service-level
            //     decision, not a storage decision.
            let _ = scope;
            match self
                .tenant_repo
                .find_many(&AccessScope::allow_all(), &unique_ids)
                .await
            {
                Ok(tenants) => tenants.into_iter().map(|t| (t.id, t.name)).collect(),
                Err(err) => {
                    // Increment a counter so the silent-fallback rate is
                    // observable on dashboards (the `tracing::warn` alone
                    // would only surface in log aggregators) — operators
                    // alerting on `op = list_inbound_for_parent_name_lookup`
                    // / `outcome = degraded_snapshot_fallback` see how
                    // often parents land on stale snapshot names.
                    crate::domain::metrics::emit_metric(
                        crate::domain::metrics::AM_CONVERSION_LIFECYCLE,
                        crate::domain::metrics::MetricKind::Counter,
                        &[
                            ("op", "list_inbound_for_parent_name_lookup"),
                            ("outcome", "degraded_snapshot_fallback"),
                        ],
                    );
                    tracing::warn!(
                        target: "am.domain",
                        error = %err,
                        parent_id = %parent_id,
                        unique_ids = unique_ids.len(),
                        "list_inbound_for_parent: find_many failed; falling back to snapshot names"
                    );
                    HashMap::new()
                }
            }
        };

        let mut items: Vec<ConversionRequestParentProjection> = Vec::with_capacity(rows.len());
        for row in rows {
            let child_tenant_name = live_names
                .get(&row.tenant_id)
                .cloned()
                .unwrap_or_else(|| row.child_tenant_name.clone());
            items.push(project_to_parent_view(&row, child_tenant_name));
        }

        Ok(OffsetPage::new(
            items,
            page_query.top(),
            page_query.skip(),
            Some(total),
        ))
    }
    // @cpt-end:cpt-cf-account-management-flow-managed-self-managed-modes-parent-child-conversions-discovery:p1:inst-flow-parent-side-discovery-service

    // ----------------------------------------------------------------
    // retention
    // ----------------------------------------------------------------

    /// Soft-delete resolved (`Approved` / `Cancelled` / `Rejected` /
    /// `Expired`) rows older than `cutoff = now - retention_window`.
    /// Implements the retention half of
    /// `cpt-cf-account-management-dod-managed-self-managed-modes-conversion-expiry`.
    ///
    /// The repo owns the SQL predicate (`status != Pending AND
    /// resolved_at <= cutoff AND deleted_at IS NULL`) and the short-
    /// lived TX; the service simply derives the cutoff from the
    /// configured `now_fn` and forwards the count back to the caller.
    ///
    /// # Authorization
    ///
    /// Retention is a system-initiated background sweep —
    /// `conversion_requests` is declared `Scopable(no_tenant,
    /// no_resource)` and the entity-level scope filter is a no-op
    /// today. The `scope` parameter is therefore a
    /// [`ConversionScope`] whose discriminator MUST be
    /// [`ConversionScopeKind::SystemSweep`]; callers wire it via
    /// [`ConversionScope::system_sweep`]. A future
    /// `InTenantSubtree` (#1813) plumb will route the wrapped
    /// `AccessScope` through to the repo without changing this
    /// signature.
    ///
    /// # Errors
    ///
    /// * Any error surfaced by `repo.soft_delete_resolved_older_than`.
    // @cpt-begin:cpt-cf-account-management-dod-managed-self-managed-modes-conversion-expiry:p1:inst-dod-conversion-expiry-retention
    pub async fn soft_delete_resolved(
        &self,
        scope: &ConversionScope,
        retention_window: StdDuration,
        batch_size: u32,
    ) -> Result<u64, DomainError> {
        // Discriminator guard: this seam is system-driven only —
        // wiring it with a URL-bound scope would lie about the
        // audit envelope's `actor_kind`. Debug-asserted because the
        // wrapped `AccessScope` is `allow_all()` either way; in
        // release builds the kind discriminator is documentary.
        debug_assert!(
            matches!(scope.kind(), ConversionScopeKind::SystemSweep),
            "soft_delete_resolved: callers MUST pass ConversionScope::system_sweep(); got {:?}",
            scope.kind()
        );
        let now = self.now();
        let cutoff = now - retention_window;
        self.repo
            .soft_delete_resolved_older_than(scope.as_access_scope(), cutoff, now, batch_size)
            .await
    }
    // @cpt-end:cpt-cf-account-management-dod-managed-self-managed-modes-conversion-expiry:p1:inst-dod-conversion-expiry-retention

    // ----------------------------------------------------------------
    // approve
    // ----------------------------------------------------------------

    /// Approve a pending conversion. Counterparty-side action.
    ///
    /// Implements `cpt-cf-account-management-flow-managed-self-managed-modes-conversion-approval`
    /// in conjunction with the repo-owned
    /// [`ConversionRepo::apply_conversion_approval`] seam. The service
    /// runs the cheap pre-checks (load row, status precondition,
    /// tenant Active precondition, actor-side rule) and delegates the
    /// load-bearing single-TX apply (type re-evaluation,
    /// `tenants.self_managed` flip, closure-barrier rewrite, request
    /// transition) to the repo.
    ///
    /// On commit the service emits `conversion_approved` on
    /// `am.events` with `actor = approver_uuid`. Audit emission
    /// failure does NOT roll back the already-committed transaction.
    ///
    /// # Errors
    ///
    /// * [`DomainError::NotFound`] — `request_id` does not resolve.
    /// * [`DomainError::AlreadyResolved`] — row is in any terminal
    ///   status (status precondition precedes the actor check).
    /// * [`DomainError::Validation`] — the converting tenant OR the
    ///   parent tenant is not `Active` (parent-side precheck catches
    ///   a peer soft-delete of the parent between request and approve
    ///   that would otherwise surface as `Internal` from the apply TX).
    /// * [`DomainError::InvalidActorForTransition`] — caller side
    ///   matches the initiator side (initiator cannot approve their
    ///   own request; approve is counterparty-only).
    /// * [`DomainError::TypeNotAllowed`] — type re-evaluation
    ///   rejected the parent / child type pairing under TX.
    /// * Any DB error from the underlying transaction.
    // @cpt-begin:cpt-cf-account-management-flow-managed-self-managed-modes-conversion-approval:p1:inst-flow-conversion-approval-service
    // @cpt-begin:cpt-cf-account-management-algo-managed-self-managed-modes-dual-consent-apply:p1:inst-algo-dual-consent-apply-service
    // @cpt-begin:cpt-cf-account-management-dod-managed-self-managed-modes-dual-consent-apply:p1:inst-dod-dual-consent-apply-service
    #[allow(
        clippy::cognitive_complexity,
        reason = "flat guard sequence (URL-coherence scope -> nil-actor precondition -> state machine -> tenant/parent active preconditions -> type-stability TOCTOU -> apply TX) is the security-critical ordering reviewers eyeball-check; extracting helpers would fragment the audit chain and obscure the @cpt-* CPT markers anchored to each step"
    )]
    pub async fn approve(
        &self,
        ctx: &SecurityContext,
        request_id: Uuid,
        caller: ConversionCaller,
    ) -> Result<ConversionRequest, DomainError> {
        // PEP gate FIRST — see `cancel` for the full rationale on
        // why the caller-bound PEP authorization precedes every
        // other guard.
        let scope = self
            .authorize(ctx, pep::actions::APPROVE, caller.scope_id())
            .await?;
        let approver_uuid = ctx.subject_id();
        // See `cancel` for the rationale on the side-specific
        // `conversion_repo_scope` shape (parent-side: subtree with
        // `respect_barriers = false`, so a self-managed child's row
        // stays visible to the parent counterparty).
        let repo_scope = conversion_repo_scope(caller);
        // @cpt-begin:cpt-cf-account-management-dod-managed-self-managed-modes-dual-consent-actor-discipline:p1:inst-dod-dual-consent-actor-discipline-approve
        let row = self
            .repo
            .find_by_id(&repo_scope, request_id)
            .await?
            .ok_or_else(|| DomainError::ConversionRequestNotFound {
                detail: format!("conversion request {request_id} not found"),
                resource: request_id.to_string(),
            })?;

        // Parent-side scope verification BEFORE state / role / tenant
        // checks. See `cancel` for the rationale on why this fence runs
        // first.
        require_caller_scope_or_not_found(
            caller,
            "approve",
            row.tenant_id,
            row.parent_id,
            request_id,
        )?;

        // State-then-role validation: see `cancel` for the full
        // rationale. Approve is counterparty-only — the matrix lives
        // in `state_machine::validate_transition`, called here so the
        // service does not duplicate the role rule.
        validate_transition(
            row.status,
            ConversionStatus::Approved,
            Some(caller.side()),
            row.initiator_side,
        )?;

        // Caller-visibility fence: resolve the caller-owned tenant
        // (`row.tenant_id` for child callers, `row.parent_id` for
        // parent callers) under the incoming `AccessScope`. Mirrors
        // the symmetric fence in `cancel` / `reject` — an internal
        // actor that can mint a matching `ConversionCaller` MUST NOT
        // be able to approve a request whose caller-owned tenant is
        // outside their `AccessScope`. Runs BEFORE the converting-
        // tenant load below so an out-of-scope caller collapses to
        // `NotFound` before the structural tenant lookup leaks
        // existence.
        self.require_caller_tenant_visible(&scope, caller, &row, "approve")
            .await?;

        // Tenant precondition runs after the state + role validation
        // so a wrong-actor or already-resolved request fails fast
        // without an extra `find_by_id` round-trip on the tenant.
        // The repo re-checks Active inside the apply transaction; this
        // is a cheap fence so the common-case rejection short-circuits
        // before the SERIALIZABLE TX opens.
        //
        // # Tenant load uses `allow_all`
        //
        // The converting tenant (`row.tenant_id`) is the child. For
        // parent-side approval a self-managed child sits behind the
        // closure barrier and is invisible to the parent's barrier-
        // respecting clamp on the `tenants` entity (`tenants` declares
        // `resource_col = "id"`). Approving a self-managed → managed
        // conversion is exactly the case where the parent counterparty
        // needs to act on a tenant they cannot directly see, so the
        // structural tenant load uses `allow_all`. The authz check on
        // the conversion row itself is carried by
        // `require_caller_scope_or_not_found` (URL coherence),
        // `require_caller_tenant_visible` (caller-owned tenant
        // visibility), and the `conversion_repo_scope(caller)` clamp
        // used on `repo.find_by_id` / `apply_conversion_approval` —
        // for parent-side the conversion clamp uses
        // `respect_barriers = false`, so the conversion row stays
        // visible while the converting tenant's own row remains
        // outside the parent's scope (which is the correct surface
        // posture: the parent acts on the request, not directly on
        // the child tenant).
        let tenant = self
            .tenant_repo
            .find_by_id(&AccessScope::allow_all(), row.tenant_id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                detail: format!("tenant {} not found", row.tenant_id),
                resource: row.tenant_id.to_string(),
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
        // Parent-side precheck mirrors the converting-tenant check
        // above. A peer soft-delete of the parent between request
        // and approve is a recoverable user-state event (the parent
        // row still exists with `deleted_at` set), not a system
        // fault — the apply TX would otherwise surface this case as
        // `Internal` (HTTP 500) at `conversion.rs:apply_conversion_approval`
        // because the TX-side reload uses a `deleted_at IS NULL`
        // filter that turns soft-deleted parents into a "disappeared"
        // diagnostic. Catching it here keeps the boundary symmetric
        // with the converting-tenant check and maps the failure to
        // a clean `Validation` (HTTP 400).
        let parent_id = row.parent_id.ok_or_else(|| DomainError::Internal {
            diagnostic: format!(
                "conversion {request_id}: parent_id missing on pending row; \
                         root-tenant guard should have rejected this earlier"
            ),
            cause: None,
        })?;
        let parent = self
            .tenant_repo
            .find_by_id(&AccessScope::allow_all(), parent_id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                detail: format!("parent tenant {parent_id} not found"),
                resource: parent_id.to_string(),
            })?;
        if !matches!(parent.status, TenantStatus::Active) {
            return Err(DomainError::Validation {
                detail: format!(
                    "parent tenant {} is not active (status={})",
                    parent.id,
                    parent.status.as_str()
                ),
            });
        }
        // @cpt-end:cpt-cf-account-management-dod-managed-self-managed-modes-dual-consent-actor-discipline:p1:inst-dod-dual-consent-actor-discipline-approve

        // Pre-apply type compatibility barrier. Runs at the domain
        // layer so the infrastructure adapter has no direct dependency
        // on `TenantTypeChecker`. A `TypeNotAllowed` rejection short-
        // circuits BEFORE the SERIALIZABLE apply TX opens; the repo
        // never sees the conversion if types don't pair up.
        //
        // TOCTOU coverage: the `tenant_type_uuid` observed on both
        // tenants here is the value the apply TX MUST still see
        // inside the SERIALIZABLE TX. The repo receives both values
        // via `ApplyConversionApprovalInput::expected_*` and aborts
        // with `Validation` if either tenant flips type between this
        // check and the apply — surfacing the race as a recoverable
        // user-state event instead of silently approving against a
        // stale pairing.
        let expected_tenant_type_uuid = tenant.tenant_type_uuid;
        let expected_parent_tenant_type_uuid = parent.tenant_type_uuid;
        self.tenant_type_checker
            .check_parent_child(expected_parent_tenant_type_uuid, expected_tenant_type_uuid)
            .await?;

        let approved = self
            .repo
            .apply_conversion_approval(
                &repo_scope,
                ApplyConversionApprovalInput {
                    request_id,
                    target_tenant_id: row.tenant_id,
                    target_mode: row.target_mode,
                    expected_tenant_type_uuid,
                    expected_parent_tenant_type_uuid,
                    approver_uuid,
                    resolved_at: self.now(),
                },
            )
            .await?;

        // Post-commit audit. Failure here MUST NOT roll back.
        tracing::info!(
            target: "am.events",
            event = "conversion_approved",
            request_id = %approved.id,
            tenant_id = %approved.tenant_id,
            caller_side = caller.side().as_str(),
            actor_uuid = %approver_uuid,
            target_mode = approved.target_mode.as_str(),
            outcome = "ok",
            "am conversion approved"
        );

        Ok(approved)
    }
    // @cpt-end:cpt-cf-account-management-dod-managed-self-managed-modes-dual-consent-apply:p1:inst-dod-dual-consent-apply-service
    // @cpt-end:cpt-cf-account-management-algo-managed-self-managed-modes-dual-consent-apply:p1:inst-algo-dual-consent-apply-service
    // @cpt-end:cpt-cf-account-management-flow-managed-self-managed-modes-conversion-approval:p1:inst-flow-conversion-approval-service

    // ----------------------------------------------------------------
    // expire_pending — system-driven reaper tick
    // ----------------------------------------------------------------

    /// Reaper tick. Discovers `Pending` rows whose `expires_at` is in
    /// the past, transitions each to `Expired`, and emits one
    /// `conversion_expired` audit event per row with `actor = system`
    /// on `am.events`. Returns the number of rows transitioned.
    ///
    /// The reaper MUST NOT mutate `tenants.self_managed` and MUST NOT
    /// touch closure rows — expire is purely a status transition on
    /// the conversion-request row.
    ///
    /// Idempotent: re-running after every expiration has been applied
    /// returns `0` and emits no further events.
    ///
    /// # Authorization
    ///
    /// Same posture as [`Self::soft_delete_resolved`] — the reaper
    /// is system-driven. The `scope` parameter is a
    /// [`ConversionScope`] whose discriminator MUST be
    /// [`ConversionScopeKind::SystemSweep`]; callers wire it via
    /// [`ConversionScope::system_sweep`].
    ///
    /// # Errors
    ///
    /// * Any error surfaced by `repo.query_expired` (the scan itself
    ///   is fail-fast — without the scan there is nothing to drive).
    /// * Per-row failures from `repo.transition_pending_to_expired`
    ///   are logged on `am.domain` and SKIPPED (best-effort batch);
    ///   the next reaper tick re-scans and re-attempts the leftovers.
    // @cpt-begin:cpt-cf-account-management-algo-managed-self-managed-modes-conversion-expiry-reaper:p1:inst-algo-conversion-expiry-reaper-service
    // @cpt-begin:cpt-cf-account-management-dod-managed-self-managed-modes-conversion-expiry:p1:inst-dod-conversion-expiry-reaper
    #[allow(
        clippy::cognitive_complexity,
        reason = "best-effort batch reaper: three per-row outcome arms (Ok / AlreadyResolved skip / failure-skip) each emit a distinct structured log on a different channel (am.events success, am.events idempotent skip, am.domain transient failure); collapsing the arms would obscure the per-outcome logging contract"
    )]
    pub async fn expire_pending(
        &self,
        scope: &ConversionScope,
        batch_size: u32,
        cancel: &CancellationToken,
    ) -> Result<usize, DomainError> {
        // The wrapped `AccessScope` is forwarded to the repo's
        // `query_expired` / `transition_pending_to_expired` calls
        // below. Under the current scope contract on
        // `conversion_requests` (`Scopable(no_tenant, no_resource,
        // no_owner, no_type)`) it is a no-op; once `InTenantSubtree`
        // (#1813) plumbs scope columns the same wrapped scope flows
        // through unchanged.
        //
        // `cancel` is the same `CancellationToken` the surrounding
        // cleanup-loop selects on. The reaper polls it between rows
        // so a shutdown signal stops the sweep mid-batch instead of
        // forcing the runtime to wait for the in-flight batch to
        // drain — the next process restart re-scans the leftovers.
        // Callers outside a lifecycle loop (one-shot test calls,
        // ad-hoc admin sweeps) pass a fresh `CancellationToken::new()`
        // that is never tripped.
        //
        // Discriminator guard: this seam is system-driven only,
        // mirroring [`Self::soft_delete_resolved`]. A URL-bound
        // scope would lie about the `actor_kind` on the
        // `conversion_expired` audit envelope.
        debug_assert!(
            matches!(scope.kind(), ConversionScopeKind::SystemSweep),
            "expire_pending: callers MUST pass ConversionScope::system_sweep(); got {:?}",
            scope.kind()
        );
        let now = self.now();
        let due = self
            .repo
            .query_expired(scope.as_access_scope(), now, batch_size)
            .await?;
        let mut transitioned: usize = 0;
        let mut failed: usize = 0;
        let due_total = due.len();
        // Single `now` stamp shared across every row in the batch
        // (previously re-sampled per row inside the loop). The
        // reaper-tick semantics are "rows in this batch expired
        // together at the tick instant" — re-sampling per row
        // produced a `resolved_at` skew across a single tick that
        // had no observable benefit (the batch is bounded by
        // `expire_batch_size`, well below the wall-clock resolution
        // a slow per-row UPDATE would observe), while letting tests
        // assert deterministic `resolved_at` ordering across a
        // batch. Matches the documented "each tick is a single
        // wall-clock moment" semantics in the FEATURE doc.
        let batch_stamp = self.now();
        for row in due {
            // Cancellation between rows: a runtime shutdown signal
            // exits the per-row loop without trying to drain the rest
            // of the batch. Whatever has already transitioned in
            // earlier iterations stays committed (the repo owns the
            // per-row TX); the leftover rows are picked up by the
            // next reaper tick after restart.
            if cancel.is_cancelled() {
                tracing::info!(
                    target: "am.lifecycle",
                    op = "expire_pending",
                    transitioned,
                    remaining = due_total.saturating_sub(transitioned + failed),
                    "expire_pending cancelled mid-batch; leftovers deferred to next tick"
                );
                break;
            }
            match self
                .repo
                .transition_pending_to_expired(scope.as_access_scope(), row.id, batch_stamp)
                .await
            {
                Ok(updated) => {
                    transitioned += 1;
                    // System-driven transition has no `actor_uuid`
                    // (the FEATURE doc audit envelope reserves
                    // `actor_uuid` for caller-issued UUIDs only).
                    // Emit `actor_kind = "system"` instead so
                    // structured-log aggregators that index
                    // `actor_uuid` see a single, uniform UUID type
                    // across `am.events` rather than a string-typed
                    // sentinel that breaks the index.
                    tracing::info!(
                        target: "am.events",
                        event = "conversion_expired",
                        request_id = %updated.id,
                        tenant_id = %updated.tenant_id,
                        actor_kind = "system",
                        outcome = "ok",
                        "am conversion expired"
                    );
                }
                Err(DomainError::AlreadyResolved) => {
                    // Peer reaper / approve / cancel / reject won
                    // this row between scan and transition. Idempotent
                    // skip; do not surface as an error to the caller.
                    tracing::debug!(
                        target: "am.events",
                        event = "conversion_expired",
                        request_id = %row.id,
                        tenant_id = %row.tenant_id,
                        outcome = "skipped_already_resolved",
                        "am conversion expire skipped"
                    );
                }
                Err(DomainError::ConversionRequestNotFound { .. }) => {
                    // Row vanished between scan and transition — most
                    // commonly the tenant was hard-deleted (FK cascade)
                    // or a peer retention sweep soft-deleted the row.
                    // Either way the absent state is success-equivalent
                    // for the expire-pending pipeline: there is no row
                    // to transition, so the loop continues with the
                    // next scanned id. Mirrors the
                    // `AlreadyResolved` idempotent-skip branch above
                    // and keeps the `failed` counter (which feeds the
                    // dashboard predicate below) from triggering an
                    // escalation warn on what is benign concurrency.
                    tracing::debug!(
                        target: "am.events",
                        event = "conversion_expired",
                        request_id = %row.id,
                        tenant_id = %row.tenant_id,
                        outcome = "skipped_not_found",
                        "am conversion expire skipped"
                    );
                }
                Err(other) => {
                    // Best-effort batch: a transient per-row failure
                    // (DB blip, SI conflict surfacing as Aborted, etc.)
                    // MUST NOT strand rows N+1..N. Log on `am.domain`
                    // (errors do not belong on the success-only
                    // `am.events` channel) and continue with the next
                    // row. Increment a counter so dashboards can tell
                    // "this tick had N due rows but K of them failed"
                    // apart from "no rows were due" — the caller's
                    // `Ok(transitioned)` return cannot distinguish the
                    // two without this metric. The next tick re-scans
                    // and re-attempts the leftovers.
                    failed += 1;
                    crate::domain::metrics::emit_metric(
                        crate::domain::metrics::AM_CONVERSION_LIFECYCLE,
                        crate::domain::metrics::MetricKind::Counter,
                        &[("op", "expire_pending"), ("outcome", "per_row_failure")],
                    );
                    tracing::warn!(
                        target: "am.domain",
                        error = %other,
                        request_id = %row.id,
                        tenant_id = %row.tenant_id,
                        "expire_pending: per-row transition failed; skipping for next tick"
                    );
                }
            }
        }
        // Escalate to a structured warn when half-or-more of the due
        // batch fails per-row. The previous predicate
        // (`transitioned == 0 && failed > 0`) only fired when 100% of
        // due rows failed and missed the 99%-failure case where a
        // single row succeeded — that asymmetry hides a degraded
        // backend until every retry is exhausted. The `2 * failed >=
        // due_total` form fires at or above 50% failure rate
        // (integer-safe; no division). It is inclusive at the
        // exact-50% point so a `due_total = 2, failed = 1` tick
        // still alerts; this is deliberate — for small batches the
        // safer posture is to alert at parity rather than wait for
        // strict-majority confirmation. The lower bound `failed > 0
        // && due_total > 0` keeps quiet ticks silent.
        if failed > 0 && due_total > 0 && failed.saturating_mul(2) >= due_total {
            tracing::warn!(
                target: "am.lifecycle",
                op = "expire_pending",
                due_total,
                failed,
                transitioned,
                "expire_pending tick saw half-or-more per-row failures (2 * failed >= due_total); \
                 see preceding `am.domain` per-row warns for causes"
            );
        }
        Ok(transitioned)
    }
    // @cpt-end:cpt-cf-account-management-dod-managed-self-managed-modes-conversion-expiry:p1:inst-dod-conversion-expiry-reaper
    // @cpt-end:cpt-cf-account-management-algo-managed-self-managed-modes-conversion-expiry-reaper:p1:inst-algo-conversion-expiry-reaper-service
}

/// Project a full [`ConversionRequest`] down to the parent-side
/// minimal surface. Centralised here so the projection contract is
/// in one place and the unit tests can pin the visible field set
/// against the model row directly.
// @cpt-begin:cpt-cf-account-management-dod-managed-self-managed-modes-parent-side-minimal-surface:p1:inst-dod-parent-side-projection-mapping
fn project_to_parent_view(
    row: &ConversionRequest,
    child_tenant_name: String,
) -> ConversionRequestParentProjection {
    ConversionRequestParentProjection {
        request_id: row.id,
        tenant_id: row.tenant_id,
        child_tenant_name,
        initiator_side: row.initiator_side,
        target_mode: row.target_mode,
        status: row.status,
        requested_by: row.requested_by,
        approved_by: row.approved_by,
        cancelled_by: row.cancelled_by,
        rejected_by: row.rejected_by,
        created_at: row.requested_at,
        expires_at: row.expires_at,
        resolved_at: row.resolved_at,
    }
}
// @cpt-end:cpt-cf-account-management-dod-managed-self-managed-modes-parent-side-minimal-surface:p1:inst-dod-parent-side-projection-mapping

/// Enforce the caller-scope contract documented on
/// [`ConversionCaller`]: every caller MUST be acting on a request
/// whose stored fields match the caller's declared scope.
///
/// * Child-side: `row.tenant_id == caller.scope_id` (the URL-bound
///   tenant from `/tenants/{tenant_id}/conversions`).
/// * Parent-side: `row.parent_id == Some(caller.scope_id)` (the
///   URL-bound parent from `/tenants/{parent_id}/child-conversions`).
///
/// Both checks fire BEFORE the state / role matrix so a misrouted
/// call cannot learn that a request exists by reading
/// `AlreadyResolved` or `NotFound` on a row outside its scope. `op`
/// is included verbatim in `detail` so the structured log on the
/// caller side disambiguates which entry point fired
/// (`request_conversion` / `cancel` / etc.). Every violation surfaces
/// as [`DomainError::Validation`] — the same envelope the rest of the
/// initiation guards use.
///
/// A parent-side row whose stored `parent_id` is `None` (i.e. the
/// row references the platform root, which the FEATURE-doc root-
/// tenant refusal blocks at initiation time) will surface as a
/// `Validation` here too with a distinct diagnostic so operators
/// reading logs can recognize the data-integrity tag rather than
/// confusing it with a regular caller-scope mismatch.
// @cpt-begin:cpt-cf-account-management-dod-managed-self-managed-modes-dual-consent-actor-discipline:p1:inst-dod-dual-consent-actor-discipline-caller-scope
// @cpt-begin:cpt-cf-account-management-flow-managed-self-managed-modes-conversion-approval:p1:inst-flow-appr-validate-caller
// @cpt-begin:cpt-cf-account-management-flow-managed-self-managed-modes-conversion-cancellation:p1:inst-flow-can-validate-caller
// @cpt-begin:cpt-cf-account-management-flow-managed-self-managed-modes-conversion-rejection:p1:inst-flow-rej-validate-caller
// @cpt-begin:cpt-cf-account-management-flow-managed-self-managed-modes-conversion-initiation:p1:inst-flow-init-validate-caller
fn verify_caller_scope(
    caller: ConversionCaller,
    op: &'static str,
    row_tenant_id: Uuid,
    row_parent_id: Option<Uuid>,
) -> Result<(), DomainError> {
    let scope_id = caller.scope_id();
    match caller.side() {
        ConversionSide::Child => {
            if row_tenant_id == scope_id {
                Ok(())
            } else {
                Err(DomainError::Validation {
                    detail: format!(
                        "{op}: child-side caller scoped to {scope_id} cannot act on a request \
                         whose tenant_id is {row_tenant_id}"
                    ),
                })
            }
        }
        ConversionSide::Parent => match row_parent_id {
            Some(p) if p == scope_id => Ok(()),
            // Stored `parent_id == None` should be impossible by
            // construction (root-tenant refusal runs before insert),
            // but if a peer raw-SQL'ed such a row in we MUST surface
            // it as a distinct diagnostic and not as a legitimate
            // scope mismatch.
            None => Err(DomainError::Validation {
                detail: format!(
                    "{op}: parent-side caller scoped to {scope_id} cannot act on a request \
                     with NULL parent_id (data-integrity violation: root-tenant refusal \
                     should have blocked the insert)"
                ),
            }),
            Some(other) => Err(DomainError::Validation {
                detail: format!(
                    "{op}: parent-side caller scoped to {scope_id} cannot act on a request \
                     whose parent_id is {other}"
                ),
            }),
        },
    }
}
// @cpt-end:cpt-cf-account-management-flow-managed-self-managed-modes-conversion-initiation:p1:inst-flow-init-validate-caller
// @cpt-end:cpt-cf-account-management-flow-managed-self-managed-modes-conversion-rejection:p1:inst-flow-rej-validate-caller
// @cpt-end:cpt-cf-account-management-flow-managed-self-managed-modes-conversion-cancellation:p1:inst-flow-can-validate-caller
// @cpt-end:cpt-cf-account-management-flow-managed-self-managed-modes-conversion-approval:p1:inst-flow-appr-validate-caller
// @cpt-end:cpt-cf-account-management-dod-managed-self-managed-modes-dual-consent-actor-discipline:p1:inst-dod-dual-consent-actor-discipline-caller-scope

/// Run [`verify_caller_scope`] and, on scope mismatch, **normalize
/// the surface error to [`DomainError::NotFound`] keyed on
/// `resource_id`** so that an out-of-scope caller cannot probe
/// resource existence through the error-code channel (existing-but-
/// foreign vs nonexistent collapse to the same response).
///
/// The data-integrity branch of `verify_caller_scope` (parent-side
/// caller acting on a row whose stored `parent_id == None`,
/// indicating that root-tenant refusal was bypassed by an out-of-
/// band insert) is logged at `warn` level here and likewise
/// surfaces as `NotFound` to the caller — operator audit gets the
/// signal via logs without leaking the corruption to a potentially
/// untrusted caller.
fn require_caller_scope_or_not_found(
    caller: ConversionCaller,
    op: &'static str,
    row_tenant_id: Uuid,
    row_parent_id: Option<Uuid>,
    resource_id: Uuid,
) -> Result<(), DomainError> {
    match verify_caller_scope(caller, op, row_tenant_id, row_parent_id) {
        Ok(()) => Ok(()),
        Err(DomainError::Validation { detail }) => {
            // `am.domain` is the established AM operational/diagnostic
            // channel (alongside `am.events`, `am.idp`, `am.user.audit`,
            // `am.bootstrap`, `am.tenant.saga`). Reusing it here keeps
            // every scope-mismatch warn in one place for operator grep
            // instead of fragmenting routing across a one-off
            // `am.conversion.audit` channel.
            tracing::warn!(
                target: "am.domain",
                op,
                resource_id = %resource_id,
                detail = %detail,
                "scope mismatch normalized to NotFound to avoid existence-leak"
            );
            Err(DomainError::ConversionRequestNotFound {
                detail: format!(
                    "{op}: resource {resource_id} not found or not accessible to the caller"
                ),
                resource: resource_id.to_string(),
            })
        }
        Err(other) => Err(other),
    }
}

/// Build the [`AccessScope`] the [`ConversionRepo`] runs at for a given
/// URL-bound [`ConversionCaller`].
///
/// `conversion_requests` declares `tenant_col = "tenant_id"` and
/// `resource_col = "id"`, so the secure-extension layer clamps both
/// columns at the database. The shape we return is side-specific:
///
/// * **Child-side caller**: clamp `tenant_id = child_id`. The URL binds
///   the converting tenant; a child-side caller acting on any other
///   tenant's request collapses to a `WHERE false` at the repo and
///   surfaces as `NotFound` — second-line enforcement on top of the
///   service-layer `require_caller_scope_or_not_found` URL-coherence
///   check.
///
/// * **Parent-side caller**: clamp `tenant_id IN closure(parent_id)`
///   with `respect_barriers = false`. A parent acting as counterparty
///   on a self-managed child whose closure barrier is `1` MUST still
///   see the conversion row — the dual-consent flows are precisely
///   where barrier penetration is correct, because the request lives
///   under the parent's URL authority even though the converting
///   child is invisible to a barrier-respecting `AccessScope`. The
///   subtree-clamp narrows the caller to descendants of the URL-bound
///   parent (which the closure invariants guarantee includes every
///   conversion-request `tenant_id` whose `parent_id` is that parent);
///   without `respect_barriers = false` the clamp would silently drop
///   every conversion targeting a self-managed child.
///
/// The returned scope is consumed by every conversion-row touching
/// repo call in `cancel` / `reject` / `approve`. INSERT paths
/// (`request_conversion`) and system-driven sweeps (`expire_pending` /
/// `soft_delete_resolved`) continue to use `scope_unchecked` /
/// [`AccessScope::allow_all`] respectively per the entity-level
/// contract documented above.
///
/// The function takes no `tenant_repo` and performs no IO — it composes
/// values from `caller.scope_id()` and the secure-property constants
/// only, so it stays a pure helper.
fn conversion_repo_scope(caller: ConversionCaller) -> AccessScope {
    let root = caller.scope_id();
    let filter = match caller.side() {
        ConversionSide::Child => {
            // Equality on `tenant_id`: the URL binds exactly one
            // converting tenant, and the row's `tenant_id` MUST match
            // it. `In` over a one-element set is the canonical shape
            // for the secure-extension layer (mirrors
            // `AccessScope::for_tenant`, but kept explicit here so the
            // companion `Parent` arm reads symmetrically).
            ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![root])
        }
        ConversionSide::Parent => {
            // `respect_barriers = false` is the load-bearing knob.
            // Without it, a self-managed child's `tenant_closure`
            // edge from the parent has `barrier = 1` and would be
            // filtered out, collapsing the parent's counterparty
            // action / inbound listing to `NotFound` on exactly the
            // case the dual-consent flows are designed for.
            ScopeFilter::InTenantSubtree(InTenantSubtreeScopeFilter::with_respect_barriers(
                pep_properties::OWNER_TENANT_ID,
                root,
                false,
            ))
        }
    };
    AccessScope::single(ScopeConstraint::new(vec![filter]))
}

/// Build the [`AccessScope`] the [`ConversionRepo`] runs at for the
/// own-tenant listing surface
/// ([`ConversionService::list_own_for_tenant`] /
/// `ConversionRepo::count_own_for_tenant`).
///
/// The converting tenant lists its own conversions, so the repo
/// clamp is the flat-equality shape `tenant_id = tenant_id` —
/// mirrors the child-side branch of [`conversion_repo_scope`] but
/// without a [`ConversionCaller`] discriminator (the listing surface
/// takes a bare `tenant_id`).
fn own_listing_repo_scope(tenant_id: Uuid) -> AccessScope {
    AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::in_uuids(
        pep_properties::OWNER_TENANT_ID,
        vec![tenant_id],
    )]))
}

/// Build the [`AccessScope`] the [`ConversionRepo`] runs at for the
/// parent-side inbound listing surface
/// ([`ConversionService::list_inbound_for_parent`] /
/// [`ConversionService::count_inbound_for_parent`]).
///
/// Same shape as the parent-side branch of [`conversion_repo_scope`]:
/// `tenant_id IN closure(parent_id)` with `respect_barriers = false`,
/// so the listing surfaces conversions targeting self-managed children
/// (which sit behind the parent's barrier and would otherwise be
/// filtered out). The doc-comment justification for barrier penetration
/// is the same as on [`conversion_repo_scope`]; this helper exists as a
/// separate seam because the listing methods take a bare `parent_id`
/// without the [`ConversionCaller`] discriminator.
fn parent_inbound_repo_scope(parent_id: Uuid) -> AccessScope {
    AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::InTenantSubtree(
        InTenantSubtreeScopeFilter::with_respect_barriers(
            pep_properties::OWNER_TENANT_ID,
            parent_id,
            false,
        ),
    )]))
}
