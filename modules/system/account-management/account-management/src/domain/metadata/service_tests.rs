//! Unit tests for [`MetadataService`].
//!
//! Every test wires the service against the in-crate fakes
//! ([`FakeMetadataRepo`], [`FakeTenantRepo`]) plus a
//! [`StubMetadataSchemaRegistry`] and a deterministic `now_fn`. This
//! pins:
//!
//! * Guard ordering: tenant existence + status guard runs BEFORE any
//!   registry / metadata-repo call on every flow.
//! * Distinct-404 disambiguation: `MetadataSchemaNotRegistered` for
//!   unknown schemas, `MetadataEntryNotFound` for unset rows under a
//!   known schema (per FEATURE §6 AC line 392 / `dod-tenant-metadata-distinct-404-codes`).
//! * Walk-up algorithm: own-first short-circuit, `override_only`
//!   short-circuit, start-tenant barrier, mid-walk barrier-stop,
//!   suspended-skip, root-empty terminal.
//! * PUT idempotency: same `(tenant, schema)` written twice returns
//!   `was_inserted = false` on the second call, preserves
//!   `created_at`, advances `updated_at` per FEATURE-doc semantics
//!   surfaced by the Phase 1 fake.
//! * LIST ordering + pagination: stable on `schema_uuid`, in-service
//!   `top` / `skip` slicing per the FEATURE-doc list flow (no
//!   ancestor walk).

#![allow(
    clippy::too_many_lines,
    reason = "service-test fixtures intentionally seed multi-row hierarchies inline so each test reads as a self-contained scenario; splitting them would scatter the seeded shape across helpers and obscure walk-up branch coverage"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test-support fakes panic on poisoned mutex; the canonical `expect(\"…\")` form is shared with FakeConversionRepo's tests"
)]

use std::sync::Arc;

use account_management_sdk::{MetadataEntry, UpsertMetadataRequest};
use modkit_odata::ODataQuery;
use modkit_security::{AccessScope, SecurityContext};
use serde_json::{Value, json};
use time::{Duration as TimeDuration, OffsetDateTime};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::metadata::registry::{InheritancePolicy, StubMetadataSchemaRegistry};
use crate::domain::metadata::repo::MetadataRepo;
use crate::domain::metadata::service::MetadataService;
use crate::domain::metadata::test_support::FakeMetadataRepo;
use crate::domain::tenant::model::{TenantModel, TenantStatus};
use crate::domain::tenant::test_support::{
    FakeTenantRepo, mock_enforcer, schema_selective_enforcer, schema_unavailable_enforcer,
};
use authz_resolver_sdk::PolicyEnforcer;
use gts::GtsSchemaId;

// ---- helpers -------------------------------------------------------

fn fixed_now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch")
}

fn scope() -> AccessScope {
    AccessScope::allow_all()
}

/// Subject tenant id used by every service-test `ctx()`. The
/// `MockAuthZResolver` returns an `InTenantSubtree` predicate rooted
/// at this id, and `seed_tenant` materialises closure rows
/// `(subject_root, tenant, barrier = 0)` so the PEP-derived scope
/// resolves to a non-empty visible set on the `FakeTenantRepo`.
const fn ctx_subject_root() -> Uuid {
    Uuid::from_u128(0xCAFE_BABE)
}

fn ctx() -> SecurityContext {
    // The seeded `subject_tenant_id` matches `ctx_subject_root()` so
    // closure-row seeding in `seed_tenant` keeps every test tenant
    // visible under the compiled PEP scope. Tests asserting on
    // explicit PEP-deny paths use `constraint_bearing_enforcer` —
    // not exercised here; the metadata-service unit tests focus on
    // service-layer guard ordering / policy mapping (PEP deny is
    // covered by the tenant-service tests).
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0xCAFE))
        .subject_tenant_id(ctx_subject_root())
        .build()
        .expect("ctx")
}

/// Build a fresh first-page `ODataQuery` for tests. The repo fake
/// honours `limit` only; cursor mechanics are covered by
/// `modkit_db::odata::sea_orm_filter` and the integration test
/// against the real SeaORM-backed repo.
fn first_page() -> ODataQuery {
    ODataQuery::default()
}

fn schema_a() -> GtsSchemaId {
    GtsSchemaId::new("gts.cf.core.am.tenant_metadata.v1~vendor.app.metadata.theme.v1~")
}

fn schema_b() -> GtsSchemaId {
    GtsSchemaId::new("gts.cf.core.am.tenant_metadata.v1~vendor.app.metadata.billing.v1~")
}

fn schema_unknown() -> GtsSchemaId {
    GtsSchemaId::new("gts.cf.core.am.tenant_metadata.v1~vendor.app.metadata.absent.v1~")
}

/// Compute the same deterministic `UUIDv5` the service / repo use
/// internally, via the upstream `gts` crate. Test-local helper —
/// service-side flow validates schema ids via `ParsedSchemaId::parse`
/// before reaching the registry, so test inputs are always valid here.
#[allow(
    clippy::expect_used,
    reason = "test helpers only see hand-crafted valid schema ids"
)]
fn schema_uuid_for(schema_id: &str) -> Uuid {
    gts::GtsID::new(schema_id)
        .expect("valid GTS id in tests")
        .to_uuid()
}

fn make_service(
    md_repo: Arc<FakeMetadataRepo>,
    tenant_repo: Arc<FakeTenantRepo>,
    registry: Arc<StubMetadataSchemaRegistry>,
    now: OffsetDateTime,
) -> MetadataService {
    make_service_with_enforcer(md_repo, tenant_repo, registry, now, mock_enforcer())
}

/// Sibling of [`make_service`] that lets the caller swap the PDP fake.
/// Used by the per-row schema-deny tests, which need
/// [`schema_selective_enforcer`] / [`schema_unavailable_enforcer`]
/// instead of the permissive [`mock_enforcer`].
fn make_service_with_enforcer(
    md_repo: Arc<FakeMetadataRepo>,
    tenant_repo: Arc<FakeTenantRepo>,
    registry: Arc<StubMetadataSchemaRegistry>,
    now: OffsetDateTime,
    enforcer: PolicyEnforcer,
) -> MetadataService {
    let now_fn = Arc::new(move || now);
    MetadataService::new(md_repo, tenant_repo, registry, enforcer).with_now_fn(now_fn)
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
        deleted_at: None,
    });
    // Closure rows the `MockAuthZResolver`-derived
    // `InTenantSubtree(root = ctx().subject_tenant_id)` predicate
    // consults via `FakeTenantRepo::visible_ids_for`. Without the
    // closure entries every PEP-derived scope would resolve to an
    // empty visible set and every tenant lookup would surface as
    // `NotFound`. Seed two rows per tenant:
    //
    // * `(subject_tenant, id, barrier = 0)` — makes the tenant
    //   visible under the test ctx's subject subtree.
    // * `(id, id, barrier = 0)` — the production "self-row" every
    //   tenant carries in `tenant_closure`; lets a scope rooted at
    //   `id` itself resolve trivially.
    let subject_root = ctx_subject_root();
    fake.seed_closure(subject_root, id, 0, status);
    if subject_root != id {
        fake.seed_closure(id, id, 0, status);
    }
}

async fn seed_metadata_row(
    fake: &FakeMetadataRepo,
    tenant_id: Uuid,
    schema_id: &GtsSchemaId,
    value: Value,
    when: OffsetDateTime,
) {
    let schema_uuid = schema_uuid_for(schema_id.as_ref());
    // Drive the seed through the trait's upsert path so the
    // created_at / updated_at semantics match production exactly. We
    // feed `when` as the upsert timestamp; subsequent rewrites stamp
    // a different `now`. Awaiting `upsert_for_tenant` directly (rather
    // than `futures::executor::block_on`) keeps the seed runtime-safe:
    // a future FakeRepo that does real async work won't deadlock the
    // tokio worker.
    let scope = scope();
    fake.upsert_for_tenant(&scope, tenant_id, schema_uuid, value, when, None)
        .await
        .expect("seed upsert");
}

// ---- list_metadata ----------------------------------------------

#[tokio::test]
async fn list_happy_path_returns_only_direct_rows_in_uuid_order() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![
        (schema_a(), InheritancePolicy::OverrideOnly),
        (schema_b(), InheritancePolicy::OverrideOnly),
    ]));
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
    // Seed direct rows on `child` for both schemas, plus a row on
    // the parent that MUST NOT surface (list flow does NOT walk).
    seed_metadata_row(
        &md_repo,
        child,
        &schema_a(),
        json!({"theme": "dark"}),
        fixed_now(),
    )
    .await;
    seed_metadata_row(
        &md_repo,
        child,
        &schema_b(),
        json!({"plan": "pro"}),
        fixed_now(),
    )
    .await;
    seed_metadata_row(
        &md_repo,
        parent,
        &schema_a(),
        json!({"theme": "ancestor"}),
        fixed_now(),
    )
    .await;

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let page = svc
        .list_metadata(&ctx(), child, &first_page())
        .await
        .expect("list happy path");

    assert_eq!(page.items.len(), 2, "two direct rows on child only");
    // Stable order on schema_uuid mirrors the repo contract; we just
    // assert both schemas are surfaced and each entry carries the
    // re-hydrated chained id.
    let hydrated: Vec<&GtsSchemaId> = page.items.iter().map(|e| &e.schema_id).collect();
    let sa = schema_a();
    let sb = schema_b();
    assert!(hydrated.contains(&&sa), "schema_a hydrated");
    assert!(hydrated.contains(&&sb), "schema_b hydrated");
}

#[tokio::test]
async fn list_pagination_limit_caps_page_size() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![
        (schema_a(), InheritancePolicy::OverrideOnly),
        (schema_b(), InheritancePolicy::OverrideOnly),
    ]));
    let tid = Uuid::from_u128(0x10);
    seed_tenant(&tenants, tid, None, TenantStatus::Active, false, "root");
    seed_metadata_row(&md_repo, tid, &schema_a(), json!({}), fixed_now()).await;
    seed_metadata_row(&md_repo, tid, &schema_b(), json!({}), fixed_now()).await;

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    // `$top = 1` caps the first page to one row. Cursor mechanics
    // (next-page traversal, filter-hash consistency) are exercised by
    // the SeaORM-backed integration tests; the fake honours the
    // limit only, which is enough to pin the service-level contract
    // here.
    let page = svc
        .list_metadata(&ctx(), tid, &ODataQuery::default().with_limit(1))
        .await
        .expect("list page");

    assert_eq!(page.items.len(), 1, "top=1 caps the slice");
    assert_eq!(page.page_info.limit, 1, "page_info echoes the limit");
}

#[tokio::test]
async fn list_rejects_unknown_tenant_with_not_found() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::new());
    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let err = svc
        .list_metadata(&ctx(), Uuid::from_u128(0xDEAD), &first_page())
        .await
        .expect_err("unknown tenant must reject");

    assert!(matches!(err, DomainError::NotFound { .. }), "got {err:?}");
}

#[tokio::test]
async fn list_accepts_suspended_tenant_per_feature_spec() {
    // FEATURE doc + DESIGN row "Metadata steward" allow Metadata.list
    // on visible tenants regardless of suspension state. A suspended
    // tenant with no rows yields an empty page, not a Validation
    // error — the earlier rejection was a stricter-than-spec gate
    // that this test now pins as fixed.
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::new());
    let tid = Uuid::from_u128(0x100);
    seed_tenant(&tenants, tid, None, TenantStatus::Suspended, false, "susp");

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let page = svc
        .list_metadata(&ctx(), tid, &first_page())
        .await
        .expect("suspended tenant must be allowed to list per feature spec");
    assert!(page.items.is_empty(), "no rows seeded; expected empty page");
}

#[tokio::test]
async fn list_rejects_deleted_tenant_with_validation() {
    // Visibility gate still rejects `Deleted` (and `Provisioning`)
    // — `resolve_visible_tenant` accepts only Active + Suspended.
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::new());
    let tid = Uuid::from_u128(0x100);
    seed_tenant(&tenants, tid, None, TenantStatus::Deleted, false, "gone");

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let err = svc
        .list_metadata(&ctx(), tid, &first_page())
        .await
        .expect_err("deleted tenant must reject list");

    assert!(matches!(err, DomainError::Validation { .. }), "got {err:?}");
}

#[tokio::test]
async fn list_drops_rows_for_schemas_caller_cannot_read() {
    // PRD §1848 contract: "list responses omit entries the actor is
    // not permitted to read". Per-row `Metadata.read` PDP recheck in
    // `list_metadata` MUST silently drop `CrossTenantDenied` rows
    // rather than fail the whole call. Seed two rows on the same
    // tenant — schema_a and schema_b — and wire a PDP fake that
    // allows `Metadata.read` only on schema_a; assert the listing
    // surfaces ONLY the schema_a row.
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![
        (schema_a(), InheritancePolicy::OverrideOnly),
        (schema_b(), InheritancePolicy::OverrideOnly),
    ]));
    let tid = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tid, None, TenantStatus::Active, false, "root");
    seed_metadata_row(
        &md_repo,
        tid,
        &schema_a(),
        json!({"theme": "dark"}),
        fixed_now(),
    )
    .await;
    seed_metadata_row(
        &md_repo,
        tid,
        &schema_b(),
        json!({"plan": "pro"}),
        fixed_now(),
    )
    .await;

    let enforcer = schema_selective_enforcer(vec![schema_a().to_string()]);
    let svc = make_service_with_enforcer(md_repo, tenants, registry, fixed_now(), enforcer);

    let page = svc
        .list_metadata(&ctx(), tid, &first_page())
        .await
        .expect("list allowed (outer LIST gate passes; per-row deny is silent-drop)");

    let hydrated: Vec<&GtsSchemaId> = page.items.iter().map(|e| &e.schema_id).collect();
    let sa = schema_a();
    let sb = schema_b();
    assert_eq!(
        hydrated.len(),
        1,
        "exactly one row must surface (schema_a allowed, schema_b denied)"
    );
    assert!(
        hydrated.contains(&&sa),
        "schema_a (allowed) MUST be present"
    );
    assert!(
        !hydrated.contains(&&sb),
        "schema_b (denied) MUST be omitted, not surfaced as an error"
    );
}

#[tokio::test]
async fn list_propagates_transport_failure_from_per_row_authz() {
    // Negative path on the silent-drop contract: only
    // `CrossTenantDenied` is dropped. Any other error — including a
    // PDP transport failure surfacing as
    // `DomainError::ServiceUnavailable` — MUST propagate out of
    // `list_metadata` rather than be silent-dropped together with
    // legitimate denies. Without this pin, a misclassified error
    // mapping in `caller_allows_schema_read` could silently hide the
    // whole tenant's metadata behind a transport blip.
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let tid = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tid, None, TenantStatus::Active, false, "root");
    seed_metadata_row(
        &md_repo,
        tid,
        &schema_a(),
        json!({"theme": "dark"}),
        fixed_now(),
    )
    .await;

    let enforcer = schema_unavailable_enforcer();
    let svc = make_service_with_enforcer(md_repo, tenants, registry, fixed_now(), enforcer);

    let err = svc
        .list_metadata(&ctx(), tid, &first_page())
        .await
        .expect_err("transport failure on per-row READ MUST propagate, not silent-drop");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "expected ServiceUnavailable, got {err:?}"
    );
}

// ---- get_metadata -----------------------------------------------

#[tokio::test]
async fn get_happy_path_returns_entry() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let tid = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tid, None, TenantStatus::Active, false, "root");
    seed_metadata_row(
        &md_repo,
        tid,
        &schema_a(),
        json!({"theme": "dark"}),
        fixed_now(),
    )
    .await;

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let entry = svc
        .get_metadata(&ctx(), tid, schema_a())
        .await
        .expect("get happy path");

    assert_eq!(entry.schema_id, schema_a());
    assert_eq!(entry.value, json!({"theme": "dark"}));
}

#[tokio::test]
async fn get_unregistered_schema_returns_distinct_schema_404() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::new()); // empty
    let tid = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tid, None, TenantStatus::Active, false, "root");

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let err = svc
        .get_metadata(&ctx(), tid, schema_unknown())
        .await
        .expect_err("unregistered schema must reject");

    assert!(
        matches!(err, DomainError::MetadataSchemaNotRegistered { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn get_registered_schema_no_row_returns_distinct_entry_404() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let tid = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tid, None, TenantStatus::Active, false, "root");

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let err = svc
        .get_metadata(&ctx(), tid, schema_a())
        .await
        .expect_err("missing entry must reject");

    assert!(
        matches!(err, DomainError::MetadataEntryNotFound { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn get_rejects_unknown_tenant_before_registry_call() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    // Registry is empty: if guard order ever flips, the test would
    // surface MetadataSchemaNotRegistered instead of NotFound.
    let registry = Arc::new(StubMetadataSchemaRegistry::new());
    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let err = svc
        .get_metadata(&ctx(), Uuid::from_u128(0xDEAD), schema_a())
        .await
        .expect_err("unknown tenant must reject");

    assert!(
        matches!(err, DomainError::NotFound { .. }),
        "guard ordering: tenant before registry; got {err:?}"
    );
}

// ---- upsert_metadata -----------------------------------------------

#[tokio::test]
async fn put_happy_path_inserts_then_updates() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let tid = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tid, None, TenantStatus::Active, false, "root");

    let svc = make_service(md_repo.clone(), tenants, registry, fixed_now());

    let first: MetadataEntry = svc
        .upsert_metadata(
            &ctx(),
            tid,
            UpsertMetadataRequest::new(schema_a(), json!({"theme": "dark"})),
        )
        .await
        .expect("put insert");
    assert_eq!(first.value, json!({"theme": "dark"}));
    assert_eq!(first.version, 1, "new row seeds version=1");

    let second = svc
        .upsert_metadata(
            &ctx(),
            tid,
            UpsertMetadataRequest::new(schema_a(), json!({"theme": "light"})),
        )
        .await
        .expect("put update");
    assert_eq!(second.value, json!({"theme": "light"}));
    assert_eq!(second.version, 2, "update bumps version");
}

#[tokio::test]
async fn upsert_with_expected_version_match_bumps_version() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let tid = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tid, None, TenantStatus::Active, false, "root");
    let svc = make_service(md_repo, tenants, registry, fixed_now());

    // Seed via opt-out upsert (no precondition).
    let v1 = svc
        .upsert_metadata(
            &ctx(),
            tid,
            UpsertMetadataRequest::new(schema_a(), json!({"plan": "free"})),
        )
        .await
        .expect("seed insert");
    assert_eq!(v1.version, 1);

    // Caller round-trips the version through the upsert.
    let v2 = svc
        .upsert_metadata(
            &ctx(),
            tid,
            UpsertMetadataRequest::new(schema_a(), json!({"plan": "pro"}))
                .with_expected_version(v1.version),
        )
        .await
        .expect("matched expected_version");
    assert_eq!(v2.version, 2, "matched precondition still bumps version");
}

#[tokio::test]
async fn upsert_with_stale_expected_version_surfaces_mismatch() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let tid = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tid, None, TenantStatus::Active, false, "root");
    let svc = make_service(md_repo, tenants, registry, fixed_now());

    // Alice seeds → version=1.
    svc.upsert_metadata(
        &ctx(),
        tid,
        UpsertMetadataRequest::new(schema_a(), json!({"plan": "free"})),
    )
    .await
    .expect("alice seed");

    // Alice's second write bumps to version=2.
    svc.upsert_metadata(
        &ctx(),
        tid,
        UpsertMetadataRequest::new(schema_a(), json!({"plan": "pro"})).with_expected_version(1),
    )
    .await
    .expect("alice second write");

    // Bob still thinks the row is at v1 → mismatch (current=2).
    let err = svc
        .upsert_metadata(
            &ctx(),
            tid,
            UpsertMetadataRequest::new(schema_a(), json!({"plan": "enterprise"}))
                .with_expected_version(1),
        )
        .await
        .expect_err("stale expected_version must surface mismatch");
    match err {
        DomainError::MetadataVersionMismatch {
            expected, current, ..
        } => {
            assert_eq!(expected, 1);
            assert_eq!(current, 2);
        }
        other => panic!("expected MetadataVersionMismatch, got {other:?}"),
    }
}

#[tokio::test]
async fn upsert_with_expected_version_on_missing_row_surfaces_mismatch() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let tid = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tid, None, TenantStatus::Active, false, "root");
    let svc = make_service(md_repo, tenants, registry, fixed_now());

    // Caller expected an existing row at v5; in fact no row exists.
    let err = svc
        .upsert_metadata(
            &ctx(),
            tid,
            UpsertMetadataRequest::new(schema_a(), json!({"plan": "free"}))
                .with_expected_version(5),
        )
        .await
        .expect_err("non-zero expected on missing row must mismatch");
    match err {
        DomainError::MetadataVersionMismatch {
            expected, current, ..
        } => {
            assert_eq!(expected, 5);
            assert_eq!(current, 0, "missing row reports current=0");
        }
        other => panic!("expected MetadataVersionMismatch, got {other:?}"),
    }
}

#[tokio::test]
async fn upsert_without_expected_version_is_last_write_wins() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let tid = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tid, None, TenantStatus::Active, false, "root");
    let svc = make_service(md_repo, tenants, registry, fixed_now());

    // Two opt-out writes from different "callers" never collide
    // regardless of the underlying version drift.
    svc.upsert_metadata(
        &ctx(),
        tid,
        UpsertMetadataRequest::new(schema_a(), json!({"plan": "free"})),
    )
    .await
    .expect("first write (opt-out)");
    let v2 = svc
        .upsert_metadata(
            &ctx(),
            tid,
            UpsertMetadataRequest::new(schema_a(), json!({"plan": "pro"})),
        )
        .await
        .expect("second write (opt-out) wins last-write-wins");
    assert_eq!(v2.version, 2);
}

#[tokio::test]
async fn put_idempotent_same_value_preserves_created_at_advances_updated_at() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let tid = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tid, None, TenantStatus::Active, false, "root");

    let t0 = fixed_now();
    let svc_t0 = make_service(md_repo.clone(), tenants.clone(), registry.clone(), t0);
    let first = svc_t0
        .upsert_metadata(
            &ctx(),
            tid,
            UpsertMetadataRequest::new(schema_a(), json!({"theme": "dark"})),
        )
        .await
        .expect("put 1");
    assert_eq!(first.updated_at, t0);

    // Re-build the service with a later clock and PUT the SAME value
    // again. Insert path won't fire because the row exists; the
    // update path stamps `updated_at = t1` and preserves
    // `created_at = t0` (verified through the repo snapshot below).
    let t1 = t0 + TimeDuration::seconds(10);
    let svc_t1 = make_service(md_repo.clone(), tenants, registry, t1);
    let second = svc_t1
        .upsert_metadata(
            &ctx(),
            tid,
            UpsertMetadataRequest::new(schema_a(), json!({"theme": "dark"})),
        )
        .await
        .expect("put 2");
    assert_eq!(second.updated_at, t1, "updated_at advanced");

    // Inspect the underlying row through the snapshot helper to
    // verify created_at preservation (the public MetadataEntry only
    // surfaces updated_at).
    let snap = md_repo.snapshot_all();
    let row = snap.into_iter().next().expect("row exists");
    assert_eq!(row.created_at, t0, "created_at preserved");
    assert_eq!(row.updated_at, t1, "updated_at advanced to t1");
}

#[tokio::test]
async fn put_rejects_unknown_tenant_with_not_found() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let err = svc
        .upsert_metadata(
            &ctx(),
            Uuid::from_u128(0xDEAD),
            UpsertMetadataRequest::new(schema_a(), json!({"theme": "dark"})),
        )
        .await
        .expect_err("unknown tenant must reject");

    assert!(matches!(err, DomainError::NotFound { .. }), "got {err:?}");
}

#[tokio::test]
async fn put_rejects_unregistered_schema_with_distinct_404() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::new()); // empty
    let tid = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tid, None, TenantStatus::Active, false, "root");

    let svc = make_service(md_repo.clone(), tenants, registry, fixed_now());

    let err = svc
        .upsert_metadata(
            &ctx(),
            tid,
            UpsertMetadataRequest::new(schema_a(), json!({"theme": "dark"})),
        )
        .await
        .expect_err("unregistered schema must reject");

    assert!(
        matches!(err, DomainError::MetadataSchemaNotRegistered { .. }),
        "got {err:?}"
    );
    // Ensure no row was written.
    assert!(
        md_repo.snapshot_all().is_empty(),
        "no row written when schema unregistered"
    );
}

#[tokio::test]
async fn put_payload_failing_schema_validation_returns_validation_and_writes_nothing() {
    // FEATURE §6 AC line 393: a PUT whose payload fails the registered
    // GTS schema body validation MUST return `code=validation` WITHOUT
    // writing any row. Pin both halves: the error variant AND the
    // empty repo snapshot.
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    // Toggle the stub to reject every payload for `schema_a` — exercises
    // the validate_value branch independently of the JSON Schema body.
    registry.fail_validation_for(schema_a());
    let tid = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tid, None, TenantStatus::Active, false, "root");

    let svc = make_service(md_repo.clone(), tenants, registry, fixed_now());

    let err = svc
        .upsert_metadata(
            &ctx(),
            tid,
            UpsertMetadataRequest::new(schema_a(), json!({"theme": "dark"})),
        )
        .await
        .expect_err("body failing schema validation must reject");

    assert!(
        matches!(err, DomainError::Validation { .. }),
        "got {err:?} (expected DomainError::Validation per FEATURE §6 AC line 393)"
    );
    assert!(
        md_repo.snapshot_all().is_empty(),
        "no row written when payload fails schema validation"
    );
}

// ---- delete_metadata --------------------------------------------

#[tokio::test]
async fn delete_happy_path_removes_only_target_row() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![
        (schema_a(), InheritancePolicy::OverrideOnly),
        (schema_b(), InheritancePolicy::OverrideOnly),
    ]));
    let parent = Uuid::from_u128(0x1);
    let child = Uuid::from_u128(0x2);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "child",
    );
    // Direct row on child + ancestor row on parent. Delete on child
    // MUST leave the parent row intact (FEATURE §2 delete success
    // scenario: ancestor entries are NOT affected).
    seed_metadata_row(&md_repo, child, &schema_a(), json!({"v": 1}), fixed_now()).await;
    seed_metadata_row(&md_repo, child, &schema_b(), json!({"v": 2}), fixed_now()).await;
    seed_metadata_row(&md_repo, parent, &schema_a(), json!({"v": 0}), fixed_now()).await;

    let svc = make_service(md_repo.clone(), tenants, registry, fixed_now());

    svc.delete_metadata(&ctx(), child, schema_a())
        .await
        .expect("delete happy path");

    // Snapshot state assertions.
    let rows = md_repo.snapshot_all();
    assert_eq!(
        rows.len(),
        2,
        "child[a] removed; child[b] + parent[a] intact"
    );
    let parent_uuid = schema_uuid_for(&schema_a());
    assert!(
        rows.iter()
            .any(|r| r.tenant_id == parent && r.schema_uuid == parent_uuid),
        "parent ancestor row preserved on child delete"
    );
}

#[tokio::test]
async fn delete_returns_distinct_entry_404_on_missing_row() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let tid = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tid, None, TenantStatus::Active, false, "root");

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let err = svc
        .delete_metadata(&ctx(), tid, schema_a())
        .await
        .expect_err("missing row must reject");

    assert!(
        matches!(err, DomainError::MetadataEntryNotFound { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn delete_unregistered_schema_returns_distinct_schema_404() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::new()); // empty
    let tid = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tid, None, TenantStatus::Active, false, "root");

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let err = svc
        .delete_metadata(&ctx(), tid, schema_unknown())
        .await
        .expect_err("unregistered schema must reject");

    assert!(
        matches!(err, DomainError::MetadataSchemaNotRegistered { .. }),
        "got {err:?}"
    );
}

// ---- resolve_metadata: override_only ----------------------------

#[tokio::test]
async fn resolve_override_only_returns_own_value() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let parent = Uuid::from_u128(0x1);
    let child = Uuid::from_u128(0x2);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "child",
    );
    seed_metadata_row(
        &md_repo,
        child,
        &schema_a(),
        json!({"v": "child"}),
        fixed_now(),
    )
    .await;
    seed_metadata_row(
        &md_repo,
        parent,
        &schema_a(),
        json!({"v": "parent"}),
        fixed_now(),
    )
    .await;

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let entry = svc
        .resolve_metadata(&ctx(), child, schema_a())
        .await
        .expect("resolve own");

    let entry: MetadataEntry = entry.expect("own row present");
    assert_eq!(entry.value, json!({"v": "child"}), "own row wins");
}

#[tokio::test]
async fn resolve_override_only_returns_none_when_own_absent_even_if_ancestor_has_value() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let parent = Uuid::from_u128(0x1);
    let child = Uuid::from_u128(0x2);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "child",
    );
    // Only the parent has a row; child has none. override_only must
    // NOT walk up.
    seed_metadata_row(
        &md_repo,
        parent,
        &schema_a(),
        json!({"v": "parent"}),
        fixed_now(),
    )
    .await;

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let result = svc
        .resolve_metadata(&ctx(), child, schema_a())
        .await
        .expect("resolve override_only");

    assert!(
        result.is_none(),
        "override_only never inherits: got {result:?}"
    );
}

// ---- resolve_metadata: inherit walk-up ---------------------------

#[tokio::test]
async fn resolve_inherit_returns_own_when_present() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::Inherit,
    )]));
    let parent = Uuid::from_u128(0x1);
    let child = Uuid::from_u128(0x2);
    seed_tenant(&tenants, parent, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        child,
        Some(parent),
        TenantStatus::Active,
        false,
        "child",
    );
    seed_metadata_row(
        &md_repo,
        child,
        &schema_a(),
        json!({"v": "child"}),
        fixed_now(),
    )
    .await;
    seed_metadata_row(
        &md_repo,
        parent,
        &schema_a(),
        json!({"v": "parent"}),
        fixed_now(),
    )
    .await;

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let entry = svc
        .resolve_metadata(&ctx(), child, schema_a())
        .await
        .expect("resolve own (inherit)")
        .expect("own row present");

    assert_eq!(
        entry.value,
        json!({"v": "child"}),
        "own row wins under inherit too"
    );
}

#[tokio::test]
async fn resolve_inherit_walks_to_first_ancestor_value() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::Inherit,
    )]));
    // root → mid → leaf; only root has the value.
    let root = Uuid::from_u128(0x1);
    let mid = Uuid::from_u128(0x2);
    let leaf = Uuid::from_u128(0x3);
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
    seed_metadata_row(
        &md_repo,
        root,
        &schema_a(),
        json!({"v": "root"}),
        fixed_now(),
    )
    .await;

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let entry = svc
        .resolve_metadata(&ctx(), leaf, schema_a())
        .await
        .expect("resolve walk-up")
        .expect("root value reached");

    assert_eq!(entry.value, json!({"v": "root"}));
}

#[tokio::test]
async fn resolve_inherit_stops_at_self_managed_start_tenant_barrier() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::Inherit,
    )]));
    let root = Uuid::from_u128(0x1);
    let leaf = Uuid::from_u128(0x2);
    seed_tenant(&tenants, root, None, TenantStatus::Active, false, "root");
    // leaf is self-managed -> start-tenant barrier.
    seed_tenant(
        &tenants,
        leaf,
        Some(root),
        TenantStatus::Active,
        true,
        "leaf-sm",
    );
    seed_metadata_row(
        &md_repo,
        root,
        &schema_a(),
        json!({"v": "root"}),
        fixed_now(),
    )
    .await;

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let result = svc
        .resolve_metadata(&ctx(), leaf, schema_a())
        .await
        .expect("resolve start-barrier");

    assert!(
        result.is_none(),
        "self-managed start tenant never inherits: got {result:?}"
    );
}

#[tokio::test]
async fn resolve_inherit_stops_at_mid_walk_self_managed_ancestor_barrier() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::Inherit,
    )]));
    let root = Uuid::from_u128(0x1);
    let mid_sm = Uuid::from_u128(0x2);
    let leaf = Uuid::from_u128(0x3);
    seed_tenant(&tenants, root, None, TenantStatus::Active, false, "root");
    // mid is self-managed -> ancestor barrier-stop BEFORE reading its
    // value (even if mid had a row, the spec says barrier stops the
    // walk before reading the ancestor).
    seed_tenant(
        &tenants,
        mid_sm,
        Some(root),
        TenantStatus::Active,
        true,
        "mid-sm",
    );
    seed_tenant(
        &tenants,
        leaf,
        Some(mid_sm),
        TenantStatus::Active,
        false,
        "leaf",
    );
    seed_metadata_row(
        &md_repo,
        root,
        &schema_a(),
        json!({"v": "root"}),
        fixed_now(),
    )
    .await;
    // Even seed a row on mid_sm to verify the barrier stop fires
    // BEFORE the read.
    seed_metadata_row(
        &md_repo,
        mid_sm,
        &schema_a(),
        json!({"v": "mid"}),
        fixed_now(),
    )
    .await;

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let result = svc
        .resolve_metadata(&ctx(), leaf, schema_a())
        .await
        .expect("resolve mid-barrier");

    assert!(
        result.is_none(),
        "barrier-stop fires BEFORE reading the self-managed ancestor's value: got {result:?}"
    );
}

#[tokio::test]
async fn resolve_inherit_skips_suspended_ancestor_and_continues_to_grandparent() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::Inherit,
    )]));
    let root = Uuid::from_u128(0x1);
    let mid_susp = Uuid::from_u128(0x2);
    let leaf = Uuid::from_u128(0x3);
    seed_tenant(&tenants, root, None, TenantStatus::Active, false, "root");
    // mid is Suspended (not self-managed) -> walk SKIPS mid's read
    // and continues to root per the FEATURE-doc step 10 contract.
    seed_tenant(
        &tenants,
        mid_susp,
        Some(root),
        TenantStatus::Suspended,
        false,
        "mid-susp",
    );
    seed_tenant(
        &tenants,
        leaf,
        Some(mid_susp),
        TenantStatus::Active,
        false,
        "leaf",
    );
    // Even if mid had a value, the walk skips it (suspension is not a
    // barrier; the value is just not consulted on that hop). Verify
    // by seeding both rows and asserting the ROOT value wins.
    seed_metadata_row(
        &md_repo,
        mid_susp,
        &schema_a(),
        json!({"v": "mid"}),
        fixed_now(),
    )
    .await;
    seed_metadata_row(
        &md_repo,
        root,
        &schema_a(),
        json!({"v": "root"}),
        fixed_now(),
    )
    .await;

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let entry = svc
        .resolve_metadata(&ctx(), leaf, schema_a())
        .await
        .expect("resolve through suspended ancestor")
        .expect("root value reached");

    assert_eq!(
        entry.value,
        json!({"v": "root"}),
        "suspension is not a barrier: walk skips mid's read and reaches root"
    );
}

#[tokio::test]
async fn resolve_inherit_propagates_through_suspended_ancestor_when_value_lives_above() {
    // Slight variant of the prior test that pins the explicit
    // suspension-skip / continuation path: there is NO row on the
    // suspended ancestor, yet the walk MUST proceed past it (rather
    // than returning empty as a barrier-stop would).
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::Inherit,
    )]));
    let grand = Uuid::from_u128(0x10);
    let mid_susp = Uuid::from_u128(0x20);
    let leaf = Uuid::from_u128(0x30);
    seed_tenant(&tenants, grand, None, TenantStatus::Active, false, "grand");
    seed_tenant(
        &tenants,
        mid_susp,
        Some(grand),
        TenantStatus::Suspended,
        false,
        "mid-susp",
    );
    seed_tenant(
        &tenants,
        leaf,
        Some(mid_susp),
        TenantStatus::Active,
        false,
        "leaf",
    );
    seed_metadata_row(
        &md_repo,
        grand,
        &schema_a(),
        json!({"v": "grand"}),
        fixed_now(),
    )
    .await;

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let entry = svc
        .resolve_metadata(&ctx(), leaf, schema_a())
        .await
        .expect("walk through suspended -> grand")
        .expect("grand value reached");

    assert_eq!(entry.value, json!({"v": "grand"}));
}

#[tokio::test]
async fn resolve_inherit_returns_empty_when_root_reached_with_no_value() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::Inherit,
    )]));
    let root = Uuid::from_u128(0x1);
    let leaf = Uuid::from_u128(0x2);
    seed_tenant(&tenants, root, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        leaf,
        Some(root),
        TenantStatus::Active,
        false,
        "leaf",
    );
    // No rows seeded — walk reaches root and returns empty.

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let result = svc
        .resolve_metadata(&ctx(), leaf, schema_a())
        .await
        .expect("resolve root-empty");

    assert!(
        result.is_none(),
        "root reached without value -> empty terminal (NOT 404): got {result:?}"
    );
}

#[tokio::test]
async fn resolve_unregistered_schema_returns_distinct_404() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::new()); // empty
    let tid = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tid, None, TenantStatus::Active, false, "root");

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let err = svc
        .resolve_metadata(&ctx(), tid, schema_unknown())
        .await
        .expect_err("unregistered schema must reject");

    assert!(
        matches!(err, DomainError::MetadataSchemaNotRegistered { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn resolve_rejects_provisioning_start_tenant_at_guard() {
    // Provisioning is not Active, so the existence guard rejects
    // before any registry / metadata-repo call.
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::new());
    let tid = Uuid::from_u128(0x1);
    seed_tenant(
        &tenants,
        tid,
        None,
        TenantStatus::Provisioning,
        false,
        "prov",
    );

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let err = svc
        .resolve_metadata(&ctx(), tid, schema_a())
        .await
        .expect_err("provisioning tenant must reject");

    assert!(matches!(err, DomainError::Validation { .. }), "got {err:?}");
}

// ---- defence-in-depth check: own row returned even on self-managed start

#[tokio::test]
async fn resolve_inherit_returns_own_value_even_on_self_managed_start() {
    // FEATURE §3 step 2 says own values are ALWAYS returned
    // regardless of self_managed status — the barrier only blocks
    // INHERITANCE from ancestors.
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::Inherit,
    )]));
    let root = Uuid::from_u128(0x1);
    let leaf = Uuid::from_u128(0x2);
    seed_tenant(&tenants, root, None, TenantStatus::Active, false, "root");
    seed_tenant(
        &tenants,
        leaf,
        Some(root),
        TenantStatus::Active,
        true,
        "leaf-sm",
    );
    seed_metadata_row(
        &md_repo,
        leaf,
        &schema_a(),
        json!({"v": "own"}),
        fixed_now(),
    )
    .await;

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let entry = svc
        .resolve_metadata(&ctx(), leaf, schema_a())
        .await
        .expect("own row resolves even when leaf is self-managed")
        .expect("own row present");

    assert_eq!(entry.value, json!({"v": "own"}));
}

// ---- suspended-tenant gate (get / put / delete / resolve) -------
//
// `list_metadata` has `list_accepts_suspended_tenant_per_feature_spec`
// above (line ~308). The four blocks below pin the same FEATURE-spec
// contract on the remaining metadata operations so a future revert
// of `resolve_visible_tenant` back to Active-only would trip each
// op's pin individually.

#[tokio::test]
async fn get_accepts_suspended_tenant_per_feature_spec() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let tid = Uuid::from_u128(0x100);
    seed_tenant(&tenants, tid, None, TenantStatus::Suspended, false, "susp");
    seed_metadata_row(
        &md_repo,
        tid,
        &schema_a(),
        json!({"theme": "dark"}),
        fixed_now(),
    )
    .await;

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let entry = svc
        .get_metadata(&ctx(), tid, schema_a())
        .await
        .expect("get_metadata MUST accept Suspended tenant per FEATURE spec");
    assert_eq!(entry.value, json!({"theme": "dark"}));
}

#[tokio::test]
async fn put_accepts_suspended_tenant_per_feature_spec() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let tid = Uuid::from_u128(0x100);
    seed_tenant(&tenants, tid, None, TenantStatus::Suspended, false, "susp");

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let _entry = svc
        .upsert_metadata(
            &ctx(),
            tid,
            UpsertMetadataRequest::new(schema_a(), json!({"plan": "free"})),
        )
        .await
        .expect("upsert_metadata MUST accept Suspended tenant per FEATURE spec");
}

#[tokio::test]
async fn delete_accepts_suspended_tenant_per_feature_spec() {
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let tid = Uuid::from_u128(0x100);
    seed_tenant(&tenants, tid, None, TenantStatus::Suspended, false, "susp");
    seed_metadata_row(
        &md_repo,
        tid,
        &schema_a(),
        json!({"theme": "dark"}),
        fixed_now(),
    )
    .await;

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    svc.delete_metadata(&ctx(), tid, schema_a())
        .await
        .expect("delete_metadata MUST accept Suspended tenant per FEATURE spec");
}

#[tokio::test]
async fn resolve_accepts_suspended_tenant_per_feature_spec() {
    // Resolve from a Suspended start tenant — the walk-up algorithm
    // applies the documented suspended-skip rule to ancestors; the
    // start tenant itself is allowed through the gate.
    let md_repo = Arc::new(FakeMetadataRepo::new());
    let tenants = Arc::new(FakeTenantRepo::new());
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::Inherit,
    )]));
    let tid = Uuid::from_u128(0x100);
    seed_tenant(&tenants, tid, None, TenantStatus::Suspended, false, "susp");
    seed_metadata_row(
        &md_repo,
        tid,
        &schema_a(),
        json!({"theme": "own"}),
        fixed_now(),
    )
    .await;

    let svc = make_service(md_repo, tenants, registry, fixed_now());

    let entry = svc
        .resolve_metadata(&ctx(), tid, schema_a())
        .await
        .expect("resolve_metadata MUST accept Suspended start tenant per FEATURE spec")
        .expect("own row present");
    assert_eq!(entry.value, json!({"theme": "own"}));
}
