//! Schema tests for the policy-engine migrations, run against in-memory
//! `SQLite`: the migrations apply, every table and named index exists and
//! matches its entity, and `down` removes them again.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use sea_orm::{ConnectionTrait, DatabaseConnection, EntityTrait, Statement};
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::{AccessScope, SecureEntityExt};
use toolkit_db::{ConnectOpts, connect_db};

use super::entity::{assignment, bundle, bundle_version, document};
use super::migrations::{self, Migrator, own_indexes, own_tables, postgres_initial_ddl};

async fn sqlite_names(conn: &DatabaseConnection, kind: &str) -> Vec<String> {
    conn.query_all_raw(Statement::from_string(
        conn.get_database_backend(),
        format!("SELECT name FROM sqlite_master WHERE type = '{kind}'"),
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.try_get::<String>("", "name").unwrap())
    .collect()
}

#[tokio::test]
async fn migrations_apply_and_every_table_matches_its_entity() {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts).await.unwrap();
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .map_err(|e| e.to_string())
        .expect("migrations apply");
    let conn = db.conn().unwrap();
    let all = AccessScope::allow_all();

    // A select over every entity column fails if the table or any column is
    // missing, so each call also pins entity/DDL agreement.
    macro_rules! select_all {
        ($($entity:ident),*) => {$(
            $entity::Entity::find()
                .secure()
                .scope_with(&all)
                .all(&conn)
                .await
                .unwrap();
        )*};
    }
    select_all!(bundle, bundle_version, document, assignment);
}

#[tokio::test]
async fn tables_and_named_indexes_exist_and_down_removes_them() {
    let conn = sea_orm::Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(&conn, None).await.unwrap();

    let tables = sqlite_names(&conn, "table").await;
    for table in own_tables() {
        assert!(tables.iter().any(|t| t == table), "missing table {table}");
    }
    let indexes = sqlite_names(&conn, "index").await;
    for index in own_indexes() {
        assert!(indexes.iter().any(|i| i == index), "missing index {index}");
    }

    Migrator::down(&conn, None).await.unwrap();
    let left = sqlite_names(&conn, "table").await;
    assert!(
        left.iter().all(|t| !t.starts_with("policy_engine__")),
        "down left tables behind: {left:?}"
    );
}

#[test]
fn mysql_is_refused() {
    let err = migrations::ensure_supported_backend(sea_orm::DatabaseBackend::MySql).unwrap_err();
    assert!(err.to_string().contains("not supported"), "{err}");
    migrations::ensure_supported_backend(sea_orm::DatabaseBackend::Postgres).unwrap();
    migrations::ensure_supported_backend(sea_orm::DatabaseBackend::Sqlite).unwrap();
}

#[test]
fn postgres_ddl_renders_the_partial_unique_indexes_and_named_objects() {
    for name in own_tables().into_iter().chain(own_indexes()) {
        assert!(name.len() <= 63, "{name} exceeds 63 bytes");
        assert!(
            name.contains("policy_engine__"),
            "{name} lacks the namespace"
        );
    }
    for (index, state) in [("one_active", 2), ("one_draft", 1)] {
        let ddl = postgres_initial_ddl()
            .into_iter()
            .find(|s| s.contains(&format!("uq_policy_engine__bundle_version__{index}")))
            .unwrap();
        assert!(ddl.starts_with("CREATE UNIQUE INDEX"), "{ddl}");
        assert!(
            ddl.ends_with(&format!("WHERE \"state\" = {state}")),
            "{ddl}"
        );
    }
}
