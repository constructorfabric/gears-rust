#![cfg(feature = "integration")]
// Created: 2026-08-19 by Constructor Tech
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::too_many_lines
)]
//! PostgreSQL concurrency tests for the resource-group gear.
//!
//! These tests spin up a real PostgreSQL via `testcontainers` and drive
//! concurrent operations through the real service+repository stack to verify
//! that the chosen isolation levels (or the constraint fallbacks) hold
//! the documented invariants under concurrent writes.
//!
//! Requires the `integration` feature and a working Docker daemon. Run via:
//!
//! ```sh
//! cargo nextest run -p cf-gears-resource-group --features integration \
//!   --test pg_concurrency_test
//! ```
//!
//! ## Scenarios
//!
//! | # | Operation A | Operation B | Checks |
//! |---|------------|------------|--------|
//! | 1 | move A→B   | move B→A   | no cycle committed |
//! | 2 | create child | move parent | closure consistent |
//! | 4 | non-force delete | create child | FK blocks create |
//! | 6 | `add_membership` (tenant A) | `add_membership` (tenant B), same resource | exactly one commits, the other gets a clean `TenantIncompatibility` -- not a bare DB error |
//! | 7 | `remove_membership` x2, same resource | | the resource is free for another tenant once its last membership is gone |
//! | 8 | tenant check + membership insert, forced overlap at `SERIALIZABLE` | | SSI cancels one side; the resource ends up owned by one tenant |
//! | 9 | same forced overlap at the backend default | | negative control: both commit and the resource ends up owned by two tenants |

mod common;

use std::sync::Arc;

use common::{CreateTypeRequest, UpdateTypeRequest};
use resource_group::domain::error::DomainError;
use resource_group::domain::group_service::GroupService;
use resource_group::domain::repo::{GroupRepositoryTrait, MembershipRepositoryTrait};
use resource_group::infra::storage::entity::resource_group_membership::{
    self as membership_entity, Entity as MembershipEntity,
};
use resource_group::infra::storage::group_repo::GroupRepository;
use resource_group::infra::storage::membership_repo::MembershipRepository;
use resource_group::infra::storage::migrations::Migrator;
use resource_group_sdk::models::CreateGroupRequest;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use sea_orm_migration::MigratorTrait;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use toolkit_db::secure::{SecureEntityExt, TxConfig};
use toolkit_db::{
    ConnectOpts, DBProvider, DbError, connect_db, migration_runner::run_migrations_for_testing,
};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

// ── Fixture ──────────────────────────────────────────────────────────

/// A PostgreSQL container plus a `DBProvider` connected to it.
struct PgFixture {
    _container: testcontainers::ContainerAsync<Postgres>,
    db: Arc<DBProvider<DbError>>,
}

fn require_docker() -> bool {
    std::env::var_os("RG_PG_REQUIRE_DOCKER").is_some_and(|v| v != "0" && !v.is_empty())
}

async fn pg_fixture() -> Option<PgFixture> {
    let request = test_containers::postgres()
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "app");

    let container = match request.start().await {
        Ok(c) => c,
        Err(e) => {
            assert!(
                !require_docker(),
                "Docker required (RG_PG_REQUIRE_DOCKER=1) but container failed: {e}"
            );
            eprintln!("pg_concurrency_test: skipping (Docker unavailable: {e})");
            return None;
        }
    };

    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("get PostgreSQL port");

    let opts = ConnectOpts {
        max_conns: Some(10),
        min_conns: Some(2),
        ..Default::default()
    };

    let dsn = format!("postgres://user:pass@127.0.0.1:{port}/app");
    let db = connect_db(&dsn, opts)
        .await
        .expect("connect to test PostgreSQL");

    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrations");

    Some(PgFixture {
        _container: container,
        db: Arc::new(DBProvider::new(db)),
    })
}

// ── Helpers ──────────────────────────────────────────────────────────

fn make_ctx(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant_id)
        .build()
        .expect("valid SecurityContext")
}

fn type_code(suffix: &str) -> String {
    format!(
        "{}x.test.{}.i{}.v1~",
        toolkit_gts::gts_id!("cf.core.rg.group.v1~"),
        suffix,
        Uuid::now_v7().as_simple()
    )
}

fn make_services(
    db: Arc<DBProvider<DbError>>,
) -> (common::TestTypes, GroupService<GroupRepository>) {
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db);
    (type_svc, group_svc)
}

/// Render a fallible operation's outcome for a diagnostic log line without
/// leaning on `Debug` formatting -- `Ok`/`Err(<Display>)` is enough to see
/// which side of a race lost without dumping the whole model.
fn fmt_outcome<T>(label: &str, r: &Result<T, DomainError>) -> String {
    match r {
        Ok(_) => format!("{label}=Ok"),
        Err(e) => format!("{label}=Err({e})"),
    }
}

// -----------------------------------------------------------------------
// 1. concurrent move A→B vs B→A
// -----------------------------------------------------------------------
#[tokio::test]
async fn concurrent_move_a_to_b_and_b_to_a() {
    let Some(fix) = pg_fixture().await else {
        return;
    };

    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);
    let (type_svc, group_svc) = make_services(fix.db.clone());

    let rt = type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: type_code("mvab"),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create type");

    // A and B below are both of this type and need to become each other's
    // parent, so the type must allow itself as a parent. It cannot list
    // itself at creation (the row does not exist yet for `resolve_ids` to
    // find), so this is a follow-up update against the now-existing row.
    let rt = type_svc
        .update_type_unscoped(
            &rt.code,
            UpdateTypeRequest {
                can_be_root: rt.can_be_root,
                allowed_parent_types: vec![rt.code.clone()],
                allowed_membership_types: rt.allowed_membership_types.clone(),
                metadata_schema: None,
            },
        )
        .await
        .expect("allow type to parent itself");

    let root = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(rt.code.clone(), "Root".to_owned()),
            tenant_id,
        )
        .await
        .expect("create root");

    let a = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(rt.code.clone(), "A".to_owned()).with_parent_id(Some(root.id)),
            tenant_id,
        )
        .await
        .expect("create A");

    let b = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(rt.code, "B".to_owned()).with_parent_id(Some(root.id)),
            tenant_id,
        )
        .await
        .expect("create B");

    // Move A→B and B→A concurrently.
    let (r1, r2) = tokio::join!(
        group_svc.move_group(&ctx, a.id, Some(b.id)),
        group_svc.move_group(&ctx, b.id, Some(a.id)),
    );

    match (&r1, &r2) {
        (Ok(_), Ok(_)) => panic!("both moves succeeded -- cycle committed!"),
        (Err(e1), Err(e2)) => {
            let m = format!("{e1}{e2}");
            assert!(
                m.contains("cycle") || m.contains("descendant"),
                "expected cycle/descendant, got: {e1} / {e2}"
            );
        }
        (Ok(_), Err(e)) | (Err(e), Ok(_)) => {
            let m = format!("{e}");
            assert!(
                m.contains("cycle") || m.contains("descendant") || m.contains("precondition"),
                "expected cycle/precondition, got: {e}"
            );
        }
    }
}

// -----------------------------------------------------------------------
// 2. concurrent create child vs move parent
// -----------------------------------------------------------------------
#[tokio::test]
async fn concurrent_create_child_and_move_parent() {
    let Some(fix) = pg_fixture().await else {
        return;
    };

    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);
    let (type_svc, group_svc) = make_services(fix.db.clone());

    let rt = type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: type_code("ccmp"),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create type");

    // `parent` and `child` below nest under groups of this same type, so the
    // type must allow itself as a parent. It cannot list itself at creation
    // (the row does not exist yet for `resolve_ids` to find), so this is a
    // follow-up update against the now-existing row.
    let rt = type_svc
        .update_type_unscoped(
            &rt.code,
            UpdateTypeRequest {
                can_be_root: rt.can_be_root,
                allowed_parent_types: vec![rt.code.clone()],
                allowed_membership_types: rt.allowed_membership_types.clone(),
                metadata_schema: None,
            },
        )
        .await
        .expect("allow type to parent itself");

    let root = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(rt.code.clone(), "Root".to_owned()),
            tenant_id,
        )
        .await
        .expect("create root");

    let other = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(rt.code.clone(), "Other".to_owned()),
            tenant_id,
        )
        .await
        .expect("create other root");

    let parent = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(rt.code.clone(), "Parent".to_owned())
                .with_parent_id(Some(root.id)),
            tenant_id,
        )
        .await
        .expect("create parent");

    let (child, _moved) = tokio::join!(
        group_svc.create_group(
            &ctx,
            CreateGroupRequest::new(rt.code.clone(), "Child".to_owned())
                .with_parent_id(Some(parent.id)),
            tenant_id
        ),
        group_svc.move_group(&ctx, parent.id, Some(other.id)),
    );

    child.expect("concurrent create child should succeed");
}

// -----------------------------------------------------------------------
// 4. non-force delete vs concurrent create child
// -----------------------------------------------------------------------
#[tokio::test]
async fn concurrent_non_force_delete_and_create_child() {
    let Some(fix) = pg_fixture().await else {
        return;
    };

    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);
    let (type_svc, group_svc) = make_services(fix.db.clone());

    let rt = type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: type_code("nfdel"),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create type");

    // `Orphan` below nests under `root`, a group of this same type, so the
    // type must allow itself as a parent. It cannot list itself at creation
    // (the row does not exist yet for `resolve_ids` to find), so this is a
    // follow-up update against the now-existing row.
    let rt = type_svc
        .update_type_unscoped(
            &rt.code,
            UpdateTypeRequest {
                can_be_root: rt.can_be_root,
                allowed_parent_types: vec![rt.code.clone()],
                allowed_membership_types: rt.allowed_membership_types.clone(),
                metadata_schema: None,
            },
        )
        .await
        .expect("allow type to parent itself");

    let root = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(rt.code.clone(), "Root".to_owned()),
            tenant_id,
        )
        .await
        .expect("create root");

    let (del_res, create_res) = tokio::join!(
        group_svc.delete_group(&ctx, root.id, false),
        group_svc.create_group(
            &ctx,
            CreateGroupRequest::new(rt.code, "Orphan".to_owned()).with_parent_id(Some(root.id)),
            tenant_id
        ),
    );

    // The FK/lock mechanism spelled out on `delete_group` resolves this race
    // one of exactly two ways, depending on which side's transaction commits
    // first:
    //
    // * the delete wins -> the child insert loses its parent to
    //   `fk_resource_group_parent` (`ON DELETE RESTRICT`), and
    //   `create_group_inner`/`map_insert_error` report that as
    //   `DomainError::GroupNotFound` on the *create* side;
    // * the create wins -> the delete's own "does this group have children"
    //   check (taken after its `FOR UPDATE` lock is granted) now sees the
    //   new child and refuses with `DomainError::ConflictActiveReferences` on
    //   the *delete* side.
    //
    // A concatenated `format!("{e1}{e2}")` check let either error's text
    // satisfy the assertion for both sides at once, so a delete error that
    // regressed into some unrelated shape could hide behind create's
    // "not found" text (or vice versa). Each side is checked against the one
    // answer its own code path can actually produce instead.
    match (del_res, create_res) {
        (Ok(()), Ok(_)) => {
            panic!("both non-force delete and create succeeded -- FK invariant broken!")
        }
        (Err(e1), Err(e2)) => {
            assert!(
                matches!(e1, DomainError::ConflictActiveReferences { .. }),
                "delete lost the race by seeing the new child -- expected \
                 ConflictActiveReferences, got: {e1}"
            );
            assert!(
                matches!(e2, DomainError::GroupNotFound { .. }),
                "create lost the race to the FK on the deleted parent -- expected \
                 GroupNotFound, got: {e2}"
            );
        }
        (Ok(()), Err(e)) => {
            assert!(
                matches!(e, DomainError::GroupNotFound { .. }),
                "delete won the race, so create must fail on the now-missing parent -- \
                 expected GroupNotFound, got: {e}"
            );
        }
        (Err(e), Ok(_)) => {
            assert!(
                matches!(e, DomainError::ConflictActiveReferences { .. }),
                "create won the race, so delete must see the new child -- expected \
                 ConflictActiveReferences, got: {e}"
            );
        }
    }
}

// -----------------------------------------------------------------------
// 6. add_membership from two tenants on the same resource
// -----------------------------------------------------------------------
#[tokio::test]
async fn concurrent_add_membership_from_two_tenants_claims_exactly_one() {
    let Some(fix) = pg_fixture().await else {
        return;
    };

    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let ctx_a = make_ctx(tenant_a);
    let ctx_b = make_ctx(tenant_b);
    let (type_svc, group_svc) = make_services(fix.db.clone());
    let membership_svc = common::make_membership_service(fix.db.clone());

    let member_type = type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: type_code("addmbr"),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create member type");

    let group_type = type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: type_code("addgrp"),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![member_type.code.clone()],
            metadata_schema: None,
        })
        .await
        .expect("create group type");

    let group_a = group_svc
        .create_group(
            &ctx_a,
            CreateGroupRequest::new(group_type.code.clone(), "A".to_owned()),
            tenant_a,
        )
        .await
        .expect("create group A");

    let group_b = group_svc
        .create_group(
            &ctx_b,
            CreateGroupRequest::new(group_type.code, "B".to_owned()),
            tenant_b,
        )
        .await
        .expect("create group B");

    let resource_id = "res-1".to_owned();

    // Both tenants race to be the first membership on the same resource,
    // through the real public `add_membership` -- not the raw repository
    // calls `run_first_membership_overlap` below drives, so there is no
    // injection point for a handshake that would force the two attempts
    // into the exact SSI-conflict window. `tokio::join!` gives actual
    // concurrency, not a guaranteed overlap: the two may interleave inside
    // their transactions, or run close enough to fully serialize, with the
    // second simply reading the first's already-committed row.
    //
    // The assertions below hold either way, which is the point of this
    // test: it is a black-box robustness check on the outcome
    // (`add_membership` never lets two tenants both succeed, and never
    // answers a genuine conflict with a bare database error), not a proof
    // that the write-skew abort itself fires. That proof is deterministic
    // and lives in `forced_overlap_at_serializable_keeps_one_tenant` below,
    // with `forced_overlap_at_read_committed_splits_the_resource` as its
    // negative control.
    let (r1, r2) = tokio::join!(
        membership_svc.add_membership(&ctx_a, group_a.id, &member_type.code, &resource_id),
        membership_svc.add_membership(&ctx_b, group_b.id, &member_type.code, &resource_id),
    );

    match (&r1, &r2) {
        (Ok(_), Ok(_)) => {
            panic!("both tenants claimed the same resource -- add_membership let them both succeed")
        }
        (Err(e1), Err(e2)) => panic!(
            "both attempts failed -- expected exactly one TenantIncompatibility, not a \
             regression to a bare database error under contention: {e1} / {e2}"
        ),
        (Ok(_), Err(e)) | (Err(e), Ok(_)) => {
            let m = format!("{e}");
            assert!(
                m.contains("already linked to a different tenant"),
                "expected a clean TenantIncompatibility naming neither tenant, got: {e}"
            );
        }
    }
}

// -----------------------------------------------------------------------
// 7. two concurrent remove_membership calls free the resource
// -----------------------------------------------------------------------
#[tokio::test]
async fn concurrent_removals_free_the_resource_for_another_tenant() {
    let Some(fix) = pg_fixture().await else {
        return;
    };

    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let ctx_a = make_ctx(tenant_a);
    let ctx_b = make_ctx(tenant_b);
    let (type_svc, group_svc) = make_services(fix.db.clone());
    let membership_svc = common::make_membership_service(fix.db.clone());

    let member_type = type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: type_code("relmbr"),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create member type");

    let group_type = type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: type_code("relgrp"),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![member_type.code.clone()],
            metadata_schema: None,
        })
        .await
        .expect("create group type");

    // Two groups under the same tenant, both linked to the same resource --
    // ownership is per tenant, not per group, so both adds succeed.
    let group_1 = group_svc
        .create_group(
            &ctx_a,
            CreateGroupRequest::new(group_type.code.clone(), "G1".to_owned()),
            tenant_a,
        )
        .await
        .expect("create group 1");

    let group_2 = group_svc
        .create_group(
            &ctx_a,
            CreateGroupRequest::new(group_type.code.clone(), "G2".to_owned()),
            tenant_a,
        )
        .await
        .expect("create group 2");

    let resource_id = "res-1".to_owned();

    membership_svc
        .add_membership(&ctx_a, group_1.id, &member_type.code, &resource_id)
        .await
        .expect("add membership 1");
    membership_svc
        .add_membership(&ctx_a, group_2.id, &member_type.code, &resource_id)
        .await
        .expect("add membership 2");

    // Remove both concurrently. Each removal is one DELETE by primary key
    // and decides nothing from a read, so there is no second piece of state
    // to fall out of step and no isolation level to get wrong -- which is
    // the point of deriving ownership from the membership rows themselves.
    let (r1, r2) = tokio::join!(
        membership_svc.remove_membership(&ctx_a, group_1.id, &member_type.code, &resource_id),
        membership_svc.remove_membership(&ctx_a, group_2.id, &member_type.code, &resource_id),
    );
    r1.expect("remove membership 1");
    r2.expect("remove membership 2");

    // With every membership gone, the resource belongs to nobody, and the
    // next tenant to link it becomes its owner.
    let group_b = group_svc
        .create_group(
            &ctx_b,
            CreateGroupRequest::new(group_type.code, "B".to_owned()),
            tenant_b,
        )
        .await
        .expect("create group B");
    membership_svc
        .add_membership(&ctx_b, group_b.id, &member_type.code, &resource_id)
        .await
        .expect(
            "a different tenant should be able to take the resource once every membership \
             on it has been removed",
        );
}

// -----------------------------------------------------------------------
// 8/9. tenant check + membership insert, forced into the overlap window
// -----------------------------------------------------------------------
// Scenario 6 races two full `add_membership` calls, but that interleaving is
// not guaranteed to land inside the window RG-01 is about: one side's check
// can just as easily run after the other has already committed, which is the
// uninteresting ordering. These two force the window with a handshake and
// differ only in the isolation level the two transactions open at -- which is
// the whole claim being tested, so the pair is written as one helper driven
// twice.
//
// The barrier is: A checks (sees nothing), B then checks (still sees
// nothing, A has not written yet) and commits its insert, and only then does
// A insert and commit. At `SERIALIZABLE` that is a cycle of
// rw-antidependencies and PostgreSQL cancels A as the pivot. At the backend
// default nothing objects and both commit -- the corruption this pairing
// exists to prevent, kept as a permanent negative control so the level
// cannot be quietly lowered.
struct OverlapOutcome {
    first: Result<(), DomainError>,
    second: Result<(), DomainError>,
    owning_tenants: Vec<Uuid>,
}

async fn run_first_membership_overlap(fix: &PgFixture, config: TxConfig) -> OverlapOutcome {
    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let ctx_a = make_ctx(tenant_a);
    let ctx_b = make_ctx(tenant_b);
    let (type_svc, group_svc) = make_services(fix.db.clone());

    let member_type = type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: type_code("ovlmbr"),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create member type");

    let group_type = type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: type_code("ovlgrp"),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![member_type.code.clone()],
            metadata_schema: None,
        })
        .await
        .expect("create group type");

    let mut group_ids = Vec::new();
    for (ctx, tenant, name) in [(&ctx_a, tenant_a, "A"), (&ctx_b, tenant_b, "B")] {
        let group = group_svc
            .create_group(
                ctx,
                CreateGroupRequest::new(group_type.code.clone(), name.to_owned()),
                tenant,
            )
            .await
            .expect("create group");
        group_ids.push(group.id);
    }
    let (group_a, group_b) = (group_ids[0], group_ids[1]);

    // The member type's dictionary key is taken up front: this race is about
    // the tenant check, not about the dictionary.
    let gts_type_id = member_type.code.clone();
    {
        let conn = fix.db.conn().expect("conn");
        GroupRepository
            .ensure_type_key(&conn, &gts_type_id)
            .await
            .expect("member type key");
    }

    let resource_id = "overlap-res".to_owned();
    let db = fix.db.db();

    let (a_checked_tx, a_checked_rx) = tokio::sync::oneshot::channel::<()>();
    let (b_done_tx, b_done_rx) = tokio::sync::oneshot::channel::<()>();

    let db_a = db.clone();
    let type_a = gts_type_id.clone();
    let resource_a = resource_id.clone();
    let config_a = config.clone();
    let first = tokio::spawn(async move {
        db_a.transaction_ref_mapped_with_config::<_, (), DomainError>(config_a, move |tx| {
            let resource_id = resource_a.clone();
            let gts_type_id = type_a.clone();
            // Ignored, not `expect`ed: at SERIALIZABLE the peer may already
            // have returned by the time this fires, and its receiver going
            // away is an outcome of the race, not a reason to panic.
            let a_checked_tx = a_checked_tx;
            let b_done_rx = b_done_rx;
            Box::pin(async move {
                let conflict = MembershipRepository
                    .has_membership_in_other_tenant(tx, &gts_type_id, &resource_id, tenant_a)
                    .await?;
                assert!(!conflict, "A must see an unclaimed resource");
                let _send = a_checked_tx.send(());
                let _recv = b_done_rx.await;
                MembershipRepository
                    .insert(tx, group_a, &gts_type_id, &resource_id)
                    .await?;
                Ok(())
            })
        })
        .await
    });

    a_checked_rx.await.expect("A must reach its check");

    let db_b = db.clone();
    let type_b = gts_type_id.clone();
    let resource_b = resource_id.clone();
    let config_b = config.clone();
    let second = tokio::spawn(async move {
        let out = db_b
            .transaction_ref_mapped_with_config::<_, (), DomainError>(config_b, move |tx| {
                let resource_id = resource_b.clone();
                let gts_type_id = type_b.clone();
                Box::pin(async move {
                    let conflict = MembershipRepository
                        .has_membership_in_other_tenant(tx, &gts_type_id, &resource_id, tenant_b)
                        .await?;
                    assert!(!conflict, "B must still see an unclaimed resource");
                    MembershipRepository
                        .insert(tx, group_b, &gts_type_id, &resource_id)
                        .await?;
                    Ok(())
                })
            })
            .await;
        let _send = b_done_tx.send(());
        out
    });

    let second_result = second.await.expect("B task");
    let first_result = first.await.expect("A task");

    // Which tenants ended up owning the resource. Read as rows, not as a
    // count: this asks *who*, and the answer is at most two groups.
    let conn = fix.db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let mut owning_tenants = Vec::new();
    for (group_id, tenant_id) in [(group_a, tenant_a), (group_b, tenant_b)] {
        let row = MembershipEntity::find()
            .filter(membership_entity::Column::GroupId.eq(group_id))
            .filter(membership_entity::Column::ResourceId.eq(resource_id.clone()))
            .secure()
            .scope_with(&scope)
            .one(&conn)
            .await
            .expect("query membership");
        if row.is_some() {
            owning_tenants.push(tenant_id);
        }
    }

    OverlapOutcome {
        first: first_result,
        second: second_result,
        owning_tenants,
    }
}

#[tokio::test]
async fn forced_overlap_at_serializable_keeps_one_tenant() {
    let Some(fix) = pg_fixture().await else {
        return;
    };

    let outcome = run_first_membership_overlap(&fix, TxConfig::serializable()).await;

    eprintln!(
        "forced_overlap_at_serializable_keeps_one_tenant: {} {}",
        fmt_outcome("first", &outcome.first),
        fmt_outcome("second", &outcome.second),
    );

    assert_eq!(
        outcome.owning_tenants.len(),
        1,
        "exactly one tenant may end up owning the resource; got {:?}",
        outcome.owning_tenants
    );
    assert!(
        outcome.first.is_err(),
        "the side that read before the winner committed must be cancelled, \
         not allowed to write into the predicate it already read"
    );
    let err = format!("{}", outcome.first.expect_err("checked above"));
    assert!(
        err.contains("40001") || err.contains("could not serialize access"),
        "the cancellation must be the SSI serialization failure the retry \
         wrapper knows how to re-run, got: {err}"
    );
}

/// The negative control for the test above, and the reason `add_membership`
/// cannot quietly drop to the backend default: the very same barrier, one
/// level lower, commits both sides and leaves the resource owned by two
/// tenants. If this ever stops reproducing, the pairing above has stopped
/// being what protects the invariant.
#[tokio::test]
async fn forced_overlap_at_read_committed_splits_the_resource() {
    let Some(fix) = pg_fixture().await else {
        return;
    };

    let outcome = run_first_membership_overlap(&fix, TxConfig::default()).await;

    eprintln!(
        "forced_overlap_at_read_committed_splits_the_resource: {} {}",
        fmt_outcome("first", &outcome.first),
        fmt_outcome("second", &outcome.second),
    );

    outcome
        .first
        .expect("at the backend default nothing objects");
    outcome
        .second
        .expect("at the backend default nothing objects");
    assert_eq!(
        outcome.owning_tenants.len(),
        2,
        "negative control: at the backend default both writers commit and the \
         resource ends up owned by two tenants; got {:?}",
        outcome.owning_tenants
    );
}

// -----------------------------------------------------------------------
// GTS type identity dictionary: SMALLINT identity use and first-use races
// -----------------------------------------------------------------------

/// The dictionary key stored for `code`, if any (read only).
async fn dictionary_key(fix: &PgFixture, code: &str) -> Option<i16> {
    use resource_group::infra::storage::entity::gts_type::{Column, Entity};
    let conn = fix.db.conn().expect("conn");
    Entity::find()
        .filter(Column::SchemaId.eq(code))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(&conn)
        .await
        .expect("dictionary read")
        .map(|row| row.id)
}

/// Create a key for a fresh probe identifier and return it: the identity
/// value the dictionary hands out next.
async fn probe_next_key(fix: &PgFixture) -> i16 {
    let probe = type_code("probe");
    let conn = fix.db.conn().expect("conn");
    GroupRepository
        .ensure_type_key(&conn, &probe)
        .await
        .expect("probe key");
    dictionary_key(fix, &probe).await.expect("probe row")
}

fn root_type(suffix: &str) -> CreateTypeRequest {
    CreateTypeRequest {
        code: type_code(suffix),
        can_be_root: true,
        ..CreateTypeRequest::default()
    }
}

/// PostgreSQL identity values survive rollback. Repeated placement and
/// duplicate-ID rejections must leave the SMALLINT sequence untouched, as
/// must successful writes that reuse an existing key.
#[tokio::test]
async fn deterministic_rejections_do_not_consume_dictionary_identities() {
    let Some(fix) = pg_fixture().await else {
        return;
    };
    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);
    let (type_svc, group_svc) = make_services(fix.db.clone());

    let parent_type = type_svc
        .create_type_unscoped(root_type("seqparent"))
        .await
        .unwrap();
    let parent = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(parent_type.code.clone(), "parent".to_owned()),
            tenant,
        )
        .await
        .expect("parent");
    // Admitted types that no group has used yet.
    let not_root = type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: type_code("seqnotroot"),
            can_be_root: false,
            ..CreateTypeRequest::default()
        })
        .await
        .unwrap();
    let wrong_parent = type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: type_code("seqwrongparent"),
            allowed_parent_types: vec![type_code("seqelsewhere")],
            ..CreateTypeRequest::default()
        })
        .await
        .unwrap();
    let duplicate_id_type = type_svc
        .create_type_unscoped(root_type("seqdupid"))
        .await
        .unwrap();

    let first = probe_next_key(&fix).await;
    for _ in 0..5 {
        let err = group_svc
            .create_group(
                &ctx,
                CreateGroupRequest::new(not_root.code.clone(), "root".to_owned()),
                tenant,
            )
            .await
            .expect_err("type cannot be a root");
        assert!(
            matches!(err, DomainError::InvalidParentType { .. }),
            "{err:?}"
        );

        let err = group_svc
            .create_group(
                &ctx,
                CreateGroupRequest::new(wrong_parent.code.clone(), "child".to_owned())
                    .with_parent_id(Some(parent.id)),
                tenant,
            )
            .await
            .expect_err("parent type not allowed");
        assert!(
            matches!(err, DomainError::InvalidParentType { .. }),
            "{err:?}"
        );

        let err = group_svc
            .create_group(
                &ctx,
                CreateGroupRequest::new(duplicate_id_type.code.clone(), "dup".to_owned())
                    .with_id(Some(parent.id)),
                tenant,
            )
            .await
            .expect_err("caller-supplied id already taken");
        assert!(
            matches!(err, DomainError::GroupAlreadyExists { .. }),
            "{err:?}"
        );
    }
    for code in [&not_root.code, &wrong_parent.code, &duplicate_id_type.code] {
        assert_eq!(dictionary_key(&fix, code).await, None, "{code}");
    }
    let second = probe_next_key(&fix).await;
    assert_eq!(
        second,
        first + 1,
        "deterministic rejections must not consume identity values"
    );

    // A successful first use takes one key for the group type. Later uses take none.
    for name in ["a", "b", "c"] {
        group_svc
            .create_group(
                &ctx,
                CreateGroupRequest::new(duplicate_id_type.code.clone(), name.to_owned()),
                tenant,
            )
            .await
            .expect("create");
    }
    assert_eq!(
        dictionary_key(&fix, &duplicate_id_type.code).await,
        Some(second + 1)
    );
    assert_eq!(probe_next_key(&fix).await, second + 2);
}

/// Concurrent first uses of one type -- groups and memberships -- all
/// succeed and converge on a single dictionary key: the losing inserts fail
/// with a serialization error and are retried, never surfacing a unique
/// violation or an aborted transaction.
#[tokio::test]
async fn concurrent_first_use_converges_on_one_dictionary_key() {
    const N: usize = 6;
    let Some(fix) = pg_fixture().await else {
        return;
    };
    let (type_svc, group_svc) = make_services(fix.db.clone());
    let group_svc = Arc::new(group_svc);
    let fresh = type_svc
        .create_type_unscoped(root_type("firstuse"))
        .await
        .unwrap();

    let barrier = Arc::new(tokio::sync::Barrier::new(N));
    let mut tasks = Vec::new();
    for i in 0..N {
        let group_svc = group_svc.clone();
        let barrier = barrier.clone();
        let code = fresh.code.clone();
        tasks.push(tokio::spawn(async move {
            let tenant = Uuid::now_v7();
            barrier.wait().await;
            group_svc
                .create_group(
                    &make_ctx(tenant),
                    CreateGroupRequest::new(code, format!("g{i}")),
                    tenant,
                )
                .await
        }));
    }
    for task in tasks {
        task.await.expect("task").expect("every first use succeeds");
    }
    let key = dictionary_key(&fix, &fresh.code).await.expect("one key");
    let conn = fix.db.conn().expect("conn");
    let stored = resource_group::infra::storage::entity::resource_group::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .unwrap();
    assert_eq!(stored.len(), N);
    assert!(stored.iter().all(|g| g.gts_type_id == key));

    // The same for a member type no membership has used yet.
    let member = type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: type_code("firstusemember"),
            ..CreateTypeRequest::default()
        })
        .await
        .unwrap();
    let holder = type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: type_code("firstuseholder"),
            can_be_root: true,
            allowed_membership_types: vec![member.code.clone()],
            ..CreateTypeRequest::default()
        })
        .await
        .unwrap();
    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);
    let holder_group = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(holder.code.clone(), "holder".to_owned()),
            tenant,
        )
        .await
        .unwrap();
    let members = Arc::new(common::make_membership_service(fix.db.clone()));
    let barrier = Arc::new(tokio::sync::Barrier::new(N));
    let mut tasks = Vec::new();
    for i in 0..N {
        let members = members.clone();
        let barrier = barrier.clone();
        let ctx = ctx.clone();
        let code = member.code.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            members
                .add_membership(&ctx, holder_group.id, &code, &format!("r{i}"))
                .await
        }));
    }
    for task in tasks {
        task.await.expect("task").expect("every first use succeeds");
    }
    let member_key = dictionary_key(&fix, &member.code).await.expect("one key");
    let rows = MembershipEntity::find()
        .filter(membership_entity::Column::GroupId.eq(holder_group.id))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .unwrap();
    assert_eq!(rows.len(), N);
    assert!(rows.iter().all(|m| m.gts_type_id == member_key));
}

/// Both `SERIALIZABLE` transactions see no key before either inserts. The
/// loser gets a typed serialization error and `transaction_with_retry` reads
/// the winner's key on retry. Both resolve to one row.
#[tokio::test]
async fn forced_first_use_overlap_retries_and_converges() {
    let Some(fix) = pg_fixture().await else {
        return;
    };
    let attempts = forced_first_use_overlap(&fix, TxConfig::serializable()).await;
    assert!(
        attempts > 2,
        "the loser must have been retried after a serialization failure"
    );
}

/// At lower isolation, `ON CONFLICT DO NOTHING` lets the losing insert
/// re-read the committed key without retrying.
#[tokio::test]
async fn forced_first_use_overlap_at_read_committed_rereads_the_winner() {
    let Some(fix) = pg_fixture().await else {
        return;
    };
    let attempts = forced_first_use_overlap(
        &fix,
        TxConfig::with_isolation(toolkit_db::secure::TxIsolationLevel::ReadCommitted),
    )
    .await;
    assert_eq!(attempts, 2, "neither side needs a retry");
}

/// Two transactions at `config` read "no key" for one fresh identifier, meet
/// at a barrier, then both `ensure_type_key` it. Asserts they converge on one
/// key and one row; returns the number of transaction attempts.
async fn forced_first_use_overlap(fix: &PgFixture, config: TxConfig) -> usize {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let code = type_code("forcedfirstuse");
    let attempts = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(tokio::sync::Barrier::new(2));

    let mut tasks = Vec::new();
    for _ in 0..2 {
        let db = fix.db.db();
        let code = code.clone();
        let attempts = attempts.clone();
        let barrier = barrier.clone();
        let first_attempt = Arc::new(AtomicBool::new(true));
        let config = config.clone();
        tasks.push(tokio::spawn(async move {
            db.transaction_with_retry(config, DomainError::db_err, move |tx| {
                let code = code.clone();
                let attempts = attempts.clone();
                let barrier = barrier.clone();
                let first_attempt = first_attempt.clone();
                Box::pin(async move {
                    use resource_group::infra::storage::entity::gts_type::{Column, Entity};
                    attempts.fetch_add(1, Ordering::SeqCst);
                    let seen = Entity::find()
                        .filter(Column::SchemaId.eq(code.as_str()))
                        .secure()
                        .scope_with(&AccessScope::allow_all())
                        .one(tx)
                        .await
                        .map_err(|e| DomainError::database(e.to_string()))?;
                    if first_attempt.swap(false, Ordering::SeqCst) {
                        assert!(seen.is_none(), "both first attempts see no key");
                        barrier.wait().await;
                    }
                    let key = GroupRepository.ensure_type_key(tx, &code).await?;
                    Ok::<_, DomainError>(key)
                })
            })
            .await
        }));
    }
    let mut keys = Vec::new();
    for task in tasks {
        keys.push(task.await.expect("task").expect("both first uses succeed"));
    }
    assert_eq!(keys[0], keys[1], "both converge on one key");
    let conn = fix.db.conn().expect("conn");
    let rows = resource_group::infra::storage::entity::gts_type::Entity::find()
        .filter(
            resource_group::infra::storage::entity::gts_type::Column::SchemaId.eq(code.as_str()),
        )
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    attempts.load(Ordering::SeqCst)
}

#[tokio::test]
async fn cleanup_removes_only_the_requested_users_type_family() {
    let Some(fixture) = pg_fixture().await else {
        return;
    };
    common::assert_stored_membership_cleanup(fixture.db).await;
}
