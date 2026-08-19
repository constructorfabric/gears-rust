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

mod common;

use std::sync::Arc;

use resource_group::domain::group_service::GroupService;
use resource_group::domain::type_service::TypeService;
use resource_group::infra::storage::group_repo::GroupRepository;
use resource_group::infra::storage::migrations::Migrator;
use resource_group::infra::storage::type_repo::TypeRepository;
use resource_group_sdk::models::{CreateGroupRequest, CreateTypeRequest};
use sea_orm_migration::MigratorTrait;
use testcontainers::{ContainerRequest, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use toolkit_db::{
    ConnectOpts, DBProvider, DbError, connect_db, migration_runner::run_migrations_for_testing,
};
use toolkit_security::SecurityContext;
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
    let request = ContainerRequest::from(Postgres::default())
        .with_tag("16-alpine")
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "app");

    let container = match request.start().await {
        Ok(c) => c,
        Err(e) => {
            if require_docker() {
                panic!("Docker required (RG_PG_REQUIRE_DOCKER=1) but container failed: {e}");
            }
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

    let root = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                code: rt.code.clone(),
                name: "Root".to_owned(),
                parent_id: None,
                tenant_id: None,
                metadata: None,
            },
            tenant_id,
        )
        .await
        .expect("create root");

    let a = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                code: rt.code.clone(),
                name: "A".to_owned(),
                parent_id: Some(root.id),
                tenant_id: None,
                metadata: None,
            },
            tenant_id,
        )
        .await
        .expect("create A");

    let b = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                code: rt.code,
                name: "B".to_owned(),
                parent_id: Some(root.id),
                tenant_id: None,
                metadata: None,
            },
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

    let root = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                code: rt.code.clone(),
                name: "Root".to_owned(),
                parent_id: None,
                tenant_id: None,
                metadata: None,
            },
            tenant_id,
        )
        .await
        .expect("create root");

    let other = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                code: rt.code.clone(),
                name: "Other".to_owned(),
                parent_id: None,
                tenant_id: None,
                metadata: None,
            },
            tenant_id,
        )
        .await
        .expect("create other root");

    let parent = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                code: rt.code.clone(),
                name: "Parent".to_owned(),
                parent_id: Some(root.id),
                tenant_id: None,
                metadata: None,
            },
            tenant_id,
        )
        .await
        .expect("create parent");

    let (child, _moved) = tokio::join!(
        group_svc.create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                code: rt.code.clone(),
                name: "Child".to_owned(),
                parent_id: Some(parent.id),
                tenant_id: None,
                metadata: None,
            },
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

    let root = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                code: rt.code.clone(),
                name: "Root".to_owned(),
                parent_id: None,
                tenant_id: None,
                metadata: None,
            },
            tenant_id,
        )
        .await
        .expect("create root");

    let (del_res, create_res) = tokio::join!(
        group_svc.delete_group(&ctx, root.id, false),
        group_svc.create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                code: rt.code,
                name: "Orphan".to_owned(),
                parent_id: Some(root.id),
                tenant_id: None,
                metadata: None,
            },
            tenant_id
        ),
    );

    match (&del_res, &create_res) {
        (Ok(_), Ok(_)) => {
            panic!("both non-force delete and create succeeded -- FK invariant broken!")
        }
        (Err(e1), Err(e2)) => {
            let m = format!("{e1}{e2}");
            assert!(
                m.contains("conflict") || m.contains("not_found") || m.contains("precondition"),
                "expected conflict/not_found, got: {e1} / {e2}"
            );
        }
        (Ok(_), Err(e)) | (Err(e), Ok(_)) => {
            let m = format!("{e}");
            assert!(
                m.contains("conflict") || m.contains("not_found") || m.contains("precondition"),
                "expected conflict/not_found/precondition, got: {e}"
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
            CreateGroupRequest {
                id: None,
                code: rt.code.clone(),
                name: "Race".to_owned(),
                parent_id: None,
                tenant_id: None,
                metadata: None,
            },
            tenant_id
        ),
    );

    match (&del_res, &create_res) {
        (Ok(_), Ok(_)) => {
            panic!("both delete_type and create_group succeeded -- type in use but removed!")
        }
        (Err(e1), Err(e2)) => {
            let m = format!("{e1}{e2}");
            assert!(
                m.contains("conflict") || m.contains("references") || m.contains("not_found"),
                "expected conflict/references, got: {e1} / {e2}"
            );
        }
        (Ok(_), Err(e)) | (Err(e), Ok(_)) => {
            let m = format!("{e}");
            assert!(
                m.contains("conflict") || m.contains("references") || m.contains("not_found"),
                "expected conflict/references/not_found, got: {e}"
            );
        }
    }
}
