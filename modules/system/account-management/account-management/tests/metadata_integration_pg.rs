//! Real-Postgres integration tests for the tenant-metadata domain.
//!
//! Mirrors the SQLite-backed `metadata_integration.rs` happy-path
//! exercise, then adds the PG-only assertion that the FK
//! `ON DELETE CASCADE` on `tenant_metadata.tenant_id → tenants.id`
//! actually fires when the tenant row is dropped via raw SQL,
//! independent of the AM-side explicit `delete_many` branch in
//! `TenantRepoImpl::hard_delete_one`. This is the only way to confirm
//! the dialect-split contract pinned by FEATURE §1.2 / `DoD`
//! `dod-tenant-metadata-cascade-delete` end-to-end on the production
//! engine.
//!
//! Gated behind `#[cfg(feature = "postgres")]` so the default
//! `cargo test` run does not require Docker. Enable explicitly:
//! `cargo test -p cf-account-management --features postgres
//!  --test metadata_integration_pg`.

#![cfg(feature = "postgres")]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::sync::Arc;

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
use gts::GtsSchemaId;
use modkit_odata::ODataQuery;
use sea_orm::ConnectionTrait;
use serde_json::json;
use uuid::Uuid;

use common::pg::bring_up_postgres;
use common::*;

const SCHEMA_A: &str = "gts.cf.core.am.tenant_metadata.v1~vendor.app.metadata.feature_flag.v1~";
const SCHEMA_B: &str = "gts.cf.core.am.tenant_metadata.v1~vendor.app.metadata.org_branding.v1~";

fn schema_a() -> GtsSchemaId {
    GtsSchemaId::new(SCHEMA_A)
}

fn schema_b() -> GtsSchemaId {
    GtsSchemaId::new(SCHEMA_B)
}

async fn create_active_child(
    h: &common::pg::PgHarness,
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

async fn seed_root(h: &common::pg::PgHarness, root_id: Uuid) {
    insert_tenant(&h.provider, root_id, None, "root", ACTIVE, false, 0)
        .await
        .expect("seed root");
    insert_closure(&h.provider, root_id, root_id, 0, ACTIVE)
        .await
        .expect("seed root self-row");
}

fn build_service(
    h: &common::pg::PgHarness,
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
// PG-only: FK ON DELETE CASCADE on tenant_metadata.tenant_id.
// ---------------------------------------------------------------------
//
// Drives a raw `DELETE FROM tenants` (bypassing
// `TenantRepoImpl::hard_delete_one` and its explicit `delete_many`
// branch) and asserts that the FK clause cascade-removes every
// `tenant_metadata` row for the dropped tenant. This is the production
// path on Postgres — `hard_delete_one`'s explicit `delete_many` is a
// belt-and-suspenders cleanup that's authoritative on SQLite and a
// no-op on Postgres (the FK fires first).

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_fk_cascade_removes_metadata_when_tenant_row_dropped() {
    let h = bring_up_postgres()
        .await
        .expect("postgres testcontainer (Docker daemon required)");
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

    let target_rows = repo
        .list_for_tenant(&allow_all(), target, &ODataQuery::default())
        .await
        .expect("list target");
    assert_eq!(
        target_rows.items.len(),
        2,
        "two metadata rows seeded for target"
    );

    // Drop the tenant row directly via the auxiliary DDL connection.
    // We have to drop closure rows first (a separate FK references
    // `tenants.id` from `tenant_closure`), then the tenant row itself.
    // The metadata FK is `ON DELETE CASCADE` so dropping the tenant
    // row is the trigger we are testing here.
    let raw = format!(
        "DELETE FROM tenant_closure WHERE ancestor_id = '{target}' OR descendant_id = '{target}';"
    );
    h.ddl_conn
        .execute_unprepared(&raw)
        .await
        .expect("delete closure rows for target");
    let raw = format!("DELETE FROM tenants WHERE id = '{target}';");
    h.ddl_conn
        .execute_unprepared(&raw)
        .await
        .expect("delete tenant row");

    // Cascade assertion: every metadata row for `target` is gone.
    let target_rows_after = repo
        .list_for_tenant(&allow_all(), target, &ODataQuery::default())
        .await
        .expect("list target after");
    assert_eq!(
        target_rows_after.items.len(),
        0,
        "FK ON DELETE CASCADE must remove every tenant_metadata row \
         for the dropped tenant"
    );

    // Sibling rows untouched.
    let sibling_rows = repo
        .list_for_tenant(&allow_all(), sibling, &ODataQuery::default())
        .await
        .expect("list sibling");
    assert_eq!(
        sibling_rows.items.len(),
        1,
        "sibling tenant's metadata MUST remain untouched"
    );
}

// ---------------------------------------------------------------------
// PG-only: hard_delete_one path remains correct on Postgres too.
// ---------------------------------------------------------------------
//
// On Postgres the FK CASCADE may fire before the explicit
// `delete_many` against `tenant_metadata`. The brief calls the
// explicit delete "defensive even though FK ON DELETE CASCADE handles
// it". Confirm the AM hard-delete path stays consistent end-to-end on
// Postgres — no errors from a no-op `delete_many`, all metadata rows
// gone after the path completes.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_hard_delete_one_clears_metadata_via_combined_path() {
    use time::Duration;

    let h = bring_up_postgres()
        .await
        .expect("postgres testcontainer (Docker daemon required)");
    let root = Uuid::new_v4();
    let target = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, target, root, "target", false, 1).await;

    let registry = Arc::new(StubMetadataSchemaRegistry::with_seed(vec![(
        schema_a(),
        InheritancePolicy::OverrideOnly,
    )]));
    let (svc, repo) = build_service(&h, registry);

    svc.upsert_metadata(
        &ctx_for(root),
        target,
        UpsertMetadataRequest::new(schema_a(), json!({"v": "target_a"})),
    )
    .await
    .expect("put target a");

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

    h.repo
        .hard_delete_one(&allow_all(), target, worker)
        .await
        .expect("hard_delete_one");

    let after = repo
        .list_for_tenant(&allow_all(), target, &ODataQuery::default())
        .await
        .expect("list target after");
    assert_eq!(after.items.len(), 0);
}
