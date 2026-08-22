#![cfg(feature = "integration")]
// Created: 2026-08-14 by Constructor Tech
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::too_many_lines
)]
//! PostgreSQL smoke suite for the resource-group DB-behavior audit's
//! set-based SQL.
//!
//! Three statement shapes are pinned by the audit and each has a property
//! that only a real `PostgreSQL` can confirm or deny -- `SQLite` accepts all
//! three regardless of whether they are actually correct SQL on a backend
//! with real query planning and constraint enforcement:
//!
//! - The `INSERT ... SELECT` cross product `insert_ancestor_closure_rows`
//!   and `rebuild_subtree_closure` issue for the closure table. Both
//!   `PostgreSQL` and `SQLite` permit a `SELECT` that names the table being
//!   written; `MySQL` does not, which is exactly the kind of divergence a
//!   `SQLite`-only suite cannot see.
//! - `rebuild_subtree_closure`'s closure-row `DELETE` wraps its subquery in
//!   a derived table so `MySQL` accepts it (see the migration history on
//!   this branch) -- worth confirming the same statement still runs
//!   correctly on `PostgreSQL`, not just that it parses.
//! - The chunked deletes (`delete_by_id_many`, and the removed-parent sweep
//!   in `type_repo.rs`) bind one parameter per id per chunk; `SQLite`'s bind
//!   ceiling and `PostgreSQL`'s are different numbers, so this is also where
//!   the chunking math first runs against the ceiling it was actually
//!   written for.
//!
//! `tests/db_behavior_audit_test.rs` already covers statement *counts* for
//! these same operations against `SQLite`, backend-agnostically; this suite
//! does not repeat that. It is narrow on purpose -- a smoke test for "does
//! this SQL actually execute here", not a second copy of the count
//! assertions or of `pg_concurrency_test.rs`'s concurrent races (that file
//! lives on `rg/db-behavior-audit-and-fix`, not this branch).
//!
//! [`PgFixture`], the Docker-availability handling, and
//! `RG_PG_REQUIRE_DOCKER` mirror `pg_concurrency_test.rs` on
//! `rg/db-behavior-audit-and-fix` -- see that file for the fuller rationale
//! (container lifetime via an owned `Drop`, why skipping is right locally
//! but wrong in CI).
//!
//! Requires the `integration` feature. Run via `make test-rg-pg` or:
//!
//! ```sh
//! cargo nextest run -p cf-gears-resource-group --features integration --test pg_smoke_test
//! ```

mod common;

use std::sync::Arc;

use resource_group::domain::group_service::GroupService;
use resource_group::domain::type_service::TypeService;
use resource_group::infra::storage::entity::gts_type::{
    Column as GtsTypeColumn, Entity as GtsTypeEntity,
};
use resource_group::infra::storage::entity::resource_group::{
    Column as RgColumn, Entity as RgEntity,
};
use resource_group::infra::storage::entity::resource_group_closure::{
    Column as ClosureColumn, Entity as ClosureEntity,
};
use resource_group::infra::storage::entity::resource_group_membership::{
    self as membership_entity, Column as MembershipColumn, Entity as MembershipEntity,
};
use resource_group::infra::storage::group_repo::GroupRepository;
use resource_group::infra::storage::migrations::Migrator;
use resource_group::infra::storage::type_repo::TypeRepository;
use resource_group_sdk::{CreateTypeRequest, UpdateTypeRequest};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use sea_orm_migration::MigratorTrait;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use toolkit_db::secure::{SecureEntityExt, secure_insert};
use toolkit_db::{
    ConnectOpts, DBProvider, DbError, connect_db, migration_runner::run_migrations_for_testing,
};
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

/// A PostgreSQL container plus a `DBProvider` connected to it, both owned by
/// whichever test created them.
///
/// Owned rather than parked in a `static`, which never runs its destructor
/// at process exit and would leak the container every run -- see
/// `pg_concurrency_test.rs::PgFixture` on `rg/db-behavior-audit-and-fix`,
/// which this mirrors.
struct PgFixture {
    _container: testcontainers::ContainerAsync<Postgres>,
    db: Arc<DBProvider<DbError>>,
}

/// Whether a missing or broken Docker must fail the run rather than skip it.
/// CI sets `RG_PG_REQUIRE_DOCKER=1`; locally it is unset and skipping is fine.
fn require_docker() -> bool {
    std::env::var_os("RG_PG_REQUIRE_DOCKER").is_some_and(|v| v != "0" && !v.is_empty())
}

/// Bring up a `testcontainers` PostgreSQL and run resource-group's
/// migrations against it.
///
/// Returns `None` if Docker isn't reachable; callers treat that as a
/// graceful skip. Skipping is right locally, but wrong in CI, where a broken
/// daemon would otherwise pass vacuously -- `RG_PG_REQUIRE_DOCKER=1` panics.
async fn pg_fixture() -> Option<PgFixture> {
    // No local tag override: the workspace floor is now PG 18, well past the
    // PG13 that made gen_random_uuid() built-in (RG's migrations use it).
    let request = test_containers::postgres()
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "app");

    let container = match request.start().await {
        Ok(container) => container,
        Err(e) => {
            let msg = format!(
                "PostgreSQL smoke tests: could not start a PostgreSQL container via \
                 testcontainers ({e}). Install/start Docker to run these for real -- see \
                 this file's module docs."
            );
            assert!(!require_docker(), "{msg}");
            eprintln!("skipping -- {msg}");
            return None;
        }
    };
    let port = match container.get_host_port_ipv4(5432).await {
        Ok(port) => port,
        Err(e) => {
            let msg = format!(
                "PostgreSQL smoke tests: container started but its port could not be \
                 resolved ({e}). Is Docker healthy?"
            );
            assert!(!require_docker(), "{msg}");
            eprintln!("skipping -- {msg}");
            return None;
        }
    };

    let opts = ConnectOpts {
        max_conns: Some(5),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db(&format!("postgres://user:pass@127.0.0.1:{port}/app"), opts)
        .await
        .expect("connect to the testcontainers PostgreSQL");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run resource-group migrations against PostgreSQL");

    Some(PgFixture {
        _container: container,
        db: Arc::new(DBProvider::new(db)),
    })
}

/// Bring up the fixture, or return from the test if Docker isn't available.
///
/// Binds an owned [`PgFixture`]; keep it alive for the whole test body,
/// since dropping it removes the container.
macro_rules! pg_fixture_or_skip {
    () => {
        match pg_fixture().await {
            Some(fixture) => fixture,
            None => return,
        }
    };
}

/// A type that can root itself and allows itself as a parent -- lets a
/// single type build chains/trees of arbitrary depth. Mirrors
/// `db_behavior_audit_test.rs`'s helper of the same name.
async fn create_self_referencing_type(
    type_svc: &TypeService<TypeRepository>,
    suffix: &str,
) -> resource_group_sdk::ResourceGroupType {
    // `resolve_ids` rejects a parent path that doesn't exist yet, so a type
    // can't reference itself as an allowed parent at create time; create it
    // plain, then update it to add the self-reference.
    let code = format!(
        "{}x.test.{}.i{}.v1~",
        gts_id!("cf.core.rg.type.v1~"),
        suffix.to_ascii_lowercase(),
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create self-referencing type (initial)");
    type_svc
        .update_type(
            &code,
            UpdateTypeRequest {
                can_be_root: true,
                allowed_parent_types: vec![code.clone()],
                allowed_membership_types: vec![],
                metadata_schema: None,
            },
        )
        .await
        .expect("update type to add self-reference")
}

/// Build a chain of `depth` nodes (root + `depth - 1` single children),
/// returning the id of the last (deepest) node. Mirrors
/// `db_behavior_audit_test.rs`'s helper of the same name.
async fn build_chain(
    group_svc: &GroupService<GroupRepository, TypeRepository>,
    ctx: &SecurityContext,
    type_code: &str,
    tenant_id: Uuid,
    depth: usize,
) -> Uuid {
    assert!(depth >= 1, "chain must have at least one node");
    let root = common::create_root_group(group_svc, ctx, type_code, "n0", tenant_id).await;
    let mut current = root.id;
    for i in 1..depth {
        let child = common::create_child_group(
            group_svc,
            ctx,
            type_code,
            current,
            &format!("n{i}"),
            tenant_id,
        )
        .await;
        current = child.id;
    }
    current
}

/// RG-06/RG-04 on real PostgreSQL: a subtree grafted under the leaf of an
/// already-deep chain, exercising both `insert_ancestor_closure_rows`'
/// `INSERT ... SELECT` (building the chain) and `rebuild_subtree_closure`'s
/// `A x N` cross-product `INSERT ... SELECT` plus its derived-table `DELETE`
/// (the move itself), then checking every depth the rebuild touched.
#[tokio::test(flavor = "multi_thread")]
async fn pg_move_under_deep_parent_rebuilds_every_depth() {
    let fixture = pg_fixture_or_skip!();
    let db = fixture.db.clone();
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let t = create_self_referencing_type(&type_svc, "pgmove").await;

    // A 5-deep chain: insert_ancestor_closure_rows's INSERT ... SELECT runs
    // once per create, copying a growing ancestor set each time.
    let deepest = build_chain(&group_svc, &ctx, &t.code, tenant_id, 5).await;

    // A small subtree elsewhere, to be grafted under the chain's leaf --
    // this is what exercises rebuild_subtree_closure's cross product against
    // a destination that already has ancestors of its own.
    let other_root =
        common::create_root_group(&group_svc, &ctx, &t.code, "other-root", tenant_id).await;
    let subtree_root = common::create_child_group(
        &group_svc,
        &ctx,
        &t.code,
        other_root.id,
        "sub-root",
        tenant_id,
    )
    .await;
    for i in 0..3 {
        common::create_child_group(
            &group_svc,
            &ctx,
            &t.code,
            subtree_root.id,
            &format!("leaf{i}"),
            tenant_id,
        )
        .await;
    }

    group_svc
        .move_group(subtree_root.id, Some(deepest))
        .await
        .expect("move subtree under the deepest chain node");

    // The strongest available check: every closure row agrees with
    // parent_id, at every depth, for every group in the database -- not
    // just the moved subtree.
    let conn = db.conn().expect("conn");
    common::assert_closure_matches_parent_links(&conn).await;
}

/// RG-10 on real PostgreSQL: `delete_by_id_many`'s chunked delete plus the
/// membership cascade, run against a tree deep enough that the closure
/// table's derived-table `DELETE` and the group/membership deletes all fire
/// in the same force-delete transaction.
#[tokio::test(flavor = "multi_thread")]
async fn pg_force_delete_leaves_no_orphans() {
    let fixture = pg_fixture_or_skip!();
    let db = fixture.db.clone();
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "pgforce").await;
    let child_type =
        common::create_child_type(&type_svc, "pgforcechild", &[&root_type.code], &[]).await;
    let grandchild_type =
        common::create_child_type(&type_svc, "pgforcegrandchild", &[&child_type.code], &[]).await;

    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "root", tenant_id).await;
    let child = common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        root.id,
        "child",
        tenant_id,
    )
    .await;
    let grandchild = common::create_child_group(
        &group_svc,
        &ctx,
        &grandchild_type.code,
        child.id,
        "grandchild",
        tenant_id,
    )
    .await;

    // A direct membership insert (mirrors group_service_test.rs's
    // group_force_delete_subtree): resolve the real surrogate gts_type_id
    // instead of hard-coding one.
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let root_type_id = GtsTypeEntity::find()
        .filter(GtsTypeColumn::SchemaId.eq(&root_type.code))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query gts_type")
        .expect("type row exists")
        .id;
    let membership = membership_entity::ActiveModel {
        group_id: Set(child.id),
        gts_type_id: Set(root_type_id),
        resource_id: Set("pg-smoke-resource".to_owned()),
        ..Default::default()
    };
    secure_insert::<MembershipEntity>(membership, &scope, &conn)
        .await
        .expect("insert membership");

    group_svc
        .delete_group(&ctx, root.id, true)
        .await
        .expect("force delete");

    for gid in [root.id, child.id, grandchild.id] {
        let group_row = RgEntity::find()
            .filter(RgColumn::Id.eq(gid))
            .secure()
            .scope_with(&scope)
            .one(&conn)
            .await
            .expect("query resource_group");
        assert!(group_row.is_none(), "group {gid} should be deleted");

        let ancestor_rows = ClosureEntity::find()
            .filter(ClosureColumn::AncestorId.eq(gid))
            .secure()
            .scope_with(&scope)
            .count(&conn)
            .await
            .expect("query resource_group_closure (ancestor side)");
        assert_eq!(
            ancestor_rows, 0,
            "closure rows with {gid} as ancestor should be gone"
        );

        let descendant_rows = ClosureEntity::find()
            .filter(ClosureColumn::DescendantId.eq(gid))
            .secure()
            .scope_with(&scope)
            .count(&conn)
            .await
            .expect("query resource_group_closure (descendant side)");
        assert_eq!(
            descendant_rows, 0,
            "closure rows with {gid} as descendant should be gone"
        );
    }

    let mem_count = MembershipEntity::find()
        .filter(MembershipColumn::GroupId.eq(child.id))
        .secure()
        .scope_with(&scope)
        .count(&conn)
        .await
        .expect("query resource_group_membership");
    assert_eq!(
        mem_count, 0,
        "memberships for the deleted group should be gone"
    );
}

/// RG-06 on real PostgreSQL: a 4-deep create chain, then the full closure
/// invariant -- every `insert_ancestor_closure_rows` `INSERT ... SELECT` in
/// the chain must have written exactly the rows `parent_id` implies.
#[tokio::test(flavor = "multi_thread")]
async fn pg_create_chain_closure_invariant() {
    let fixture = pg_fixture_or_skip!();
    let db = fixture.db.clone();
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let t = create_self_referencing_type(&type_svc, "pgchain").await;
    build_chain(&group_svc, &ctx, &t.code, tenant_id, 4).await;

    let conn = db.conn().expect("conn");
    common::assert_closure_matches_parent_links(&conn).await;
}
