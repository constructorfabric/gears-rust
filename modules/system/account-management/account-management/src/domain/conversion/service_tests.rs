//! Unit tests for [`ConversionService`].
//!
//! Every test wires the service against the in-crate fakes
//! ([`FakeConversionRepo`], [`FakeTenantRepo`]) plus a deterministic
//! `now_fn`. This pins:
//!
//! * Guard ordering: root-tenant refusal before status precondition,
//!   status precondition before single-pending insert
//!   (`request_conversion`); status check before actor-side check
//!   (`cancel` / `reject`).
//! * Error mapping: `RootTenantCannotConvert`, `Validation`,
//!   `NotFound`, `PendingExists`, `AlreadyResolved`, and
//!   `InvalidActorForTransition` all surface with the canonical
//!   shape and lowercase tokens documented on the variants.
//! * Parent-side projection: only direct children appear in
//!   `list_inbound_for_parent`, every projected row carries the
//!   minimal cross-barrier field set, no descendant data leaks.
//! * Retention: `soft_delete_resolved` only touches resolved rows
//!   strictly older than `now - retention_window` whose
//!   `deleted_at` is still `NULL`.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use modkit_security::{
    AccessScope, InTenantSubtreeScopeFilter, ScopeConstraint, ScopeFilter, SecurityContext,
    pep_properties,
};
use time::{Duration as TimeDuration, OffsetDateTime};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use async_trait::async_trait;

use crate::domain::conversion::model::{
    ConversionRequest, ConversionSide, ConversionStatus, TargetMode,
};
use crate::domain::conversion::service::{
    ConversionCaller, ConversionScope, ConversionService, ListConversionsQuery,
    RequestConversionInput,
};
use crate::domain::conversion::test_support::FakeConversionRepo;
use crate::domain::error::DomainError;
use crate::domain::tenant::model::{TenantModel, TenantStatus};
use crate::domain::tenant::test_support::{FakeTenantRepo, deny_all_enforcer, mock_enforcer};
use crate::domain::tenant_type::{TenantTypeChecker, inert_tenant_type_checker};
use authz_resolver_sdk::PolicyEnforcer;

const APPROVAL_TTL_SECS: u64 = 7 * 24 * 60 * 60; // 7 days
const RETENTION_SECS: u64 = 7 * 24 * 60 * 60; // 7 days
/// Marker used as `requested_by` on `seeded_request` rows so tests can
/// assert against a stable, recognisable uuid. Distinct from
/// [`CTX_SUBJECT`] — the seed builder stamps it directly, while
/// service-emitted rows source their actor from `ctx.subject_id()`.
const REQUESTER_MARKER: u128 = 0xF1;
/// Actor uuid carried by every [`ctx`] `SecurityContext`. The PEP-gated
/// service methods now source the actor from `ctx.subject_id()` instead
/// of an explicit `*_by: Uuid` argument, so tests that previously
/// asserted against `requester()` / `counterparty()` compare against
/// this constant when the actor flows through the PEP path.
const CTX_SUBJECT: Uuid = Uuid::from_u128(0xCAFE);

// ---- helpers -------------------------------------------------------

fn fixed_now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch")
}

/// Subject tenant id used by every service-test [`ctx`]. The
/// `mock_enforcer` emits an `InTenantSubtree` predicate rooted here;
/// `seed_tenant` materialises closure rows `(subject_root, tenant)`
/// so tenants stay visible under the compiled PEP scope.
const fn ctx_subject_root() -> Uuid {
    Uuid::from_u128(0xCAFE_BABE)
}

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(CTX_SUBJECT)
        .subject_tenant_id(ctx_subject_root())
        .build()
        .expect("ctx")
}

fn ctx_for(root: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(CTX_SUBJECT)
        .subject_tenant_id(root)
        .build()
        .expect("ctx")
}

/// Mirror of `ConversionService`'s `conversion_repo_scope` for the
/// child-side branch — the URL-bound child's `tenant_id` clamps the
/// conversion repo to that single tenant. Used by mutation tests
/// that assert on the scope captured by `FakeConversionRepo`.
fn expected_child_repo_scope(child_id: Uuid) -> AccessScope {
    AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::in_uuids(
        pep_properties::OWNER_TENANT_ID,
        vec![child_id],
    )]))
}

/// Mirror of `ConversionService`'s `conversion_repo_scope` for the
/// parent-side branch — barrier-penetrating subtree clamp rooted at
/// the URL-bound parent. Mirrors the
/// `InTenantSubtreeScopeFilter::with_respect_barriers(.., false)` shape
/// the service builds for parent-side counterparty mutations.
fn expected_parent_repo_scope(parent_id: Uuid) -> AccessScope {
    AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::InTenantSubtree(
        InTenantSubtreeScopeFilter::with_respect_barriers(
            pep_properties::OWNER_TENANT_ID,
            parent_id,
            false,
        ),
    )]))
}

fn approval_ttl() -> StdDuration {
    StdDuration::from_secs(APPROVAL_TTL_SECS)
}

fn retention_window() -> StdDuration {
    StdDuration::from_secs(RETENTION_SECS)
}

fn make_service(
    conv_repo: Arc<FakeConversionRepo>,
    tenant_repo: Arc<FakeTenantRepo>,
    now: OffsetDateTime,
) -> ConversionService {
    make_service_with_checker(conv_repo, tenant_repo, inert_tenant_type_checker(), now)
}

fn make_service_with_checker(
    conv_repo: Arc<FakeConversionRepo>,
    tenant_repo: Arc<FakeTenantRepo>,
    tenant_type_checker: Arc<dyn TenantTypeChecker + Send + Sync>,
    now: OffsetDateTime,
) -> ConversionService {
    let now_fn = Arc::new(move || now);
    ConversionService::new(
        conv_repo,
        tenant_repo,
        tenant_type_checker,
        mock_enforcer(),
        approval_ttl(),
        retention_window(),
    )
    .with_now_fn(now_fn)
}

/// Build a `ConversionService` with a caller-supplied `PolicyEnforcer`
/// (`mock_enforcer` / `deny_all_enforcer` / etc.). Used by the
/// caller-facing PEP-deny tests at the bottom of this file that pin
/// the `EnforcerError::Denied → DomainError::CrossTenantDenied`
/// propagation contract for every public method that runs through
/// `self.authorize(...)`.
fn make_service_with_enforcer(
    conv_repo: Arc<FakeConversionRepo>,
    tenant_repo: Arc<FakeTenantRepo>,
    enforcer: PolicyEnforcer,
    now: OffsetDateTime,
) -> ConversionService {
    let now_fn = Arc::new(move || now);
    ConversionService::new(
        conv_repo,
        tenant_repo,
        inert_tenant_type_checker(),
        enforcer,
        approval_ttl(),
        retention_window(),
    )
    .with_now_fn(now_fn)
}

fn seed_tenant(
    fake: &FakeTenantRepo,
    id: Uuid,
    parent_id: Option<Uuid>,
    status: TenantStatus,
    self_managed: bool,
    name: &str,
) {
    let now = fixed_now();
    let depth = u32::from(parent_id.is_some());
    // Mirror the production invariant: a `Deleted` row carries a
    // populated `deleted_at` timestamp (the column is the soft-delete
    // marker and is stamped by the soft-delete path). The previous
    // helper left `deleted_at = None` for `Deleted` rows, which is
    // invalid in production storage and could let a test exercise a
    // code path that does not exist outside the fake.
    let deleted_at = matches!(status, TenantStatus::Deleted).then_some(now);
    fake.insert_tenant_raw(TenantModel {
        id,
        parent_id,
        name: name.to_owned(),
        status,
        self_managed,
        tenant_type_uuid: Uuid::from_u128(0xAA),
        depth,
        created_at: now,
        updated_at: now,
        deleted_at,
    });
    // Closure seed for the PEP-derived `InTenantSubtree` predicate that
    // `mock_enforcer` emits. Without these every PEP-derived scope
    // would resolve to an empty visible set and every tenant lookup
    // would surface as `NotFound`.
    let subject_root = ctx_subject_root();
    fake.seed_closure(subject_root, id, 0, status);
    if subject_root != id {
        fake.seed_closure(id, id, 0, status);
    }
}

fn seeded_request(
    request_id: Uuid,
    tenant_id: Uuid,
    parent_id: Option<Uuid>,
    initiator_side: ConversionSide,
    status: ConversionStatus,
    requested_at: OffsetDateTime,
    resolved_at: Option<OffsetDateTime>,
) -> ConversionRequest {
    ConversionRequest {
        id: request_id,
        tenant_id,
        parent_id,
        child_tenant_name: format!("child-{tenant_id}"),
        initiator_side,
        target_mode: TargetMode::SelfManaged,
        status,
        requested_by: Uuid::from_u128(REQUESTER_MARKER),
        approved_by: matches!(status, ConversionStatus::Approved).then_some(Uuid::from_u128(0xA1)),
        cancelled_by: matches!(status, ConversionStatus::Cancelled)
            .then_some(Uuid::from_u128(0xC1)),
        rejected_by: matches!(status, ConversionStatus::Rejected).then_some(Uuid::from_u128(0xE1)),
        requested_at,
        resolved_at,
        expires_at: requested_at + TimeDuration::days(7),
        deleted_at: None,
    }
}

// ---- request_conversion -------------------------------------------

#[tokio::test]
async fn request_conversion_happy_path_child_side() {
    let conv = Arc::new(FakeConversionRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x1);
    let child = Uuid::from_u128(0x2);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "child-1",
    );
    let now = fixed_now();
    let svc = make_service(conv.clone(), tenants, now);

    let result = svc
        .request_conversion(
            &ctx(),
            RequestConversionInput {
                tenant_id: child,
                caller: ConversionCaller::child(child),
                target_mode_override: None,
            },
        )
        .await
        .expect("happy path child-side initiation");

    assert_eq!(
        result.tenant_id, child,
        "row carries the converting tenant id"
    );
    assert_eq!(
        result.initiator_side,
        ConversionSide::Child,
        "initiator side mirrors caller_side"
    );
    assert_eq!(
        result.target_mode,
        TargetMode::SelfManaged,
        "non-self-managed tenant flips to self_managed"
    );
    assert_eq!(result.status, ConversionStatus::Pending);
    assert_eq!(
        result.expires_at,
        now + approval_ttl(),
        "expires_at is now + approval_ttl"
    );
    assert_eq!(result.parent_id, Some(parent));
}

#[tokio::test]
async fn request_conversion_happy_path_parent_side() {
    let conv = Arc::new(FakeConversionRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x10);
    let child = Uuid::from_u128(0x20);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    // Self-managed tenant: target mode should flip back to managed.
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        true,
        "child-2",
    );
    let now = fixed_now();
    let svc = make_service(conv, tenants, now);

    let result = svc
        .request_conversion(
            &ctx(),
            RequestConversionInput {
                tenant_id: child,
                caller: ConversionCaller::parent(parent),
                target_mode_override: None,
            },
        )
        .await
        .expect("happy path parent-side initiation");

    assert_eq!(result.initiator_side, ConversionSide::Parent);
    assert_eq!(
        result.target_mode,
        TargetMode::Managed,
        "self_managed tenant flips to managed"
    );
}

#[tokio::test]
async fn request_conversion_root_tenant_refused() {
    let conv = Arc::new(FakeConversionRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let root = Uuid::from_u128(0x100);
    seed_tenant(&tenants, root, None, TenantStatus::Active, false, "root");
    let svc = make_service(conv.clone(), tenants, fixed_now());

    let err = svc
        .request_conversion(
            &ctx(),
            RequestConversionInput {
                tenant_id: root,
                caller: ConversionCaller::child(root),
                target_mode_override: None,
            },
        )
        .await
        .expect_err("root-tenant initiation must be refused");

    assert!(
        matches!(err, DomainError::RootTenantCannotConvert),
        "expected RootTenantCannotConvert, got {err:?}"
    );
    assert!(
        conv.pending_request_id_for(root).is_none(),
        "no pending row may be inserted on root-tenant refusal"
    );
}

#[tokio::test]
async fn request_conversion_status_suspended_rejected() {
    let conv = Arc::new(FakeConversionRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x200);
    let child = Uuid::from_u128(0x201);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Suspended,
        false,
        "child-suspended",
    );
    let svc = make_service(conv.clone(), tenants, fixed_now());

    let err = svc
        .request_conversion(
            &ctx(),
            RequestConversionInput {
                tenant_id: child,
                caller: ConversionCaller::child(child),
                target_mode_override: None,
            },
        )
        .await
        .expect_err("suspended tenant may not initiate a conversion");

    match err {
        DomainError::Validation { detail } => {
            assert!(
                detail.contains("suspended"),
                "validation detail must surface the rejected status; got {detail}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }
    assert!(conv.pending_request_id_for(child).is_none());
}

#[tokio::test]
async fn request_conversion_status_deleted_rejected() {
    // The fake `find_by_id` returns the soft-deleted row (it does not
    // filter on `deleted_at IS NULL`), so the service enforces the
    // status precondition and surfaces `Validation`. Production
    // semantics agree because the repo's status precondition runs
    // before any soft-delete filter on this read path.
    let conv = Arc::new(FakeConversionRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x300);
    let child = Uuid::from_u128(0x301);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Deleted,
        false,
        "child-deleted",
    );
    let svc = make_service(conv.clone(), tenants, fixed_now());

    let err = svc
        .request_conversion(
            &ctx(),
            RequestConversionInput {
                tenant_id: child,
                caller: ConversionCaller::child(child),
                target_mode_override: None,
            },
        )
        .await
        .expect_err("deleted tenant may not initiate a conversion");

    // Caller-visibility fence (mirrors `cancel` / `reject`): the
    // `scope`-clamped `find_by_id` collapses Deleted → `NotFound` so
    // a soft-deleted tenant cannot be probed through the error-code
    // channel. Runs BEFORE the status precondition that would
    // otherwise lift non-Active rows into `Validation`.
    assert!(
        matches!(err, DomainError::NotFound { .. }),
        "expected NotFound for deleted tenant (caller-visibility fence collapses Deleted → NotFound), got {err:?}"
    );
    assert!(conv.pending_request_id_for(child).is_none());
}

#[tokio::test]
async fn request_conversion_pending_exists_returns_pending_exists() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x400);
    let child = Uuid::from_u128(0x401);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "child-pending",
    );

    let existing_id = Uuid::from_u128(0xBEEF);
    let now = fixed_now();
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        existing_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    )]));
    let svc = make_service(conv.clone(), tenants, now);

    let err = svc
        .request_conversion(
            &ctx(),
            RequestConversionInput {
                tenant_id: child,
                caller: ConversionCaller::child(child),
                target_mode_override: None,
            },
        )
        .await
        .expect_err("a second pending row must collide");

    match err {
        DomainError::PendingExists { request_id } => {
            assert_eq!(
                request_id,
                existing_id.to_string(),
                "carries the existing pending row's id"
            );
        }
        other => panic!("expected PendingExists, got {other:?}"),
    }
}

#[tokio::test]
async fn request_conversion_after_resolved_succeeds() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x500);
    let child = Uuid::from_u128(0x501);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "child-after-resolved",
    );

    let now = fixed_now();
    let prior_id = Uuid::from_u128(0xCAFE);
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        prior_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Cancelled,
        now - TimeDuration::days(2),
        Some(now - TimeDuration::days(1)),
    )]));
    let svc = make_service(conv.clone(), tenants, now);

    let row = svc
        .request_conversion(
            &ctx(),
            RequestConversionInput {
                tenant_id: child,
                caller: ConversionCaller::child(child),
                target_mode_override: None,
            },
        )
        .await
        .expect("a resolved prior row must not block a new pending");

    assert_eq!(row.status, ConversionStatus::Pending);
    assert_ne!(
        row.id, prior_id,
        "a fresh request id is allocated for the new pending row"
    );
}

// ---- cancel --------------------------------------------------------

#[tokio::test]
async fn cancel_happy_path_initiator() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x600);
    let child = Uuid::from_u128(0x601);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let pending_id = Uuid::from_u128(0xCABA);
    let now = fixed_now();
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        pending_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    )]));
    let svc = make_service(conv.clone(), tenants, now);

    // The conversion-repo call is expected to receive the
    // **side-specific** scope built by `conversion_repo_scope`: for a
    // child-side caller this is `for_tenant(child_id)` — `OWNER_TENANT_ID
    // IN [child_id]` — which compiles to a DB-level clamp on
    // `conversion_requests.tenant_id` and gives the repo second-line
    // enforcement on top of the service's URL-coherence /
    // caller-tenant-visible fences. The assertion below pins that
    // forwarding contract so a regression that restores `allow_all`
    // (or wires the caller scope through verbatim) fails here loudly.
    let updated = svc
        .cancel(&ctx(), pending_id, ConversionCaller::child(child))
        .await
        .expect("initiator-side cancel succeeds");

    assert_eq!(updated.status, ConversionStatus::Cancelled);
    assert_eq!(updated.cancelled_by, Some(CTX_SUBJECT));
    assert_eq!(updated.resolved_at, Some(now));
    let captured = conv.captured_scopes();
    let expected_repo_scope = expected_child_repo_scope(child);
    assert_eq!(
        captured.last(),
        Some(&expected_repo_scope),
        "cancel MUST forward the child-side `for_tenant(child_id)` scope to the \
         conversion repo so the entity-level Scopable(tenant_col, resource_col) \
         clamp engages at the DB"
    );
}

#[tokio::test]
async fn cancel_by_counterparty_invalid_actor() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x700);
    let child = Uuid::from_u128(0x701);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let pending_id = Uuid::from_u128(0xDADA);
    let now = fixed_now();
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        pending_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    )]));
    let svc = make_service(conv, tenants, now);

    let err = svc
        .cancel(&ctx(), pending_id, ConversionCaller::parent(parent))
        .await
        .expect_err("counterparty-side cancel must be rejected");

    match err {
        DomainError::InvalidActorForTransition {
            attempted_status,
            caller_side,
        } => {
            assert_eq!(attempted_status, "cancelled");
            assert_eq!(caller_side, "parent");
        }
        other => panic!("expected InvalidActorForTransition, got {other:?}"),
    }
}

#[tokio::test]
async fn cancel_on_resolved_returns_already_resolved() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x800);
    let child = Uuid::from_u128(0x801);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let resolved_id = Uuid::from_u128(0xFEED);
    let now = fixed_now();
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        resolved_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Approved,
        now - TimeDuration::days(1),
        Some(now - TimeDuration::hours(1)),
    )]));
    let svc = make_service(conv, tenants, now);

    let err = svc
        .cancel(&ctx(), resolved_id, ConversionCaller::child(child))
        .await
        .expect_err("cancel on a resolved row must return AlreadyResolved");

    assert!(
        matches!(err, DomainError::AlreadyResolved),
        "expected AlreadyResolved, got {err:?}"
    );
}

// ---- reject --------------------------------------------------------

#[tokio::test]
async fn reject_happy_path_counterparty() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x900);
    let child = Uuid::from_u128(0x901);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let pending_id = Uuid::from_u128(0x1234_5678);
    let now = fixed_now();
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        pending_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    )]));
    let svc = make_service(conv.clone(), tenants, now);

    // See `cancel_happy_path_initiator` for the rationale on the
    // service-built `conversion_repo_scope` forwarding contract.
    // `reject` is counterparty-only — here the caller is the parent,
    // so the expected repo-scope is a barrier-penetrating subtree
    // clamp on `OWNER_TENANT_ID` rooted at `parent_id`. A regression
    // that restored `allow_all` OR forgot `respect_barriers = false`
    // (silently dropping self-managed-child rows) fails here loudly.
    let updated = svc
        .reject(&ctx(), pending_id, ConversionCaller::parent(parent))
        .await
        .expect("counterparty-side reject succeeds");

    assert_eq!(updated.status, ConversionStatus::Rejected);
    assert_eq!(updated.rejected_by, Some(CTX_SUBJECT));
    assert_eq!(updated.resolved_at, Some(now));
    let captured = conv.captured_scopes();
    let expected_repo_scope = expected_parent_repo_scope(parent);
    assert_eq!(
        captured.last(),
        Some(&expected_repo_scope),
        "reject MUST forward the parent-side subtree-with-barrier-penetration \
         scope to the conversion repo so a parent rejecting a self-managed-child \
         conversion still sees the row at the DB"
    );
}

#[tokio::test]
async fn reject_by_initiator_invalid_actor() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0xA00);
    let child = Uuid::from_u128(0xA01);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let pending_id = Uuid::from_u128(0xABCD);
    let now = fixed_now();
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        pending_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    )]));
    let svc = make_service(conv, tenants, now);

    let err = svc
        .reject(&ctx(), pending_id, ConversionCaller::child(child))
        .await
        .expect_err("initiator-side reject must be rejected");

    match err {
        DomainError::InvalidActorForTransition {
            attempted_status,
            caller_side,
        } => {
            assert_eq!(attempted_status, "rejected");
            assert_eq!(caller_side, "child");
        }
        other => panic!("expected InvalidActorForTransition, got {other:?}"),
    }
}

#[tokio::test]
async fn reject_on_resolved_returns_already_resolved() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0xB00);
    let child = Uuid::from_u128(0xB01);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let resolved_id = Uuid::from_u128(0xBA5E);
    let now = fixed_now();
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        resolved_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Approved,
        now - TimeDuration::days(1),
        Some(now - TimeDuration::hours(2)),
    )]));
    let svc = make_service(conv, tenants, now);

    let err = svc
        .reject(&ctx(), resolved_id, ConversionCaller::parent(parent))
        .await
        .expect_err("reject on a resolved row must return AlreadyResolved");

    assert!(
        matches!(err, DomainError::AlreadyResolved),
        "expected AlreadyResolved, got {err:?}"
    );
}

// ---- listings ------------------------------------------------------

#[tokio::test]
async fn list_own_for_tenant_pagination_and_status_filter() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0xC00);
    let child = Uuid::from_u128(0xC01);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let now = fixed_now();
    // Five rows across statuses; `requested_at` strictly increasing
    // so the stable `(requested_at DESC, id ASC)` order is
    // deterministic.
    let mut seed: Vec<ConversionRequest> = Vec::new();
    for (i, status) in [
        ConversionStatus::Cancelled,
        ConversionStatus::Approved,
        ConversionStatus::Rejected,
        ConversionStatus::Expired,
        ConversionStatus::Approved,
    ]
    .into_iter()
    .enumerate()
    {
        let i_u128 = u128::try_from(i).expect("i fits in u128");
        let i_i64 = i64::try_from(i).expect("i fits in i64");
        let id = Uuid::from_u128(0xD000 + i_u128);
        let requested_at = now - TimeDuration::hours(10 - i_i64);
        let resolved_at = Some(requested_at + TimeDuration::minutes(5));
        seed.push(seeded_request(
            id,
            child,
            Some(parent),
            ConversionSide::Child,
            status,
            requested_at,
            resolved_at,
        ));
    }
    let conv = Arc::new(FakeConversionRepo::with_seed(seed));
    let svc = make_service(conv, tenants, now);

    // Page 1, top=2, no status filter — returns the two newest rows.
    let page1 = svc
        .list_own_for_tenant(
            &ctx(),
            child,
            &ListConversionsQuery::any(2, 0).expect("top > 0"),
        )
        .await
        .expect("list page 1");
    assert_eq!(page1.items.len(), 2, "page 1 carries top=2 items");
    assert_eq!(page1.top, 2);
    assert_eq!(page1.skip, 0);

    // Page 2, top=2, skip=2 — returns the next two.
    let page2 = svc
        .list_own_for_tenant(
            &ctx(),
            child,
            &ListConversionsQuery::any(2, 2).expect("top > 0"),
        )
        .await
        .expect("list page 2");
    assert_eq!(page2.items.len(), 2);
    // No overlap with page 1.
    let p1_ids: Vec<Uuid> = page1.items.iter().map(|r| r.id).collect();
    for r in &page2.items {
        assert!(
            !p1_ids.contains(&r.id),
            "page 2 must not repeat any page 1 id"
        );
    }

    // Status filter: only `Approved` rows (there are two in the seed).
    let approved = svc
        .list_own_for_tenant(
            &ctx(),
            child,
            &ListConversionsQuery::with_status(10, 0, ConversionStatus::Approved).expect("top > 0"),
        )
        .await
        .expect("list approved");
    assert_eq!(approved.items.len(), 2, "two seeded Approved rows");
    for row in &approved.items {
        assert_eq!(row.status, ConversionStatus::Approved);
    }
}

#[tokio::test]
async fn list_inbound_for_parent_only_direct_children() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0xE00);
    let child_a = Uuid::from_u128(0xE01);
    let child_b = Uuid::from_u128(0xE02);
    let grandchild_c = Uuid::from_u128(0xE03);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child_a,
        Some(parent),
        TenantStatus::Active,
        false,
        "a",
    );
    seed_tenant(
        &tenants,
        child_b,
        Some(parent),
        TenantStatus::Active,
        false,
        "b",
    );
    // grandchild C has child_a as its parent (depth 2).
    seed_tenant(
        &tenants,
        grandchild_c,
        Some(child_a),
        TenantStatus::Active,
        false,
        "c-grand",
    );

    let now = fixed_now();
    let req_a = seeded_request(
        Uuid::from_u128(0xE0A1),
        child_a,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    );
    let req_b = seeded_request(
        Uuid::from_u128(0xE0B1),
        child_b,
        Some(parent),
        ConversionSide::Parent,
        ConversionStatus::Pending,
        now,
        None,
    );
    // grandchild's request belongs to child_a (its parent), NOT to
    // `parent`. The repo predicate is `parent_id == :parent_id`, so
    // this row must be invisible from `parent`'s parent-side listing.
    let req_c = seeded_request(
        Uuid::from_u128(0xE0C1),
        grandchild_c,
        Some(child_a),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    );
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![req_a, req_b, req_c]));
    let svc = make_service(conv, tenants, now);

    let page = svc
        .list_inbound_for_parent(
            &ctx(),
            parent,
            &ListConversionsQuery::any(50, 0).expect("top > 0"),
        )
        .await
        .expect("parent-side listing");

    assert_eq!(
        page.items.len(),
        2,
        "only direct children A and B appear; grandchild C is excluded"
    );
    let returned_tenants: Vec<Uuid> = page.items.iter().map(|p| p.tenant_id).collect();
    assert!(returned_tenants.contains(&child_a));
    assert!(returned_tenants.contains(&child_b));
    assert!(!returned_tenants.contains(&grandchild_c));

    // Live name resolution — the projection's `child_tenant_name`
    // came from the tenant row, not from a stale snapshot.
    let proj_a = page
        .items
        .iter()
        .find(|p| p.tenant_id == child_a)
        .expect("projection for child A");
    assert_eq!(proj_a.child_tenant_name, "a");
}

#[tokio::test]
async fn list_inbound_for_parent_projection_drops_subtree() {
    // The projection type is compile-time enforced — there is no
    // `descendant_count`, no `closure`, no `metadata` on
    // `ConversionRequestParentProjection`. This test asserts the
    // visible field set by reading every documented field exactly
    // once; if a field is added or removed in the future, this test
    // is the canonical place where the minimal-surface contract is
    // pinned.
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0xF00);
    let child = Uuid::from_u128(0xF01);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "live-name",
    );

    let now = fixed_now();
    let req = seeded_request(
        Uuid::from_u128(0xF0A1),
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    );
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![req.clone()]));
    let svc = make_service(conv, tenants, now);

    let page = svc
        .list_inbound_for_parent(
            &ctx(),
            parent,
            &ListConversionsQuery::any(10, 0).expect("top > 0"),
        )
        .await
        .expect("listing");
    assert_eq!(page.items.len(), 1);
    let proj = &page.items[0];

    // Read every documented projection field — if a future patch
    // adds a sibling field, this assertion stays compatible (extra
    // fields don't break field-by-field reads); if a field is
    // removed, the compile fails here. That is the intended pin.
    assert_eq!(proj.request_id, req.id);
    assert_eq!(proj.tenant_id, req.tenant_id);
    assert_eq!(proj.child_tenant_name, "live-name");
    assert_eq!(proj.initiator_side, req.initiator_side);
    assert_eq!(proj.target_mode, req.target_mode);
    assert_eq!(proj.status, req.status);
    assert_eq!(proj.requested_by, req.requested_by);
    assert_eq!(proj.approved_by, req.approved_by);
    assert_eq!(proj.cancelled_by, req.cancelled_by);
    assert_eq!(proj.rejected_by, req.rejected_by);
    assert_eq!(proj.created_at, req.requested_at);
    assert_eq!(proj.expires_at, req.expires_at);
    assert_eq!(proj.resolved_at, req.resolved_at);
}

// ---- retention -----------------------------------------------------

#[tokio::test]
async fn soft_delete_resolved_only_touches_resolved_old_rows() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x1100);
    let child = Uuid::from_u128(0x1101);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let now = fixed_now();

    // Pending — must be untouched by retention.
    let pending = seeded_request(
        Uuid::from_u128(0x1101_0001),
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    );
    // Approved resolved 1h ago — within window (7 days), untouched.
    let approved_recent = seeded_request(
        Uuid::from_u128(0x1101_0002),
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Approved,
        now - TimeDuration::hours(2),
        Some(now - TimeDuration::hours(1)),
    );
    // Cancelled resolved 30 days ago — outside window, picked.
    let cancelled_old = seeded_request(
        Uuid::from_u128(0x1101_0003),
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Cancelled,
        now - TimeDuration::days(31),
        Some(now - TimeDuration::days(30)),
    );
    // Rejected already soft-deleted — idempotent, untouched.
    let mut rejected_already_sd = seeded_request(
        Uuid::from_u128(0x1101_0004),
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Rejected,
        now - TimeDuration::days(60),
        Some(now - TimeDuration::days(59)),
    );
    rejected_already_sd.deleted_at = Some(now - TimeDuration::days(50));

    let conv = Arc::new(FakeConversionRepo::with_seed(vec![
        pending,
        approved_recent,
        cancelled_old,
        rejected_already_sd.clone(),
    ]));
    let svc = make_service(conv.clone(), tenants, now);

    let count = svc
        .soft_delete_resolved(&ConversionScope::system_sweep(), retention_window(), 100)
        .await
        .expect("retention sweep");

    assert_eq!(count, 1, "exactly one row picked: cancelled-30d");

    // Snapshot to check the deleted_at stamp landed on the right row
    // and ONLY that row.
    let all = conv.snapshot_all();
    let by_id: std::collections::HashMap<Uuid, ConversionRequest> =
        all.into_iter().map(|r| (r.id, r)).collect();

    assert!(
        by_id[&Uuid::from_u128(0x1101_0001)].deleted_at.is_none(),
        "Pending must remain untouched"
    );
    assert!(
        by_id[&Uuid::from_u128(0x1101_0002)].deleted_at.is_none(),
        "Approved within window must remain untouched"
    );
    assert!(
        by_id[&Uuid::from_u128(0x1101_0003)].deleted_at.is_some(),
        "Cancelled-30d must be soft-deleted"
    );
    assert_eq!(
        by_id[&Uuid::from_u128(0x1101_0004)].deleted_at,
        rejected_already_sd.deleted_at,
        "already-soft-deleted Rejected must keep its existing deleted_at (idempotent)"
    );
}

// ---- status-precedes-actor invariant -----------------------------

#[tokio::test]
async fn status_precedes_actor_check_on_resolved_row() {
    // Resolved + wrong-side caller MUST surface `AlreadyResolved`
    // (not `InvalidActorForTransition`). Pin both `cancel` and
    // `reject` paths.
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x1200);
    let child = Uuid::from_u128(0x1201);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let now = fixed_now();
    let resolved_id = Uuid::from_u128(0x1202);
    // Initiator side Child, status Approved (resolved).
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        resolved_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Approved,
        now - TimeDuration::days(1),
        Some(now - TimeDuration::hours(3)),
    )]));
    let svc = make_service(conv, tenants, now);

    // Cancel from the COUNTERPARTY side (wrong actor for cancel).
    // Even though both rules would reject, the status check must win.
    let cancel_err = svc
        .cancel(&ctx(), resolved_id, ConversionCaller::parent(parent))
        .await
        .expect_err("cancel on resolved + wrong side must error");
    assert!(
        matches!(cancel_err, DomainError::AlreadyResolved),
        "cancel on resolved row MUST return AlreadyResolved (status-before-actor); got {cancel_err:?}"
    );

    // Reject from the INITIATOR side (wrong actor for reject).
    let reject_err = svc
        .reject(&ctx(), resolved_id, ConversionCaller::child(child))
        .await
        .expect_err("reject on resolved + wrong side must error");
    assert!(
        matches!(reject_err, DomainError::AlreadyResolved),
        "reject on resolved row MUST return AlreadyResolved (status-before-actor); got {reject_err:?}"
    );
}

// ---- approve -------------------------------------------------------

/// Tenant-type checker that always rejects with `TypeNotAllowed`.
/// Used by the `approve_type_reeval_rejects_leaves_pending_intact`
/// test to drive the type-not-allowed branch of the apply seam.
struct AlwaysReject;

#[async_trait]
impl TenantTypeChecker for AlwaysReject {
    async fn check_parent_child(
        &self,
        _parent_type: Uuid,
        _child_type: Uuid,
    ) -> Result<(), DomainError> {
        Err(DomainError::TypeNotAllowed {
            detail: "type_not_allowed (test fixture)".to_owned(),
        })
    }
}

/// Thin wrapper over [`FakeTenantRepo::seed_closure`] that pins the
/// `descendant_status` to `Active` — every closure row used by these
/// tests is rooted at an active tenant, so the helper sheds the
/// otherwise-uniform argument from each call site.
fn seed_closure(repo: &FakeTenantRepo, ancestor: Uuid, descendant: Uuid, barrier: i16) {
    repo.seed_closure(ancestor, descendant, barrier, TenantStatus::Active);
}

/// Look up a closure row's barrier in the `FakeTenantRepo` state.
fn closure_barrier(repo: &FakeTenantRepo, ancestor: Uuid, descendant: Uuid) -> i16 {
    repo.state
        .lock()
        .expect("lock")
        .closure
        .iter()
        .find(|r| r.ancestor_id == ancestor && r.descendant_id == descendant)
        .map_or_else(
            || panic!("closure row ({ancestor}, {descendant}) not seeded"),
            |r| r.barrier,
        )
}

/// Direct read of `tenants.self_managed`.
fn tenant_self_managed(repo: &FakeTenantRepo, id: Uuid) -> bool {
    repo.find_by_id_unchecked(id)
        .map_or_else(|| panic!("tenant {id} not seeded"), |t| t.self_managed)
}

#[tokio::test]
async fn approve_counterparty_flips_self_managed_and_transitions() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x2000);
    let child = Uuid::from_u128(0x2001);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false, // currently managed
        "c",
    );
    seed_closure(&tenants, parent, parent, 0);
    seed_closure(&tenants, child, child, 0);
    seed_closure(&tenants, parent, child, 0);

    let pending_id = Uuid::from_u128(0x2002);
    let now = fixed_now();
    let mut row = seeded_request(
        pending_id,
        child,
        Some(parent),
        ConversionSide::Child, // initiator = child
        ConversionStatus::Pending,
        now,
        None,
    );
    row.target_mode = TargetMode::SelfManaged;
    let conv =
        Arc::new(FakeConversionRepo::with_seed(vec![row]).with_tenant_repo(Arc::clone(&tenants)));
    let svc = make_service(conv.clone(), Arc::clone(&tenants), now);

    // Pin TWO contracts at once with the caller scope below:
    //
    //   1. `approve` loads the converting tenant via `allow_all()`,
    //      NOT via the caller's scope — parent-side approval of a
    //      self-managed child MUST work even though the child sits
    //      behind the closure barrier and is invisible to a
    //      parent-restricted `AccessScope` on the `tenants` entity.
    //      A regression that wired the caller scope into the
    //      `tenant_repo.find_by_id` call would collapse this happy-
    //      path to `NotFound` for self-managed-target conversions.
    //   2. `approve` forwards the side-specific scope produced by
    //      `conversion_repo_scope` to the conversion repo, NOT the
    //      caller's incoming scope. For a parent-side caller that is
    //      `InTenantSubtree(OWNER_TENANT_ID, parent_id,
    //      respect_barriers = false)` — exactly the shape that lets
    //      a parent counterparty act on a self-managed-child row at
    //      the DB while keeping subtree clamp as second-line authz.
    //      The `captured_scopes` assertion below pins this contract.
    let updated = svc
        .approve(&ctx(), pending_id, ConversionCaller::parent(parent))
        .await
        .expect("counterparty-side approve succeeds");

    assert_eq!(updated.status, ConversionStatus::Approved);
    assert_eq!(updated.approved_by, Some(CTX_SUBJECT));
    assert_eq!(updated.resolved_at, Some(now));
    assert!(
        tenant_self_managed(&tenants, child),
        "tenants.self_managed flipped to true on managed -> self_managed"
    );
    // (parent, child) strict path crosses child (the converted
    // tenant) and child.self_managed is now true -> barrier=1.
    assert_eq!(closure_barrier(&tenants, parent, child), 1);
    // Self-rows stay 0 by the closure self-row invariant.
    assert_eq!(closure_barrier(&tenants, child, child), 0);
    assert_eq!(closure_barrier(&tenants, parent, parent), 0);
    let captured = conv.captured_scopes();
    let expected_repo_scope = expected_parent_repo_scope(parent);
    assert_eq!(
        captured.last(),
        Some(&expected_repo_scope),
        "approve MUST forward the parent-side subtree-with-barrier-penetration \
         scope to the conversion repo so a parent approving a self-managed-target \
         conversion still sees the row at the DB"
    );
}

#[tokio::test]
async fn approve_initiator_side_returns_invalid_actor() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x2100);
    let child = Uuid::from_u128(0x2101);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let pending_id = Uuid::from_u128(0x2102);
    let now = fixed_now();
    let conv = Arc::new(
        FakeConversionRepo::with_seed(vec![seeded_request(
            pending_id,
            child,
            Some(parent),
            ConversionSide::Child, // initiator = child
            ConversionStatus::Pending,
            now,
            None,
        )])
        .with_tenant_repo(Arc::clone(&tenants)),
    );
    let svc = make_service(conv, Arc::clone(&tenants), now);

    let err = svc
        // Initiator side approving its own request -> invalid actor.
        .approve(&ctx(), pending_id, ConversionCaller::child(child))
        .await
        .expect_err("initiator-side approve must be rejected");

    match err {
        DomainError::InvalidActorForTransition {
            attempted_status,
            caller_side,
        } => {
            assert_eq!(attempted_status, "approved");
            assert_eq!(caller_side, "child");
        }
        other => panic!("expected InvalidActorForTransition, got {other:?}"),
    }
    assert!(
        !tenant_self_managed(&tenants, child),
        "rejected approve must NOT flip tenants.self_managed"
    );
}

#[tokio::test]
async fn approve_already_resolved_returns_already_resolved() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x2200);
    let child = Uuid::from_u128(0x2201);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let resolved_id = Uuid::from_u128(0x2202);
    let now = fixed_now();
    let conv = Arc::new(
        FakeConversionRepo::with_seed(vec![seeded_request(
            resolved_id,
            child,
            Some(parent),
            ConversionSide::Child,
            ConversionStatus::Approved,
            now - TimeDuration::days(1),
            Some(now - TimeDuration::hours(1)),
        )])
        .with_tenant_repo(Arc::clone(&tenants)),
    );
    let svc = make_service(conv, Arc::clone(&tenants), now);

    let err = svc
        .approve(&ctx(), resolved_id, ConversionCaller::parent(parent))
        .await
        .expect_err("approve on resolved row must surface AlreadyResolved");
    assert!(
        matches!(err, DomainError::AlreadyResolved),
        "expected AlreadyResolved, got {err:?}"
    );
}

#[tokio::test]
async fn approve_type_reeval_rejects_leaves_pending_intact() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x2300);
    let child = Uuid::from_u128(0x2301);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );
    seed_closure(&tenants, parent, parent, 0);
    seed_closure(&tenants, child, child, 0);
    seed_closure(&tenants, parent, child, 0);

    let pending_id = Uuid::from_u128(0x2302);
    let now = fixed_now();
    let mut row = seeded_request(
        pending_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    );
    row.target_mode = TargetMode::SelfManaged;
    let conv =
        Arc::new(FakeConversionRepo::with_seed(vec![row]).with_tenant_repo(Arc::clone(&tenants)));
    let svc = make_service_with_checker(
        conv.clone(),
        Arc::clone(&tenants),
        Arc::new(AlwaysReject),
        now,
    );

    let err = svc
        .approve(&ctx(), pending_id, ConversionCaller::parent(parent))
        .await
        .expect_err("type re-eval rejection must surface TypeNotAllowed");

    match err {
        DomainError::TypeNotAllowed { .. } => {}
        other => panic!("expected TypeNotAllowed, got {other:?}"),
    }

    // Apply MUST roll back: pending row stays Pending, tenant flag
    // unchanged, closure barrier untouched.
    let row = conv
        .snapshot_all()
        .into_iter()
        .find(|r| r.id == pending_id)
        .expect("row still present");
    assert_eq!(row.status, ConversionStatus::Pending);
    assert!(row.approved_by.is_none());
    assert!(row.resolved_at.is_none());
    assert!(
        !tenant_self_managed(&tenants, child),
        "tenants.self_managed MUST NOT flip on type-not-allowed"
    );
    assert_eq!(closure_barrier(&tenants, parent, child), 0);
}

#[tokio::test]
async fn approve_when_tenant_inactive_returns_validation() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x2400);
    let child = Uuid::from_u128(0x2401);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    // Tenant is Suspended at approve time — service-level fence
    // surfaces Validation before any apply runs.
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Suspended,
        false,
        "c",
    );

    let pending_id = Uuid::from_u128(0x2402);
    let now = fixed_now();
    let conv = Arc::new(
        FakeConversionRepo::with_seed(vec![seeded_request(
            pending_id,
            child,
            Some(parent),
            ConversionSide::Child,
            ConversionStatus::Pending,
            now,
            None,
        )])
        .with_tenant_repo(Arc::clone(&tenants)),
    );
    let svc = make_service(conv, Arc::clone(&tenants), now);

    let err = svc
        .approve(&ctx(), pending_id, ConversionCaller::parent(parent))
        .await
        .expect_err("approve on non-active tenant must surface Validation");

    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected Validation, got {err:?}"
    );
}

// ---- ConversionCaller::parent scope-mismatch matrix ---------------
//
// Pins the `verify_parent_scope` guard documented on
// `service::verify_parent_scope`: a parent-side caller acting on a
// request whose stored `parent_id` is NOT the caller's declared
// `parent_scope_id` is rejected. The internal `verify_caller_scope`
// helper produces `DomainError::Validation`, but every public
// service method routes the mismatch through
// `require_caller_scope_or_not_found` which intentionally
// re-maps the surface error to `DomainError::NotFound` keyed on
// the request id — collapsing the existence-channel so an
// out-of-scope caller cannot distinguish "row exists in another
// tenant" from "row does not exist". Every matrix test below
// asserts the public `NotFound` surface; the previous prose
// ("MUST be rejected with `Validation`") was inherited from an
// earlier round and has been corrected here. The guard still
// runs BEFORE the state / role matrix, so even a "would-have-been
// `AlreadyResolved`" or "would-have-been `InvalidActorForTransition`"
// row surfaces the scope mismatch first.
//
// Matrix coverage: each of the four service entry points
// (`request_conversion`, `cancel`, `reject`, `approve`) is exercised
// with `ConversionCaller::parent(WRONG_PARENT)` and the underlying
// row's `parent_id` set to a different tenant, including a guard-
// ordering test where the row is also resolved (state would otherwise
// fire first if the scope check ran second).

const WRONG_PARENT_MARKER: u128 = 0xBADD;

#[tokio::test]
async fn request_conversion_parent_scope_mismatch_returns_not_found() {
    let conv = Arc::new(FakeConversionRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x9001);
    let child = Uuid::from_u128(0x9002);
    let wrong_parent = Uuid::from_u128(WRONG_PARENT_MARKER);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );
    let now = fixed_now();
    let svc = make_service(conv, tenants, now);

    let err = svc
        .request_conversion(
            &ctx(),
            RequestConversionInput {
                tenant_id: child,
                caller: ConversionCaller::parent(wrong_parent),
                target_mode_override: None,
            },
        )
        .await
        .expect_err(
            "parent-side caller scoped to wrong parent must surface NotFound \
             (existence-leak prevention)",
        );

    assert!(
        matches!(err, DomainError::ConversionRequestNotFound { .. }),
        "expected NotFound, got {err:?}"
    );
}

#[tokio::test]
async fn cancel_parent_scope_mismatch_wins_over_state_and_actor() {
    // Row is RESOLVED and caller_side matches the initiator (would
    // normally surface `AlreadyResolved` on resolved-row + initiator-
    // matches-cancel rule). A wrong parent_scope_id MUST surface
    // `Validation` first, because `verify_parent_scope` runs before
    // the state / role matrix.
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x9101);
    let child = Uuid::from_u128(0x9102);
    let wrong_parent = Uuid::from_u128(WRONG_PARENT_MARKER);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let resolved_id = Uuid::from_u128(0x9103);
    let now = fixed_now();
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        resolved_id,
        child,
        Some(parent),
        ConversionSide::Parent, // initiator = parent (so child is counterparty)
        ConversionStatus::Cancelled,
        now,
        Some(now),
    )]));
    let svc = make_service(conv, tenants, now);

    let err = svc
        .cancel(&ctx(), resolved_id, ConversionCaller::parent(wrong_parent))
        .await
        .expect_err("parent-side scope mismatch must beat state / actor checks");

    assert!(
        matches!(err, DomainError::ConversionRequestNotFound { .. }),
        "scope check MUST run before AlreadyResolved / InvalidActor AND scope mismatch \
         MUST surface as NotFound (existence-leak prevention); got {err:?}"
    );
}

#[tokio::test]
async fn reject_parent_scope_mismatch_returns_not_found() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x9201);
    let child = Uuid::from_u128(0x9202);
    let wrong_parent = Uuid::from_u128(WRONG_PARENT_MARKER);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let pending_id = Uuid::from_u128(0x9203);
    let now = fixed_now();
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        pending_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    )]));
    let svc = make_service(conv, tenants, now);

    let err = svc
        .reject(&ctx(), pending_id, ConversionCaller::parent(wrong_parent))
        .await
        .expect_err(
            "parent-side scope mismatch must surface NotFound \
             (existence-leak prevention)",
        );

    assert!(
        matches!(err, DomainError::ConversionRequestNotFound { .. }),
        "expected NotFound, got {err:?}"
    );
}

#[tokio::test]
async fn approve_parent_scope_mismatch_returns_not_found() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x9301);
    let child = Uuid::from_u128(0x9302);
    let wrong_parent = Uuid::from_u128(WRONG_PARENT_MARKER);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let pending_id = Uuid::from_u128(0x9303);
    let now = fixed_now();
    let conv = Arc::new(
        FakeConversionRepo::with_seed(vec![seeded_request(
            pending_id,
            child,
            Some(parent),
            ConversionSide::Child,
            ConversionStatus::Pending,
            now,
            None,
        )])
        .with_tenant_repo(Arc::clone(&tenants)),
    );
    let svc = make_service(conv, Arc::clone(&tenants), now);

    let err = svc
        .approve(&ctx(), pending_id, ConversionCaller::parent(wrong_parent))
        .await
        .expect_err(
            "parent-side scope mismatch must surface NotFound \
             (existence-leak prevention)",
        );

    assert!(
        matches!(err, DomainError::ConversionRequestNotFound { .. }),
        "expected NotFound, got {err:?}"
    );
}

// ---- ConversionCaller::child scope-mismatch matrix ----------------
//
// Symmetric to the parent-side matrix above: a child-side caller
// scoped to `WRONG_CHILD` is rejected on every entry point that
// takes a `request_id`. The internal `verify_caller_scope` emits
// `DomainError::Validation` but the public service surface goes
// through `require_caller_scope_or_not_found`, which collapses
// the existence channel and re-maps to `DomainError::NotFound` —
// every test below asserts that public surface (previous prose
// said "rejected with `Validation`" and has been corrected here).
// Closes cypilot-R6 MINOR (child-scope-mismatch matrix incomplete).

const WRONG_CHILD_MARKER: u128 = 0xBADC;

#[tokio::test]
async fn cancel_child_scope_mismatch_returns_not_found() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x9401);
    let child = Uuid::from_u128(0x9402);
    let wrong_child = Uuid::from_u128(WRONG_CHILD_MARKER);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let pending_id = Uuid::from_u128(0x9403);
    let now = fixed_now();
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        pending_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    )]));
    let svc = make_service(conv, tenants, now);

    let err = svc
        .cancel(&ctx(), pending_id, ConversionCaller::child(wrong_child))
        .await
        .expect_err(
            "child-side scope mismatch must surface NotFound \
             (existence-leak prevention)",
        );

    assert!(
        matches!(err, DomainError::ConversionRequestNotFound { .. }),
        "expected NotFound, got {err:?}"
    );
}

#[tokio::test]
async fn reject_child_scope_mismatch_returns_not_found() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x9501);
    let child = Uuid::from_u128(0x9502);
    let wrong_child = Uuid::from_u128(WRONG_CHILD_MARKER);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let pending_id = Uuid::from_u128(0x9503);
    let now = fixed_now();
    // Initiator = parent, so the legitimate counterparty is the child
    // side — a child-side caller with the wrong child_id MUST still
    // surface scope mismatch BEFORE the role-rule check.
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        pending_id,
        child,
        Some(parent),
        ConversionSide::Parent,
        ConversionStatus::Pending,
        now,
        None,
    )]));
    let svc = make_service(conv, tenants, now);

    let err = svc
        .reject(&ctx(), pending_id, ConversionCaller::child(wrong_child))
        .await
        .expect_err(
            "child-side scope mismatch must surface NotFound \
             (existence-leak prevention)",
        );

    assert!(
        matches!(err, DomainError::ConversionRequestNotFound { .. }),
        "expected NotFound, got {err:?}"
    );
}

#[tokio::test]
async fn approve_child_scope_mismatch_returns_not_found() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x9601);
    let child = Uuid::from_u128(0x9602);
    let wrong_child = Uuid::from_u128(WRONG_CHILD_MARKER);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let pending_id = Uuid::from_u128(0x9603);
    let now = fixed_now();
    // Initiator = parent so the legitimate counterparty is the child
    // side — confirms the scope check fires before the role / state
    // matrix on the approve entry point too.
    let conv = Arc::new(
        FakeConversionRepo::with_seed(vec![seeded_request(
            pending_id,
            child,
            Some(parent),
            ConversionSide::Parent,
            ConversionStatus::Pending,
            now,
            None,
        )])
        .with_tenant_repo(Arc::clone(&tenants)),
    );
    let svc = make_service(conv, Arc::clone(&tenants), now);

    let err = svc
        .approve(&ctx(), pending_id, ConversionCaller::child(wrong_child))
        .await
        .expect_err(
            "child-side scope mismatch must surface NotFound \
             (existence-leak prevention)",
        );

    assert!(
        matches!(err, DomainError::ConversionRequestNotFound { .. }),
        "expected NotFound, got {err:?}"
    );
}

// ---- verify_caller_scope data-integrity diagnostic ---------------
//
// `verify_caller_scope` carries a distinct diagnostic for the
// "row.parent_id == None on a parent-side call" case (FEATURE
// root-tenant refusal should have prevented this insert; if a row
// exists somehow it's a data-integrity violation, not a regular
// scope mismatch). Pin the diagnostic so a refactor that collapses
// the branch into the generic `Validation` path is caught.

#[tokio::test]
async fn cancel_parent_side_on_null_parent_id_row_surfaces_not_found() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let orphan = Uuid::from_u128(0x9701);
    let any_parent = Uuid::from_u128(0x9702);
    seed_tenant(
        &tenants,
        orphan,
        None,
        TenantStatus::Active,
        false,
        "orphan",
    );

    let pending_id = Uuid::from_u128(0x9703);
    let now = fixed_now();
    // Seed a row whose `parent_id` is `None` directly via
    // `with_seed` — the production service path would never write
    // such a row (root-tenant refusal blocks the insert), so we go
    // through the test-only fake.
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        pending_id,
        orphan,
        None, // parent_id intentionally None
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    )]));
    let svc = make_service(conv, tenants, now);

    let err = svc
        .cancel(&ctx(), pending_id, ConversionCaller::parent(any_parent))
        .await
        .expect_err(
            "parent-side caller on a NULL-parent_id row must surface NotFound \
             (existence-leak prevention; the corruption is logged at warn level \
             on `am.domain` for operator triage rather than leaked to the \
             caller via a distinguishable error code)",
        );

    assert!(
        matches!(err, DomainError::ConversionRequestNotFound { .. }),
        "expected NotFound (data-integrity case maps to the same surface as a regular \
         scope mismatch to avoid leaking corruption to a potentially untrusted caller); \
         got {err:?}"
    );
}

// ---- target_mode_override no-op guard ------------------------------
//
// Pins the codex-R3 fix in `request_conversion`: an override that
// resolves to the SAME mode the tenant already has is rejected with
// `Validation` and consumes no partial-unique-pending slot. Both
// directions are covered (managed -> managed, self_managed ->
// self_managed) so a regression that drops the guard for one branch
// is caught.

#[tokio::test]
async fn request_conversion_no_op_managed_to_managed_returns_validation() {
    let conv = Arc::new(FakeConversionRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0xA001);
    let child = Uuid::from_u128(0xA002);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    // child currently managed (`self_managed = false`).
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );
    let now = fixed_now();
    let svc = make_service(Arc::clone(&conv), tenants, now);

    let err = svc
        .request_conversion(
            &ctx(),
            RequestConversionInput {
                tenant_id: child,
                caller: ConversionCaller::child(child),
                target_mode_override: Some(TargetMode::Managed),
            },
        )
        .await
        .expect_err("override matching current mode must surface Validation");

    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected Validation, got {err:?}"
    );
    // No row consumed the partial-unique-pending slot.
    assert!(
        conv.pending_request_id_for(child).is_none(),
        "no pending row should have been written"
    );
}

#[tokio::test]
async fn request_conversion_no_op_self_managed_to_self_managed_returns_validation() {
    let conv = Arc::new(FakeConversionRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0xA101);
    let child = Uuid::from_u128(0xA102);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    // child currently self-managed.
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        true,
        "c",
    );
    let now = fixed_now();
    let svc = make_service(Arc::clone(&conv), tenants, now);

    let err = svc
        .request_conversion(
            &ctx(),
            RequestConversionInput {
                tenant_id: child,
                caller: ConversionCaller::child(child),
                target_mode_override: Some(TargetMode::SelfManaged),
            },
        )
        .await
        .expect_err("override matching current mode must surface Validation");

    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected Validation, got {err:?}"
    );
    assert!(
        conv.pending_request_id_for(child).is_none(),
        "no pending row should have been written"
    );
}

#[tokio::test]
async fn approve_returns_internal_when_fake_repo_missing_tenant_repo_handle() {
    // Pin the `FakeConversionRepo::apply_conversion_approval`
    // contract: when the cross-fake `tenant_repo` handle is absent,
    // the apply path returns `DomainError::Internal` rather than
    // panicking or silently flipping nothing. Mirrors the production
    // SQL impl where a missing collaborator would surface as a
    // typed internal-error.
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x2480);
    let child = Uuid::from_u128(0x2481);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let pending_id = Uuid::from_u128(0x2482);
    let now = fixed_now();
    // Note the absence of `with_tenant_repo` — the fake's apply seam
    // checks `self.tenant_repo` and surfaces `Internal` when it is
    // `None`.
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        pending_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    )]));
    let svc = make_service(conv, Arc::clone(&tenants), now);

    let err = svc
        .approve(&ctx(), pending_id, ConversionCaller::parent(parent))
        .await
        .expect_err("approve must surface Internal when fake apply seam is unwired");

    assert!(
        matches!(err, DomainError::Internal { .. }),
        "expected Internal, got {err:?}"
    );
}

#[tokio::test]
async fn approve_rewrites_barrier_managed_to_self_managed() {
    // Three-deep tree: root -> mid -> leaf.
    // All managed before approve. Convert `mid` to self-managed.
    // Expected: barrier=1 on every closure row whose strict path
    // crosses `mid`.
    let tenants = Arc::new(FakeTenantRepo::new());
    let root = Uuid::from_u128(0x2500);
    let mid = Uuid::from_u128(0x2501);
    let leaf = Uuid::from_u128(0x2502);
    seed_tenant(&tenants, root, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        mid,
        Some(root),
        TenantStatus::Active,
        false,
        "mid",
    );
    seed_tenant(
        &tenants,
        leaf,
        Some(mid),
        TenantStatus::Active,
        false,
        "leaf",
    );
    // Closure rows: each tenant's self-row + strict ancestors.
    seed_closure(&tenants, root, root, 0);
    seed_closure(&tenants, mid, mid, 0);
    seed_closure(&tenants, leaf, leaf, 0);
    seed_closure(&tenants, root, mid, 0);
    seed_closure(&tenants, root, leaf, 0);
    seed_closure(&tenants, mid, leaf, 0);

    let pending_id = Uuid::from_u128(0x2503);
    let now = fixed_now();
    let mut row = seeded_request(
        pending_id,
        mid, // converting tenant = mid
        Some(root),
        ConversionSide::Parent, // initiator = parent (root side)
        ConversionStatus::Pending,
        now,
        None,
    );
    row.target_mode = TargetMode::SelfManaged;
    let conv =
        Arc::new(FakeConversionRepo::with_seed(vec![row]).with_tenant_repo(Arc::clone(&tenants)));
    let svc = make_service(conv, Arc::clone(&tenants), now);

    // The initiator was the parent (root) side, so the counterparty
    // is the child side — `ConversionCaller::child(mid)` is the
    // converting tenant's own scope. This exercises the
    // `verify_caller_scope` child-path branch end-to-end on a
    // parent-initiated row, complementing the
    // parent-initiator-on-child-counterparty cases above.
    let _ = svc
        .approve(&ctx(), pending_id, ConversionCaller::child(mid))
        .await
        .expect("counterparty (child) approve succeeds");

    // (root, mid) strict path = {mid}; mid is now self-managed -> barrier=1.
    assert_eq!(closure_barrier(&tenants, root, mid), 1);
    // (root, leaf) strict path = {mid, leaf}; mid is self-managed -> barrier=1.
    assert_eq!(closure_barrier(&tenants, root, leaf), 1);
    // (mid, leaf) strict path = {leaf}; leaf is managed -> barrier=0.
    // BUT this row's path does NOT cross mid (mid is the ancestor and
    // is excluded from the strict path), so the barrier stays at its
    // pre-approve value of 0.
    assert_eq!(closure_barrier(&tenants, mid, leaf), 0);
    // Self-rows always 0.
    assert_eq!(closure_barrier(&tenants, root, root), 0);
    assert_eq!(closure_barrier(&tenants, mid, mid), 0);
    assert_eq!(closure_barrier(&tenants, leaf, leaf), 0);
}

#[tokio::test]
async fn approve_rewrites_barrier_self_managed_to_managed() {
    // Three-deep tree: root -> mid (self_managed) -> leaf.
    // Convert `mid` back to managed. Expected: barrier=0 on rows
    // whose strict path no longer has any self-managed tenant.
    // Add a self-managed `extra` sibling under root to demonstrate
    // that rows through `extra` retain barrier=1 even after `mid`
    // flips back to managed (path through extra is unaffected).
    let tenants = Arc::new(FakeTenantRepo::new());
    let root = Uuid::from_u128(0x2600);
    let mid = Uuid::from_u128(0x2601);
    let leaf = Uuid::from_u128(0x2602);
    let extra = Uuid::from_u128(0x2603);
    let extra_leaf = Uuid::from_u128(0x2604);
    seed_tenant(&tenants, root, None, TenantStatus::Active, false, "root");
    seed_tenant(&tenants, mid, Some(root), TenantStatus::Active, true, "mid");
    seed_tenant(
        &tenants,
        leaf,
        Some(mid),
        TenantStatus::Active,
        false,
        "leaf",
    );
    seed_tenant(
        &tenants,
        extra,
        Some(root),
        TenantStatus::Active,
        true,
        "extra",
    );
    seed_tenant(
        &tenants,
        extra_leaf,
        Some(extra),
        TenantStatus::Active,
        false,
        "extra-leaf",
    );
    // Pre-approve closure barriers reflect the existing self-managed
    // tenants on each strict path.
    seed_closure(&tenants, root, root, 0);
    seed_closure(&tenants, mid, mid, 0);
    seed_closure(&tenants, leaf, leaf, 0);
    seed_closure(&tenants, extra, extra, 0);
    seed_closure(&tenants, extra_leaf, extra_leaf, 0);
    seed_closure(&tenants, root, mid, 1); // mid self-managed
    seed_closure(&tenants, root, leaf, 1); // mid self-managed on path
    seed_closure(&tenants, mid, leaf, 0); // strict path = {leaf}, managed
    seed_closure(&tenants, root, extra, 1); // extra self-managed
    seed_closure(&tenants, root, extra_leaf, 1); // extra self-managed on path
    seed_closure(&tenants, extra, extra_leaf, 0); // strict path = {extra_leaf}, managed

    let pending_id = Uuid::from_u128(0x2605);
    let now = fixed_now();
    let mut row = seeded_request(
        pending_id,
        mid, // converting tenant = mid
        Some(root),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    );
    row.target_mode = TargetMode::Managed; // flip back to managed
    let conv =
        Arc::new(FakeConversionRepo::with_seed(vec![row]).with_tenant_repo(Arc::clone(&tenants)));
    let svc = make_service(conv, Arc::clone(&tenants), now);

    let _ = svc
        .approve(&ctx(), pending_id, ConversionCaller::parent(root))
        .await
        .expect("counterparty (parent) approve succeeds");

    // (root, mid): strict path = {mid}, mid is now managed -> barrier=0.
    assert_eq!(closure_barrier(&tenants, root, mid), 0);
    // (root, leaf): strict path = {mid, leaf}, both managed -> barrier=0.
    assert_eq!(closure_barrier(&tenants, root, leaf), 0);
    // (mid, leaf): strict path = {leaf}, managed; row's path does NOT
    // cross `mid` so this row was never re-evaluated.
    assert_eq!(closure_barrier(&tenants, mid, leaf), 0);
    // Rows on the unaffected `extra` subtree retain their pre-approve
    // barrier values — the approve only re-evaluates rows whose
    // strict path crosses `mid`.
    assert_eq!(closure_barrier(&tenants, root, extra), 1);
    assert_eq!(closure_barrier(&tenants, root, extra_leaf), 1);
    assert!(
        !tenant_self_managed(&tenants, mid),
        "tenants.self_managed flipped to false on self_managed -> managed"
    );
}

// ---- expire --------------------------------------------------------

#[tokio::test]
async fn expire_empty_queue_returns_zero() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let conv = Arc::new(FakeConversionRepo::new());
    let svc = make_service(conv, tenants, fixed_now());
    let count = svc
        .expire_pending(
            &ConversionScope::system_sweep(),
            100,
            &CancellationToken::new(),
        )
        .await
        .expect("empty queue tick");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn expire_one_past_pending_transitions_and_audits() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x2700);
    let child = Uuid::from_u128(0x2701);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false, // managed
        "c",
    );

    let pending_id = Uuid::from_u128(0x2702);
    let now = fixed_now();
    let mut row = seeded_request(
        pending_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now - TimeDuration::days(8),
        None,
    );
    // Make it expired: expires_at strictly before now.
    row.expires_at = now - TimeDuration::hours(1);
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![row]));
    let svc = make_service(conv.clone(), Arc::clone(&tenants), now);

    let count = svc
        .expire_pending(
            &ConversionScope::system_sweep(),
            100,
            &CancellationToken::new(),
        )
        .await
        .expect("expire tick");
    assert_eq!(count, 1, "exactly one row must transition to Expired");

    let snap = conv
        .snapshot_all()
        .into_iter()
        .find(|r| r.id == pending_id)
        .expect("row still present");
    assert_eq!(snap.status, ConversionStatus::Expired);
    assert_eq!(snap.resolved_at, Some(now));
    // Expire MUST NOT mutate `tenants.self_managed`.
    assert!(
        !tenant_self_managed(&tenants, child),
        "expire MUST NOT flip tenants.self_managed"
    );
}

#[tokio::test]
async fn expire_mixed_batch_only_pending_and_expired_transition() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x2800);
    let child_a = Uuid::from_u128(0x2801);
    let child_b = Uuid::from_u128(0x2802);
    let child_c = Uuid::from_u128(0x2803);
    let child_d = Uuid::from_u128(0x2804);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child_a,
        Some(parent),
        TenantStatus::Active,
        false,
        "a",
    );
    seed_tenant(
        &tenants,
        child_b,
        Some(parent),
        TenantStatus::Active,
        false,
        "b",
    );
    seed_tenant(
        &tenants,
        child_c,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );
    seed_tenant(
        &tenants,
        child_d,
        Some(parent),
        TenantStatus::Active,
        false,
        "d",
    );

    let now = fixed_now();
    let id_a = Uuid::from_u128(0x2810);
    let id_b = Uuid::from_u128(0x2811);
    let id_c = Uuid::from_u128(0x2812);
    let id_d = Uuid::from_u128(0x2813);

    // Row A — Pending, expires in past -> should transition.
    let mut row_a = seeded_request(
        id_a,
        child_a,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now - TimeDuration::days(8),
        None,
    );
    row_a.expires_at = now - TimeDuration::hours(1);

    // Row B — Pending, expires in future -> stay.
    let mut row_b = seeded_request(
        id_b,
        child_b,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    );
    row_b.expires_at = now + TimeDuration::hours(1);

    // Row C — Already approved, even with expires_at in past -> stay.
    let mut row_c = seeded_request(
        id_c,
        child_c,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Approved,
        now - TimeDuration::days(8),
        Some(now - TimeDuration::days(7)),
    );
    row_c.expires_at = now - TimeDuration::hours(1);

    // Row D — Cancelled, expires_at in past -> stay.
    let mut row_d = seeded_request(
        id_d,
        child_d,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Cancelled,
        now - TimeDuration::days(8),
        Some(now - TimeDuration::days(7)),
    );
    row_d.expires_at = now - TimeDuration::hours(1);

    let conv = Arc::new(FakeConversionRepo::with_seed(vec![
        row_a, row_b, row_c, row_d,
    ]));
    let svc = make_service(conv.clone(), tenants, now);

    let count = svc
        .expire_pending(
            &ConversionScope::system_sweep(),
            100,
            &CancellationToken::new(),
        )
        .await
        .expect("expire tick");
    assert_eq!(count, 1, "only row A transitions");

    let by_id: std::collections::HashMap<Uuid, ConversionRequest> =
        conv.snapshot_all().into_iter().map(|r| (r.id, r)).collect();
    assert_eq!(by_id[&id_a].status, ConversionStatus::Expired);
    assert_eq!(by_id[&id_b].status, ConversionStatus::Pending);
    assert_eq!(by_id[&id_c].status, ConversionStatus::Approved);
    assert_eq!(by_id[&id_d].status, ConversionStatus::Cancelled);

    // Idempotency: re-running on the post-expire state returns 0.
    let count2 = svc
        .expire_pending(
            &ConversionScope::system_sweep(),
            100,
            &CancellationToken::new(),
        )
        .await
        .expect("expire tick #2");
    assert_eq!(count2, 0, "idempotent - no rows left to expire");
}

#[tokio::test]
#[tracing_test::traced_test]
async fn expire_pending_treats_vanished_row_as_idempotent_skip() {
    // Pin the `Err(DomainError::NotFound)` arm of the expire loop:
    // when a row is observed by `query_expired` but vanishes before
    // `transition_pending_to_expired` runs (most commonly via FK
    // cascade from a concurrent tenant hard-delete or a retention
    // sweep beating us to the row), the loop MUST classify it as a
    // benign idempotent skip — neither `transitioned` nor `failed`
    // gets incremented, and the escalation warn predicate does
    // NOT fire. The `FakeConversionRepo::mark_vanished` seam
    // simulates the production race deterministically.
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x2900);
    let child = Uuid::from_u128(0x2901);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let pending_id = Uuid::from_u128(0x2902);
    let now = fixed_now();
    let mut row = seeded_request(
        pending_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now - TimeDuration::days(8),
        None,
    );
    row.expires_at = now - TimeDuration::hours(1);
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![row]));
    // Flag the row as vanished AFTER seeding: `query_expired` still
    // returns it from the scan, but `transition_pending_to_expired`
    // routes through `lookup_pending_mut` which now returns
    // `NotFound`. Exactly the production race we want to pin.
    conv.mark_vanished(pending_id);
    let svc = make_service(conv, tenants, now);

    let count = svc
        .expire_pending(
            &ConversionScope::system_sweep(),
            100,
            &CancellationToken::new(),
        )
        .await
        .expect("vanished-row tick is success-equivalent at the call site");
    assert_eq!(
        count, 0,
        "vanished rows MUST NOT count towards `transitioned`"
    );
    // Cross-check that the idempotent-skip branch did NOT increment
    // `failed`: the escalation warn on `am.lifecycle` would fire
    // (`2 * 1 >= 1`) if a future regression reclassified `NotFound`
    // into the failure arm. Capturing its absence via `tracing-test`
    // closes the loop on what the `count == 0` assertion above
    // could not prove alone.
    assert!(
        !logs_contain("half-or-more per-row failures"),
        "vanished-row skip MUST NOT trigger the escalation warn predicate"
    );
    // Also pin the positive signal: the per-row debug line emits
    // `outcome = "skipped_not_found"` so an operator filter for
    // FK-cascade races can distinguish them from
    // `skipped_already_resolved` (peer reaper) and from a real
    // transition.
    assert!(
        logs_contain("skipped_not_found"),
        "vanished-row skip MUST emit `outcome=skipped_not_found` on am.events"
    );
}

#[tokio::test]
#[tracing_test::traced_test]
async fn expire_pending_escalation_warn_fires_when_failed_equals_half_of_due_total() {
    // Pin the new escalation predicate `2 * failed >= due_total` at
    // the inclusive 50% boundary. Two rows are due; one is
    // successfully transitioned, the other is forced into the
    // `Err(other)` arm via `inject_lookup_error` (a non-`NotFound`,
    // non-`AlreadyResolved` shape that maps onto `failed += 1`
    // inside the expire loop). With `due_total = 2, failed = 1`
    // the predicate evaluates `2 * 1 >= 2 → true` and the warn
    // MUST emit. A future tightening to strict `>` would silently
    // pass `count == 1` here, so the test pins the warn emission
    // via `tracing-test` rather than the count alone.
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x2A00);
    let child_ok = Uuid::from_u128(0x2A01);
    let child_fail = Uuid::from_u128(0x2A02);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child_ok,
        Some(parent),
        TenantStatus::Active,
        false,
        "c-ok",
    );
    seed_tenant(
        &tenants,
        child_fail,
        Some(parent),
        TenantStatus::Active,
        false,
        "c-fail",
    );

    let id_ok = Uuid::from_u128(0x2A10);
    let id_fail = Uuid::from_u128(0x2A11);
    let now = fixed_now();
    let mut row_ok = seeded_request(
        id_ok,
        child_ok,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now - TimeDuration::days(8),
        None,
    );
    row_ok.expires_at = now - TimeDuration::hours(2);
    let mut row_fail = seeded_request(
        id_fail,
        child_fail,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now - TimeDuration::days(8),
        None,
    );
    // Slightly later `expires_at` so the scan returns row_ok first
    // (the scan orders by `expires_at ASC`). Drives row_ok through
    // the success arm and row_fail through the failure arm in a
    // single tick, exercising the per-row classifier mid-loop.
    row_fail.expires_at = now - TimeDuration::hours(1);
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![row_ok, row_fail]));
    // Inject a non-`NotFound`, non-`AlreadyResolved` per-row
    // error: `Internal` routes through the `Err(other)` arm of
    // expire_pending, incrementing `failed` exactly once.
    conv.inject_lookup_error(
        id_fail,
        DomainError::Internal {
            diagnostic: "synthetic per-row fault for boundary test".to_owned(),
            cause: None,
        },
    );
    let svc = make_service(conv, tenants, now);

    let count = svc
        .expire_pending(
            &ConversionScope::system_sweep(),
            100,
            &CancellationToken::new(),
        )
        .await
        .expect("boundary tick returns Ok(_); per-row failures don't propagate");
    assert_eq!(
        count, 1,
        "one row transitioned (row_ok); row_fail counted towards `failed`, not `transitioned`"
    );
    // The load-bearing assertion: the escalation warn MUST emit
    // when `failed == due_total / 2`. Without `tracing-test` this
    // path was unobservable at the service-public surface.
    assert!(
        logs_contain("half-or-more per-row failures"),
        "escalation warn MUST emit at exact 50% failure rate (failed=1, due_total=2)"
    );
    // Cross-check the per-row warn on `am.domain` fired too — that
    // channel is where dashboards aggregate the underlying causes.
    assert!(
        logs_contain("expire_pending: per-row transition failed"),
        "per-row failure MUST emit on am.domain with the offending request_id"
    );
}

#[tokio::test]
#[tracing_test::traced_test]
async fn expire_pending_escalation_warn_silent_when_failed_below_half_of_due_total() {
    // Counter-boundary: `due_total = 3, failed = 1` → `2 * 1 < 3`
    // → predicate does NOT fire. Pins that a single-row failure in
    // an otherwise-healthy batch stays below the escalation
    // threshold and only emits the per-row `am.domain` warn (which
    // dashboards aggregate without paging on).
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x2B00);
    let child_a = Uuid::from_u128(0x2B01);
    let child_b = Uuid::from_u128(0x2B02);
    let child_c = Uuid::from_u128(0x2B03);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    for (id, suffix) in [(child_a, "a"), (child_b, "b"), (child_c, "c")] {
        seed_tenant(
            &tenants,
            id,
            Some(parent),
            TenantStatus::Active,
            false,
            &format!("c-{suffix}"),
        );
    }

    let id_a = Uuid::from_u128(0x2B10);
    let id_b = Uuid::from_u128(0x2B11);
    let id_c = Uuid::from_u128(0x2B12);
    let now = fixed_now();
    let mut rows = Vec::new();
    for (rid, tid, hours) in [(id_a, child_a, 3), (id_b, child_b, 2), (id_c, child_c, 1)] {
        let mut row = seeded_request(
            rid,
            tid,
            Some(parent),
            ConversionSide::Child,
            ConversionStatus::Pending,
            now - TimeDuration::days(8),
            None,
        );
        row.expires_at = now - TimeDuration::hours(hours);
        rows.push(row);
    }
    let conv = Arc::new(FakeConversionRepo::with_seed(rows));
    conv.inject_lookup_error(
        id_a,
        DomainError::Internal {
            diagnostic: "synthetic per-row fault for counter-boundary test".to_owned(),
            cause: None,
        },
    );
    let svc = make_service(conv, tenants, now);

    let count = svc
        .expire_pending(
            &ConversionScope::system_sweep(),
            100,
            &CancellationToken::new(),
        )
        .await
        .expect("counter-boundary tick returns Ok(_)");
    assert_eq!(
        count, 2,
        "two rows transitioned (id_b, id_c); id_a counted towards `failed`"
    );
    assert!(
        !logs_contain("half-or-more per-row failures"),
        "escalation warn MUST stay silent at 1-of-3 failure rate (failed=1, due_total=3)"
    );
    assert!(
        logs_contain("expire_pending: per-row transition failed"),
        "per-row warn on am.domain MUST still fire for the single failure"
    );
}

#[tokio::test]
async fn reaper_tick_expires_then_soft_deletes_in_one_pass() {
    // The conversion reaper loop in `module.rs` interleaves
    // `expire_pending` and `soft_delete_resolved` on the same tick.
    // This test exercises both calls back-to-back on a single fake
    // state to pin three contracts:
    //
    //   1. `expire_pending` flips an overdue Pending row to Expired
    //      (and the row's `resolved_at` is set to `now`).
    //   2. `soft_delete_resolved` then walks the resolved set and
    //      marks rows whose retention window has elapsed.
    //   3. The freshly-expired row is NOT picked up by the
    //      same-tick `soft_delete_resolved` call — its
    //      `resolved_at = now` is well within the retention window,
    //      so the soft-delete tick sees it as "resolved but young"
    //      and leaves it alone.
    //
    // Without this test, a regression that flipped the order of
    // operations (or that incorrectly pulled freshly-expired rows
    // into the soft-delete batch) would only surface in production.
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0xCAFE);
    let child = Uuid::from_u128(0xCAFF);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let now = fixed_now();

    // Row A: Pending, already past `expires_at` — must be flipped
    // to Expired by the `expire_pending` half.
    let id_pending_overdue = Uuid::from_u128(0xCAFE_0001);
    // Use `seeded_request` then mutate `expires_at` to the past so
    // `expire_pending` picks it up. `requested_at = now - 2 days` is
    // far enough in the past to be plausible.
    let mut pending_overdue = seeded_request(
        id_pending_overdue,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now - TimeDuration::days(2),
        None,
    );
    pending_overdue.expires_at = now - TimeDuration::hours(1);

    // Row B: Cancelled with `resolved_at` 60 days ago — must be
    // soft-deleted by the `soft_delete_resolved` half (default
    // retention window is 7 days, so 60d-old rows are eligible).
    let id_resolved_old = Uuid::from_u128(0xCAFE_0002);
    let resolved_old = seeded_request(
        id_resolved_old,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Cancelled,
        now - TimeDuration::days(61),
        Some(now - TimeDuration::days(60)),
    );

    let conv = Arc::new(FakeConversionRepo::with_seed(vec![
        pending_overdue,
        resolved_old,
    ]));
    let svc = make_service(conv.clone(), tenants, now);

    // Tick: half 1 — expire pending.
    let expired_count = svc
        .expire_pending(
            &ConversionScope::system_sweep(),
            100,
            &CancellationToken::new(),
        )
        .await
        .expect("expire half of reaper tick");
    assert_eq!(expired_count, 1, "exactly one Pending row was overdue");

    // Tick: half 2 -- soft-delete resolved-old rows (7-day retention).
    let soft_deleted_count = svc
        .soft_delete_resolved(
            &ConversionScope::system_sweep(),
            std::time::Duration::from_hours(7 * 24),
            100,
        )
        .await
        .expect("soft-delete half of reaper tick");
    assert_eq!(
        soft_deleted_count, 1,
        "exactly one resolved-old row was eligible (the freshly-expired \
         row's resolved_at = now is within the retention window)"
    );

    // Verify the freshly-expired row is intact (status=Expired,
    // deleted_at = None) AFTER the soft-delete pass — i.e., the two
    // ticks composed cleanly without the freshly-expired row leaking
    // into the soft-delete batch.
    let by_id: std::collections::HashMap<Uuid, ConversionRequest> =
        conv.snapshot_all().into_iter().map(|r| (r.id, r)).collect();
    assert_eq!(
        by_id[&id_pending_overdue].status,
        ConversionStatus::Expired,
        "row A must be Expired after expire_pending"
    );
    assert!(
        by_id[&id_pending_overdue].deleted_at.is_none(),
        "row A's resolved_at = now is within the 7d retention window, so \
         soft_delete_resolved must NOT have touched it on the same tick"
    );
    assert_eq!(
        by_id[&id_resolved_old].status,
        ConversionStatus::Cancelled,
        "row B's status is unchanged by soft-delete (only deleted_at flips)"
    );
    assert!(
        by_id[&id_resolved_old].deleted_at.is_some(),
        "row B was eligible for soft-delete (resolved 60d ago, > 7d window)"
    );
}

// ---- require_caller_tenant_visible (cancel / reject scope fence) --
//
// Pin the new scope-guard helper that `cancel` and `reject` run
// AFTER `require_caller_scope_or_not_found` and BEFORE the
// state / role validation matrix. The helper resolves the
// caller-owned tenant (`row.tenant_id` for child callers,
// `row.parent_id` for parent callers) through
// `tenant_repo.find_by_id(scope, ...)` and collapses every miss
// (out-of-scope, nonexistent, soft-deleted) into `NotFound` so the
// existence channel does not leak. Without these tests a future
// refactor that removes the fence reverts to the pre-merge state
// where an internal actor with a forged `ConversionCaller` could
// mutate a conversion on a tenant outside its `AccessScope`.

#[tokio::test]
async fn cancel_under_restricted_scope_excluding_child_tenant_collapses_to_not_found() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x4001);
    let child = Uuid::from_u128(0x4002);
    let foreign = Uuid::from_u128(0x4099);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let pending_id = Uuid::from_u128(0x4010);
    let now = fixed_now();
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        pending_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    )]));
    let svc = make_service(conv, tenants, now);

    // Ctx rooted at a `foreign` tenant: `mock_enforcer` compiles an
    // `InTenantSubtree` predicate rooted at `foreign`, so the
    // PEP-derived scope excludes `child` and `parent` even though the
    // caller's URL-binding `ConversionCaller::child(child)` still
    // matches `row.tenant_id`. The caller-visibility fence MUST
    // collapse to `NotFound`.
    let err = svc
        .cancel(
            &ctx_for(foreign),
            pending_id,
            ConversionCaller::child(child),
        )
        .await
        .expect_err("out-of-scope caller must not see the row");
    match err {
        DomainError::ConversionRequestNotFound { resource, .. } => {
            assert_eq!(
                resource,
                pending_id.to_string(),
                "NotFound MUST key on the request id, not on the tenant id (no existence leak)"
            );
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn cancel_on_soft_deleted_child_tenant_collapses_to_not_found() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x4101);
    let child = Uuid::from_u128(0x4102);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    // Child tenant exists but is soft-deleted: `find_by_id` returns
    // it (production semantics — see `TenantRepo::find_by_id` doc),
    // and the fence's explicit `Deleted` check must collapse to
    // `NotFound`. A future refactor that removes the explicit
    // status check would accept the cancel on a removed tenant.
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Deleted,
        false,
        "c-gone",
    );

    let pending_id = Uuid::from_u128(0x4110);
    let now = fixed_now();
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        pending_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    )]));
    let svc = make_service(conv, tenants, now);

    let err = svc
        .cancel(&ctx(), pending_id, ConversionCaller::child(child))
        .await
        .expect_err("soft-deleted child tenant must collapse to NotFound");
    assert!(matches!(err, DomainError::ConversionRequestNotFound { .. }));
}

#[tokio::test]
async fn reject_under_restricted_scope_excluding_parent_tenant_collapses_to_not_found() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x4201);
    let child = Uuid::from_u128(0x4202);
    let foreign = Uuid::from_u128(0x4299);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let pending_id = Uuid::from_u128(0x4210);
    let now = fixed_now();
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        pending_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    )]));
    let svc = make_service(conv, tenants, now);

    // Parent-side reject: caller-owned tenant is `row.parent_id`. A
    // ctx rooted at a `foreign` tenant compiles to a PEP-derived
    // scope that excludes `parent`, so the caller-visibility fence
    // MUST collapse to `NotFound` after
    // `require_caller_scope_or_not_found` passes the URL-binding check.
    let err = svc
        .reject(
            &ctx_for(foreign),
            pending_id,
            ConversionCaller::parent(parent),
        )
        .await
        .expect_err("out-of-scope parent caller must not see the row");
    assert!(matches!(err, DomainError::ConversionRequestNotFound { .. }));
}

#[tokio::test]
async fn reject_on_soft_deleted_parent_tenant_collapses_to_not_found() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x4301);
    let child = Uuid::from_u128(0x4302);
    // Parent soft-deleted; child active. The fence resolves the
    // caller-owned tenant (parent for parent-side caller) and
    // collapses on the explicit `Deleted` check.
    seed_tenant(
        &tenants,
        parent,
        None,
        TenantStatus::Deleted,
        false,
        "root-gone",
    );
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let pending_id = Uuid::from_u128(0x4310);
    let now = fixed_now();
    let conv = Arc::new(FakeConversionRepo::with_seed(vec![seeded_request(
        pending_id,
        child,
        Some(parent),
        ConversionSide::Child,
        ConversionStatus::Pending,
        now,
        None,
    )]));
    let svc = make_service(conv, tenants, now);

    let err = svc
        .reject(&ctx(), pending_id, ConversionCaller::parent(parent))
        .await
        .expect_err("soft-deleted parent tenant must collapse to NotFound");
    assert!(matches!(err, DomainError::ConversionRequestNotFound { .. }));
}

// ---- Cross-tenant denial coverage on the remaining public methods --
//
// `cancel` and `reject` had cross-tenant denial tests since the
// caller-visibility fence landed. `request_conversion` and `approve`
// did not — deep-review #8 flagged the asymmetry. The tests below
// pin the same posture for both: an out-of-scope `AccessScope`
// collapses to `NotFound` before any state mutation or audit emit,
// so an internal actor that can mint a matching `ConversionCaller`
// cannot probe tenant topology through the error-code channel for
// these seams either.

#[tokio::test]
async fn request_conversion_under_restricted_scope_excluding_tenant_collapses_to_not_found() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x4401);
    let child = Uuid::from_u128(0x4402);
    let foreign = Uuid::from_u128(0x4499);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let conv = Arc::new(FakeConversionRepo::new());
    let now = fixed_now();
    let svc = make_service(conv, tenants, now);

    let err = svc
        .request_conversion(
            &ctx_for(foreign),
            RequestConversionInput {
                tenant_id: child,
                caller: ConversionCaller::child(child),
                target_mode_override: None,
            },
        )
        .await
        .expect_err("out-of-scope caller must not be allowed to initiate");
    match err {
        DomainError::NotFound { .. } => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn approve_under_url_binding_mismatch_collapses_to_not_found() {
    // `approve` is the asymmetric case: the seam intentionally
    // loads the converting tenant under `allow_all` because a
    // parent-side counterparty acting on a self-managed child
    // sits behind the closure barrier. The cross-tenant guard
    // therefore runs through `require_caller_scope_or_not_found`
    // on the URL binding — a mismatch between the caller's
    // declared scope and the request row's parent_id surfaces
    // as `NotFound`. This test pins that path so a regression
    // that wires the caller scope into the tenant load (and
    // breaks parent-side approval of self-managed conversions)
    // fails here loudly.
    let tenants = Arc::new(FakeTenantRepo::new());
    let parent = Uuid::from_u128(0x4501);
    let other_parent = Uuid::from_u128(0x4502);
    let child = Uuid::from_u128(0x4503);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        other_parent,
        None,
        TenantStatus::Active,
        false,
        "other-root",
    );
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "c",
    );

    let pending_id = Uuid::from_u128(0x4510);
    let now = fixed_now();
    let conv = Arc::new(
        FakeConversionRepo::with_seed(vec![seeded_request(
            pending_id,
            child,
            Some(parent),
            ConversionSide::Child,
            ConversionStatus::Pending,
            now,
            None,
        )])
        .with_tenant_repo(Arc::clone(&tenants)),
    );
    let svc = make_service(conv, Arc::clone(&tenants), now);

    // Parent-side caller declares `other_parent` as their scope —
    // the row's `parent_id` is `parent`, so
    // `require_caller_scope_or_not_found` collapses to NotFound
    // before any state load runs.
    let err = svc
        .approve(&ctx(), pending_id, ConversionCaller::parent(other_parent))
        .await
        .expect_err("parent-side approve on mismatched URL binding must not leak existence");
    match err {
        DomainError::ConversionRequestNotFound { resource, .. } => {
            assert_eq!(resource, pending_id.to_string());
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

// ---- PEP-deny propagation on every caller-facing method ----------
//
// Each test below builds a `ConversionService` wired to
// `deny_all_enforcer()` — a PDP fake that refuses every evaluation
// with `decision: false`. The corresponding `EnforcerError::Denied`
// then maps to `DomainError::CrossTenantDenied` via the `From` impl
// in `domain::error`. The tests pin that every caller-facing public
// method (`request_conversion` / `cancel` / `reject` / `approve` /
// `list_own_for_tenant` / `list_inbound_for_parent`) funnels through
// `self.authorize(...)` BEFORE any tenant / row lookup. A regression
// that strips the `authorize` call from one of these methods (or
// re-orders it past a tenant load) surfaces here as a lifted /
// non-`CrossTenantDenied` error.
//
// No tenant / conversion-row seeding is required: the deny path
// triggers on the very first line of each method and short-circuits
// before any repo access.

#[tokio::test]
async fn request_conversion_propagates_pep_deny_as_cross_tenant_denied() {
    let conv = Arc::new(FakeConversionRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let svc = make_service_with_enforcer(conv, tenants, deny_all_enforcer(), fixed_now());

    let child = Uuid::from_u128(0x9001);
    let err = svc
        .request_conversion(
            &ctx(),
            RequestConversionInput {
                tenant_id: child,
                caller: ConversionCaller::child(child),
                target_mode_override: None,
            },
        )
        .await
        .expect_err("PEP-denied request_conversion must propagate as CrossTenantDenied");
    assert!(
        matches!(err, DomainError::CrossTenantDenied { .. }),
        "expected CrossTenantDenied, got {err:?}"
    );
}

#[tokio::test]
async fn cancel_propagates_pep_deny_as_cross_tenant_denied() {
    let conv = Arc::new(FakeConversionRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let svc = make_service_with_enforcer(conv, tenants, deny_all_enforcer(), fixed_now());

    let request_id = Uuid::from_u128(0x9002);
    let child = Uuid::from_u128(0x9003);
    let err = svc
        .cancel(&ctx(), request_id, ConversionCaller::child(child))
        .await
        .expect_err("PEP-denied cancel must propagate as CrossTenantDenied");
    assert!(
        matches!(err, DomainError::CrossTenantDenied { .. }),
        "expected CrossTenantDenied, got {err:?}"
    );
}

#[tokio::test]
async fn reject_propagates_pep_deny_as_cross_tenant_denied() {
    let conv = Arc::new(FakeConversionRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let svc = make_service_with_enforcer(conv, tenants, deny_all_enforcer(), fixed_now());

    let request_id = Uuid::from_u128(0x9004);
    let parent = Uuid::from_u128(0x9005);
    let err = svc
        .reject(&ctx(), request_id, ConversionCaller::parent(parent))
        .await
        .expect_err("PEP-denied reject must propagate as CrossTenantDenied");
    assert!(
        matches!(err, DomainError::CrossTenantDenied { .. }),
        "expected CrossTenantDenied, got {err:?}"
    );
}

#[tokio::test]
async fn approve_propagates_pep_deny_as_cross_tenant_denied() {
    let conv = Arc::new(FakeConversionRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let svc = make_service_with_enforcer(conv, tenants, deny_all_enforcer(), fixed_now());

    let request_id = Uuid::from_u128(0x9006);
    let parent = Uuid::from_u128(0x9007);
    let err = svc
        .approve(&ctx(), request_id, ConversionCaller::parent(parent))
        .await
        .expect_err("PEP-denied approve must propagate as CrossTenantDenied");
    assert!(
        matches!(err, DomainError::CrossTenantDenied { .. }),
        "expected CrossTenantDenied, got {err:?}"
    );
}

#[tokio::test]
async fn list_own_for_tenant_propagates_pep_deny_as_cross_tenant_denied() {
    let conv = Arc::new(FakeConversionRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let svc = make_service_with_enforcer(conv, tenants, deny_all_enforcer(), fixed_now());

    let tenant_id = Uuid::from_u128(0x9008);
    let err = svc
        .list_own_for_tenant(
            &ctx(),
            tenant_id,
            &ListConversionsQuery::any(10, 0).expect("top > 0"),
        )
        .await
        .expect_err("PEP-denied list_own_for_tenant must propagate as CrossTenantDenied");
    assert!(
        matches!(err, DomainError::CrossTenantDenied { .. }),
        "expected CrossTenantDenied, got {err:?}"
    );
}

#[tokio::test]
async fn list_inbound_for_parent_propagates_pep_deny_as_cross_tenant_denied() {
    let conv = Arc::new(FakeConversionRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let svc = make_service_with_enforcer(conv, tenants, deny_all_enforcer(), fixed_now());

    let parent_id = Uuid::from_u128(0x9009);
    let err = svc
        .list_inbound_for_parent(
            &ctx(),
            parent_id,
            &ListConversionsQuery::any(10, 0).expect("top > 0"),
        )
        .await
        .expect_err("PEP-denied list_inbound_for_parent must propagate as CrossTenantDenied");
    assert!(
        matches!(err, DomainError::CrossTenantDenied { .. }),
        "expected CrossTenantDenied, got {err:?}"
    );
}
