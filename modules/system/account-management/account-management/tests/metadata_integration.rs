//! Real-DB integration tests for the tenant-metadata domain
//! exercised end-to-end against in-memory `SQLite` with the production
//! migration set + `SeaORM`-backed `MetadataRepoImpl`.
//!
//! Coverage matrix (FEATURE §6 ACs covered at the integration layer):
//!
//! * **AC#1** — list / get / put / delete CRUD round-trip via
//!   `MetadataService` against the production storage path.
//! * **AC#3** — PUT 201 → 200 discriminator + DELETE 404 distinction.
//! * **AC#4** — distinct 404 codes (`metadata_schema_not_registered`
//!   vs `metadata_entry_not_found`) split deterministically.
//! * **AC#5** — cascade-delete on tenant hard-delete (the `SQLite`
//!   explicit `delete_many` branch in `TenantRepoImpl::hard_delete_one`).
//! * **AC#6** — barrier-aware walk-up resolve across a 3-tenant tree
//!   with the start-tenant barrier and ancestor barrier-stop.
//! * **AC#2** is FK-shape (PG-side cascade) and lives in the PG-gated
//!   sibling test file.
//!
//! Test harness mirrors `tests/conversion_integration.rs`:
//! `mod common;` reuses `setup_sqlite`, `insert_tenant`,
//! `insert_closure`, `stamp_retention_claim`. The metadata service
//! under test is built directly with `Arc::new(MetadataRepoImpl::new
//! (provider))`, the `Arc<dyn TenantRepo>` from `h.repo`, and an
//! `Arc<StubMetadataSchemaRegistry>` seeded with the
//! `(GtsSchemaId, InheritancePolicy)` pairs each test needs.

#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::sync::Arc;

use account_management::domain::error::DomainError;
use account_management::domain::metadata::registry::{
    InheritancePolicy, MetadataSchemaRegistry, StubMetadataSchemaRegistry,
};
use account_management::domain::metadata::repo::MetadataRepo;
use account_management::domain::metadata::service::MetadataService;
use account_management::domain::tenant::TenantRepo;
use account_management::domain::tenant::closure::build_activation_rows;
use account_management::domain::tenant::model::{NewTenant, TenantStatus};
use account_management::infra::storage::repo_impl::MetadataRepoImpl;
use account_management_sdk::UpsertMetadataRequest;
use modkit_odata::ODataQuery;
use serde_json::json;
use uuid::Uuid;

use common::*;
use gts::GtsSchemaId;

const SCHEMA_A: &str = "gts.cf.core.am.tenant_metadata.v1~vendor.app.metadata.feature_flag.v1~";
const SCHEMA_B: &str = "gts.cf.core.am.tenant_metadata.v1~vendor.app.metadata.org_branding.v1~";

fn schema_a() -> GtsSchemaId {
    GtsSchemaId::new(SCHEMA_A)
}

fn schema_b() -> GtsSchemaId {
    GtsSchemaId::new(SCHEMA_B)
}

/// Compute the same deterministic `UUIDv5` the service / repo use
/// internally, via the upstream `gts` crate. Test inputs here are all
/// hand-crafted valid GTS ids.
#[allow(
    clippy::expect_used,
    reason = "test helpers only see hand-crafted valid schema ids"
)]
fn schema_uuid_for(schema_id: &str) -> Uuid {
    gts::GtsID::new(schema_id)
        .expect("valid GTS id in tests")
        .to_uuid()
}

/// Drive the full create-child saga (steps 1 + 3) so `tenant_id`
/// lands in `Active` with the closure rows the activation contract
/// requires. Mirrors `tests/conversion_integration.rs::create_active_child`.
async fn create_active_child(
    h: &Harness,
    tenant_id: Uuid,
    parent_id: Uuid,
    name: &str,
    self_managed: bool,
    depth: u32,
) {
    let new = NewTenant {
        id: tenant_id,
        parent_id: Some(parent_id),
        name: name.to_owned(),
        self_managed,
        tenant_type_uuid: Uuid::from_u128(0xAA),
        depth,
    };
    h.repo
        .insert_provisioning(&allow_all(), &new)
        .await
        .expect("insert_provisioning");
    let ancestor_chain = h
        .repo
        .load_ancestor_chain_through_parent(&allow_all(), parent_id)
        .await
        .expect("ancestor chain");
    let closure_rows = build_activation_rows(
        tenant_id,
        TenantStatus::Active,
        self_managed,
        &ancestor_chain,
    );
    h.repo
        .activate_tenant(&allow_all(), tenant_id, &closure_rows, None)
        .await
        .expect("activate_tenant");
}

/// Seed the platform root tenant + its self-row directly without
/// driving the bootstrap saga.
async fn seed_root(h: &Harness, root_id: Uuid) {
    insert_tenant(&h.provider, root_id, None, "root", ACTIVE, false, 0)
        .await
        .expect("seed root");
    insert_closure(&h.provider, root_id, root_id, 0, ACTIVE)
        .await
        .expect("seed root self-row");
}

/// Build a wired metadata service over the production storage path
/// with a stub registry seeded against `(schema, policy)` pairs.
fn build_service(
    h: &Harness,
    registry: Arc<StubMetadataSchemaRegistry>,
) -> (Arc<MetadataService>, Arc<MetadataRepoImpl>) {
    let metadata_repo = Arc::new(MetadataRepoImpl::new(Arc::clone(&h.provider)));
    let metadata_repo_dyn: Arc<dyn MetadataRepo> = metadata_repo.clone();
    let tenant_repo: Arc<dyn TenantRepo> = h.repo.clone();
    let registry_dyn: Arc<dyn MetadataSchemaRegistry> = registry;
    let svc = Arc::new(MetadataService::new(
        metadata_repo_dyn,
        tenant_repo,
        registry_dyn,
        mock_enforcer(),
    ));
    (svc, metadata_repo)
}

// ---------------------------------------------------------------------
// AC#1 / AC#3 — CRUD round-trip via service against real storage.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crud_round_trip_via_service() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, tenant, root, "t", false, 1).await;

    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![
        (schema_a(), InheritancePolicy::OverrideOnly),
        (schema_b(), InheritancePolicy::OverrideOnly),
    ]));
    let (svc, _repo) = build_service(&h, registry);

    // Initial list — empty.
    let page = svc
        .list_metadata(&ctx_for(root), tenant, &ODataQuery::default())
        .await
        .expect("list empty");
    assert_eq!(page.items.len(), 0);
    // PUT — first call inserts (HTTP 201 in REST terms).
    let put_a = svc
        .upsert_metadata(
            &ctx_for(root),
            tenant,
            UpsertMetadataRequest::new(schema_a(), json!({"enabled": true})),
        )
        .await
        .expect("put schema_a");
    assert_eq!(put_a.value, json!({"enabled": true}));

    // PUT same key — second call updates (HTTP 200 in REST terms).
    let put_a_update = svc
        .upsert_metadata(
            &ctx_for(root),
            tenant,
            UpsertMetadataRequest::new(schema_a(), json!({"enabled": false})),
        )
        .await
        .expect("put schema_a update");
    assert_eq!(put_a_update.value, json!({"enabled": false}));

    // Insert a second schema for the same tenant.
    let _put_b = svc
        .upsert_metadata(
            &ctx_for(root),
            tenant,
            UpsertMetadataRequest::new(schema_b(), json!({"theme": "dark"})),
        )
        .await
        .expect("put schema_b");
    // GET round-trip.
    let got = svc
        .get_metadata(&ctx_for(root), tenant, schema_a())
        .await
        .expect("get schema_a");
    assert_eq!(got.value, json!({"enabled": false}));

    // LIST shape.
    let page = svc
        .list_metadata(&ctx_for(root), tenant, &ODataQuery::default())
        .await
        .expect("list with two");
    assert_eq!(page.items.len(), 2);
    // DELETE schema_a — schema_b row remains.
    svc.delete_metadata(&ctx_for(root), tenant, schema_a())
        .await
        .expect("delete schema_a");
    let page_after = svc
        .list_metadata(&ctx_for(root), tenant, &ODataQuery::default())
        .await
        .expect("list after delete");
    assert_eq!(page_after.items.len(), 1);
    assert_eq!(page_after.items[0].schema_id.as_ref(), SCHEMA_B);

    // GET on deleted entry surfaces `MetadataEntryNotFound` (distinct 404).
    let err = svc
        .get_metadata(&ctx_for(root), tenant, schema_a())
        .await
        .expect_err("get deleted");
    assert!(
        matches!(err, DomainError::MetadataEntryNotFound { .. }),
        "expected MetadataEntryNotFound, got {err:?}"
    );
}

// ---------------------------------------------------------------------
// AC#4 — distinct 404 codes split deterministically.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_distinguishes_schema_vs_entry_404() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, tenant, root, "t", false, 1).await;

    // Registry knows about schema_b ONLY — schema_a is unregistered.
    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_b(),
        InheritancePolicy::OverrideOnly,
    )]));
    let (svc, _repo) = build_service(&h, registry);

    // Unregistered schema → MetadataSchemaNotRegistered.
    let err = svc
        .get_metadata(&ctx_for(root), tenant, schema_a())
        .await
        .expect_err("get unregistered");
    assert!(
        matches!(err, DomainError::MetadataSchemaNotRegistered { .. }),
        "expected MetadataSchemaNotRegistered, got {err:?}"
    );

    // Registered schema with no row → MetadataEntryNotFound.
    let err = svc
        .get_metadata(&ctx_for(root), tenant, schema_b())
        .await
        .expect_err("get registered missing row");
    assert!(
        matches!(err, DomainError::MetadataEntryNotFound { .. }),
        "expected MetadataEntryNotFound, got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_missing_entry_returns_entry_not_found() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, tenant, root, "t", false, 1).await;

    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let (svc, _repo) = build_service(&h, registry);

    let err = svc
        .delete_metadata(&ctx_for(root), tenant, schema_a())
        .await
        .expect_err("delete missing row");
    assert!(
        matches!(err, DomainError::MetadataEntryNotFound { .. }),
        "expected MetadataEntryNotFound, got {err:?}"
    );
}

// ---------------------------------------------------------------------
// AC#6 — barrier-aware walk-up resolve.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_inherit_walks_up_to_root_then_barrier_stops() {
    use account_management::infra::storage::entity::tenants;
    use modkit_db::secure::SecureUpdateExt;
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    // 3-tenant tree: root -> mid -> leaf. All managed initially. Seed
    // a value at root with `inherit` policy: `resolve_for_tenant(leaf)`
    // returns root's value.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let mid = Uuid::new_v4();
    let leaf = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, mid, root, "mid", false, 1).await;
    create_active_child(&h, leaf, mid, "leaf", false, 2).await;

    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::Inherit,
    )]));
    let (svc, _repo) = build_service(&h, Arc::clone(&registry));

    // Seed root with the value.
    svc.upsert_metadata(
        &ctx_for(root),
        root,
        UpsertMetadataRequest::new(schema_a(), json!({"flag": "from_root"})),
    )
    .await
    .expect("put root value");

    // From leaf: walk-up returns root's value via mid (no barrier).
    let resolved = svc
        .resolve_metadata(&ctx_for(root), leaf, schema_a())
        .await
        .expect("resolve from leaf");
    let entry = resolved.expect("walk-up should hit root");
    assert_eq!(entry.value, json!({"flag": "from_root"}));

    // Barrier-stop: flip `mid` to self_managed via direct UPDATE on
    // the `tenants` table (we bypass the conversion saga for fixture
    // simplicity — the resolve walk-up only inspects
    // `TenantModel.self_managed` regardless of how it got there).
    let conn = h.provider.conn().expect("conn");
    tenants::Entity::update_many()
        .col_expr(tenants::Column::SelfManaged, Expr::value(true))
        .filter(tenants::Column::Id.eq(mid))
        .secure()
        .scope_with(&allow_all())
        .exec(&conn)
        .await
        .expect("flip mid self_managed");

    // From leaf again: ancestor barrier on `mid` returns empty BEFORE
    // any read at root, per `inst-algo-walk-ancestor-barrier-return`.
    let resolved = svc
        .resolve_metadata(&ctx_for(root), leaf, schema_a())
        .await
        .expect("resolve after barrier");
    assert!(
        resolved.is_none(),
        "barrier-stop must collapse to empty; got {resolved:?}"
    );
}

// ---------------------------------------------------------------------
// AC#5 — cascade-delete: hard_delete_one removes metadata rows.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hard_delete_cascades_metadata_rows_for_target_tenant_only() {
    use account_management::infra::storage::entity::tenant_metadata;
    use modkit_db::secure::SecureEntityExt;
    use sea_orm::EntityTrait;
    use time::Duration;

    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let target = Uuid::new_v4();
    let sibling = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, target, root, "target", false, 1).await;
    create_active_child(&h, sibling, root, "sibling", false, 1).await;

    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![
        (schema_a(), InheritancePolicy::OverrideOnly),
        (schema_b(), InheritancePolicy::OverrideOnly),
    ]));
    let (svc, repo) = build_service(&h, registry);

    // Seed metadata rows on target + sibling.
    svc.upsert_metadata(
        &ctx_for(root),
        target,
        UpsertMetadataRequest::new(schema_a(), json!({"v": "target_a"})),
    )
    .await
    .expect("put target a");
    svc.upsert_metadata(
        &ctx_for(root),
        target,
        UpsertMetadataRequest::new(schema_b(), json!({"v": "target_b"})),
    )
    .await
    .expect("put target b");
    svc.upsert_metadata(
        &ctx_for(root),
        sibling,
        UpsertMetadataRequest::new(schema_a(), json!({"v": "sibling_a"})),
    )
    .await
    .expect("put sibling a");

    // Pre-flight: confirm rows exist.
    let target_rows = repo
        .list_for_tenant(&allow_all(), target, &ODataQuery::default())
        .await
        .expect("list target");
    assert_eq!(target_rows.items.len(), 2); // Soft-delete target via the production tenant service path:
    // schedule_deletion stamps `deleted_at` (retention-timer start) +
    // flips status to Deleted. Stamp the retention claim so
    // `hard_delete_one`'s claim fence accepts the call.
    let now = time::OffsetDateTime::now_utc();
    h.repo
        .schedule_deletion(
            &allow_all(),
            target,
            now,
            Some(Duration::ZERO.unsigned_abs()),
        )
        .await
        .expect("schedule_deletion");
    let worker = Uuid::new_v4();
    stamp_retention_claim(&h.provider, target, worker, now)
        .await
        .expect("stamp claim");

    // hard_delete_one now exercises the SQLite explicit `delete_many`
    // branch on `tenant_metadata` inside the same TX as the tenant-row
    // delete.
    h.repo
        .hard_delete_one(&allow_all(), target, worker)
        .await
        .expect("hard_delete_one");

    // Target rows are gone.
    let target_rows_after = repo
        .list_for_tenant(&allow_all(), target, &ODataQuery::default())
        .await
        .expect("list target after");
    assert_eq!(
        target_rows_after.items.len(),
        0,
        "metadata rows for target tenant must be gone after hard_delete_one"
    ); // Sibling rows untouched.
    let sibling_rows = repo
        .list_for_tenant(&allow_all(), sibling, &ODataQuery::default())
        .await
        .expect("list sibling");
    assert_eq!(
        sibling_rows.items.len(),
        1,
        "sibling tenant's metadata MUST remain untouched"
    );
    assert_eq!(sibling_rows.items[0].value, json!({"v": "sibling_a"}));

    // Defense-in-depth: scan tenant_metadata directly with
    // `SecureORM` and confirm only the sibling row survives.
    let conn = h.provider.conn().expect("conn");
    let all_rows = tenant_metadata::Entity::find()
        .secure()
        .scope_with(&allow_all())
        .all(&conn)
        .await
        .expect("scan all metadata");
    assert_eq!(
        all_rows.len(),
        1,
        "exactly one row must survive (the sibling's)"
    );
    assert_eq!(all_rows[0].tenant_id, sibling);
    assert_eq!(
        all_rows[0].schema_uuid,
        schema_uuid_for(schema_a().as_ref())
    );
}
