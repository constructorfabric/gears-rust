//! Repository tests on an in-memory `SQLite` database: scope enforcement on
//! insert and the `ScopeError` to `DomainError` mapping.

use construct_sdk::models::FoundationNote;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::ScopeError;
use toolkit_db::{ConnectOpts, Db, DbError, connect_db};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::migrations::Migrator;
use super::sea_orm_repo::{SeaOrmNoteRepository, map_scope_error};
use crate::domain::error::DomainError;
use crate::domain::repo::NoteRepository;

async fn inmem_db() -> Db {
    use sea_orm_migration::MigratorTrait;

    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("connect in-memory database");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrations");
    db
}

#[tokio::test]
async fn insert_outside_scope_is_forbidden() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let repo = SeaOrmNoteRepository::new();
    let scope = AccessScope::for_tenant(Uuid::new_v4());
    let note = FoundationNote::new(Uuid::new_v4(), Uuid::new_v4(), "text".to_owned());

    let err = repo
        .insert(&conn, &scope, note.clone())
        .await
        .expect_err("insert outside the scope must fail");

    assert!(matches!(err, DomainError::Forbidden(_)), "got {err:?}");
    let found = repo
        .find_by_id(&conn, &AccessScope::allow_all(), note.id)
        .await
        .expect("lookup");
    assert!(found.is_none(), "nothing may be persisted");
}

#[tokio::test]
async fn insert_inside_scope_round_trips() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let repo = SeaOrmNoteRepository::new();
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let note = FoundationNote::new(Uuid::new_v4(), tenant, "text".to_owned());

    let stored = repo
        .insert(&conn, &scope, note.clone())
        .await
        .expect("insert");
    assert_eq!(stored, note);

    let found = repo.find_by_id(&conn, &scope, note.id).await.expect("find");
    assert_eq!(found, Some(note));
}

#[test]
fn map_scope_error_maps_each_variant() {
    let tenant_id = Uuid::new_v4();
    let cases: Vec<(ScopeError, &str)> = vec![
        (ScopeError::Denied("nope"), "forbidden"),
        (ScopeError::Invalid("bad"), "internal"),
        (ScopeError::TenantNotInScope { tenant_id }, "forbidden"),
        (
            ScopeError::Db(sea_orm::DbErr::Custom("boom".to_owned())),
            "database",
        ),
        (
            ScopeError::UnresolvedScopeProperty {
                element: "n",
                property: "p".to_owned(),
            },
            "internal",
        ),
    ];

    for (input, expected) in cases {
        let label = format!("{input:?}");
        let kind = match map_scope_error(input) {
            DomainError::Forbidden(_) => "forbidden",
            DomainError::Internal(_) => "internal",
            DomainError::Database(DbError::Sea(_)) => "database",
            other => panic!("{label} mapped to unexpected {other:?}"),
        };
        assert_eq!(kind, expected, "{label}");
    }
}

#[test]
fn unknown_scope_error_message_is_neutral() {
    let err = map_scope_error(ScopeError::UnresolvedScopeProperty {
        element: "n",
        property: "p".to_owned(),
    });
    let DomainError::Internal(msg) = err else {
        panic!("expected Internal");
    };
    assert!(msg.starts_with("unhandled scope error"), "{msg}");
}
