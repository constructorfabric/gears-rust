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
//! | 3 | two `create_type` same code | | exactly one 409 |
//! | 4 | non-force delete | create child | FK blocks create |
//! | 5 | `delete_type` | `create_group` of type | RESTRICT blocks |
//! | 6 | `add_membership` (tenant A) | `add_membership` (tenant B), same resource | exactly one commits, the other gets a clean `TenantIncompatibility` -- not a bare DB error |
//! | 7 | `remove_membership` x2, same resource | | the resource is free for another tenant once its last membership is gone |
//! | 8 | tenant check + membership insert, forced overlap at `SERIALIZABLE` | | SSI cancels one side; the resource ends up owned by one tenant |
//! | 9 | same forced overlap at the backend default | | negative control: both commit and the resource ends up owned by two tenants |

mod common;

use std::sync::Arc;

use resource_group::domain::error::DomainError;
use resource_group::domain::group_service::GroupService;
use resource_group::domain::repo::MembershipRepositoryTrait;
use resource_group::domain::type_service::TypeService;
use resource_group::infra::storage::entity::resource_group_membership::{
    self as membership_entity, Entity as MembershipEntity,
};
use resource_group::infra::storage::group_repo::GroupRepository;
use resource_group::infra::storage::membership_repo::MembershipRepository;
use resource_group::infra::storage::migrations::Migrator;
use resource_group::infra::storage::type_repo::TypeRepository;
use resource_group_sdk::models::{CreateGroupRequest, CreateTypeRequest, UpdateTypeRequest};
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
        toolkit_gts::gts_id!("cf.core.rg.type.v1~"),
        suffix,
        Uuid::now_v7().as_simple()
    )
}

fn make_services(
    db: Arc<DBProvider<DbError>>,
) -> (
    TypeService<TypeRepository>,
    GroupService<GroupRepository, TypeRepository>,
) {
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
        group_svc.move_group(a.id, Some(b.id)),
        group_svc.move_group(b.id, Some(a.id)),
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
        group_svc.move_group(parent.id, Some(other.id)),
    );

    child.expect("concurrent create child should succeed");
}

// -----------------------------------------------------------------------
// 3. two concurrent create_type of same code → 409
// -----------------------------------------------------------------------
#[tokio::test]
async fn concurrent_create_type_same_code_returns_one_409() {
    let Some(fix) = pg_fixture().await else {
        return;
    };

    let (type_svc, _) = make_services(fix.db.clone());
    let code = type_code("dup");

    let req = CreateTypeRequest {
        code: code.clone(),
        can_be_root: true,
        allowed_parent_types: vec![],
        allowed_membership_types: vec![],
        metadata_schema: None,
    };

    let (r1, r2) = tokio::join!(
        type_svc.create_type_unscoped(req.clone()),
        type_svc.create_type_unscoped(req),
    );

    match (&r1, &r2) {
        (Ok(_), Ok(_)) => panic!("both creates succeeded -- duplicate committed!"),
        (Err(e1), Err(e2)) => {
            let m = format!("{e1}{e2}");
            assert!(
                m.contains("already exists"),
                "expected 'already exists', got: {e1} / {e2}"
            );
        }
        (Ok(t), Err(e)) | (Err(e), Ok(t)) => {
            assert_eq!(t.code, code);
            assert!(
                format!("{e}").contains("already exists"),
                "expected 'already exists', got: {e}"
            );
        }
    }
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
// 5. delete_type vs create_group of that type
// -----------------------------------------------------------------------
#[tokio::test]
async fn concurrent_delete_type_and_create_group() {
    let Some(fix) = pg_fixture().await else {
        return;
    };

    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);
    let (type_svc, group_svc) = make_services(fix.db.clone());

    let rt = type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: type_code("deltp"),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create type");

    let (del_res, create_res) = tokio::join!(
        type_svc.delete_type(&ctx, &rt.code),
        group_svc.create_group(
            &ctx,
            CreateGroupRequest::new(rt.code.clone(), "Race".to_owned()),
            tenant_id
        ),
    );

    match (&del_res, &create_res) {
        (Ok(()), Ok(_)) => {
            panic!("both delete_type and create_group succeeded -- type in use but removed!")
        }
        (Err(e1), Err(e2)) => {
            let m = format!("{e1}{e2}");
            assert!(
                m.contains("conflict") || m.contains("references") || m.contains("not found"),
                "expected conflict/references, got: {e1} / {e2}"
            );
        }
        (Ok(()), Err(e)) | (Err(e), Ok(_)) => {
            let m = format!("{e}");
            assert!(
                m.contains("conflict") || m.contains("references") || m.contains("not found"),
                "expected conflict/references/not_found, got: {e}"
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

    let conn = fix.db.conn().expect("conn");
    let gts_type_id: i16 = TypeRepository::resolve_id(&conn, &member_type.code)
        .await
        .expect("resolve type id")
        .expect("type must exist");

    let resource_id = "overlap-res".to_owned();
    let db = fix.db.db();

    let (a_checked_tx, a_checked_rx) = tokio::sync::oneshot::channel::<()>();
    let (b_done_tx, b_done_rx) = tokio::sync::oneshot::channel::<()>();

    let db_a = db.clone();
    let resource_a = resource_id.clone();
    let config_a = config.clone();
    let first = tokio::spawn(async move {
        db_a.transaction_ref_mapped_with_config::<_, (), DomainError>(config_a, move |tx| {
            let resource_id = resource_a.clone();
            // Ignored, not `expect`ed: at SERIALIZABLE the peer may already
            // have returned by the time this fires, and its receiver going
            // away is an outcome of the race, not a reason to panic.
            let a_checked_tx = a_checked_tx;
            let b_done_rx = b_done_rx;
            Box::pin(async move {
                let conflict = MembershipRepository
                    .has_membership_in_other_tenant(tx, gts_type_id, &resource_id, tenant_a)
                    .await?;
                assert!(!conflict, "A must see an unclaimed resource");
                let _send = a_checked_tx.send(());
                let _recv = b_done_rx.await;
                MembershipRepository
                    .insert(tx, group_a, gts_type_id, &resource_id)
                    .await?;
                Ok(())
            })
        })
        .await
    });

    a_checked_rx.await.expect("A must reach its check");

    let db_b = db.clone();
    let resource_b = resource_id.clone();
    let config_b = config.clone();
    let second = tokio::spawn(async move {
        let out = db_b
            .transaction_ref_mapped_with_config::<_, (), DomainError>(config_b, move |tx| {
                let resource_id = resource_b.clone();
                Box::pin(async move {
                    let conflict = MembershipRepository
                        .has_membership_in_other_tenant(tx, gts_type_id, &resource_id, tenant_b)
                        .await?;
                    assert!(!conflict, "B must still see an unclaimed resource");
                    MembershipRepository
                        .insert(tx, group_b, gts_type_id, &resource_id)
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
            .filter(membership_entity::Column::GtsTypeId.eq(gts_type_id))
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
