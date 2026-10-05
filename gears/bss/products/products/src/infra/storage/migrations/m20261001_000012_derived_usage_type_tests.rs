#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};

const TABLES: &[&str] = &[
    "products_derived_usage_type",
    "products_derived_usage_type_version",
];
const T1: &str = "00000000-0000-0000-0000-0000000000a1";
const T2: &str = "00000000-0000-0000-0000-0000000000a2";
const TYPE: &str = "00000000-0000-0000-0000-0000000000b1";
const ACTOR: &str = "00000000-0000-0000-0000-0000000000c1";
const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

async fn exec(db: &DatabaseConnection, sql: &str) -> Result<(), sea_orm::DbErr> {
    db.execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
        .await
        .map(|_| ())
}

async fn applied() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    Migration.up(&SchemaManager::new(&db)).await.unwrap();
    exec(
        &db,
        &format!(
            "INSERT INTO products_derived_usage_type VALUES ('{T1}', '{TYPE}', 'cloudlets', \
             'Cloudlets', '{ACTOR}', '2026-10-01T00:00:00Z')"
        ),
    )
    .await
    .unwrap();
    db
}

fn version(tenant: &str, n: i64, digest: &str) -> String {
    format!(
        "INSERT INTO products_derived_usage_type_version VALUES ('{tenant}', '{TYPE}', {n}, '{{}}', \
         '{digest}', '{ACTOR}', '2026-10-01T00:00:00Z')"
    )
}

#[tokio::test]
async fn applies_replays_and_reverts_on_sqlite() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    Migration.up(&manager).await.unwrap();
    Migration.up(&manager).await.unwrap();
    for table in TABLES {
        assert!(manager.has_table(*table).await.unwrap(), "{table}");
    }
    Migration.down(&manager).await.unwrap();
    Migration.down(&manager).await.unwrap();
    for table in TABLES {
        assert!(!manager.has_table(*table).await.unwrap(), "{table}");
    }
}

/// A version is append-only on `SQLite` too: its two triggers refuse every UPDATE and DELETE by
/// their own messages.
#[tokio::test]
async fn a_version_refuses_update_and_delete() {
    let db = applied().await;
    exec(&db, &version(T1, 1, DIGEST)).await.unwrap();
    let update = exec(
        &db,
        "UPDATE products_derived_usage_type_version SET digest = digest",
    )
    .await
    .unwrap_err();
    assert!(
        update
            .to_string()
            .contains("products_derived_usage_type_version is append-only: UPDATE"),
        "{update}"
    );
    let delete = exec(&db, "DELETE FROM products_derived_usage_type_version")
        .await
        .unwrap_err();
    assert!(
        delete
            .to_string()
            .contains("products_derived_usage_type_version is append-only: DELETE"),
        "{delete}"
    );
}

/// The version's foreign key is tenant-qualified: a version naming the type under another tenant
/// is refused, and the same type id under its own tenant is accepted.
#[tokio::test]
async fn a_version_cannot_name_another_tenants_type() {
    let db = applied().await;
    let foreign = exec(&db, &version(T2, 1, DIGEST)).await.unwrap_err();
    assert!(
        foreign
            .to_string()
            .contains("FOREIGN KEY constraint failed"),
        "{foreign}"
    );
    exec(&db, &version(T1, 1, DIGEST)).await.unwrap();
}

/// The code, the version and the digest CHECKs hold their shapes; a second type with the same code
/// in the tenant meets the unique index, and the same code in another tenant does not.
#[tokio::test]
async fn the_checks_and_the_code_index_hold() {
    let db = applied().await;
    for (n, digest, check) in [
        (0, DIGEST, "chk_products_derived_usage_type_version_version"),
        (1, "ABC", "chk_products_derived_usage_type_version_digest"),
        (
            1,
            "0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef",
            "chk_products_derived_usage_type_version_digest",
        ),
    ] {
        let e = exec(&db, &version(T1, n, digest)).await.unwrap_err();
        assert!(e.to_string().contains(check), "{n} {digest}: {e}");
    }
    let row = |tenant: &str, id: &str, code: &str| {
        format!(
            "INSERT INTO products_derived_usage_type VALUES ('{tenant}', '{id}', '{code}', 'N', \
             '{ACTOR}', '2026-10-01T00:00:00Z')"
        )
    };
    for code in ["", "Cloudlets", "-lead", "a b", &"a".repeat(65)] {
        let e = exec(&db, &row(T1, "00000000-0000-0000-0000-0000000000b9", code))
            .await
            .unwrap_err();
        assert!(
            e.to_string()
                .contains("chk_products_derived_usage_type_code"),
            "{code:?}: {e}"
        );
    }
    let taken = exec(
        &db,
        &row(T1, "00000000-0000-0000-0000-0000000000b2", "cloudlets"),
    )
    .await
    .unwrap_err();
    assert!(
        taken
            .to_string()
            .contains("products_derived_usage_type.tenant_id, products_derived_usage_type.code"),
        "{taken}"
    );
    exec(
        &db,
        &row(T2, "00000000-0000-0000-0000-0000000000b3", "cloudlets"),
    )
    .await
    .unwrap();
    exec(
        &db,
        &row(T1, "00000000-0000-0000-0000-0000000000b4", &"a".repeat(64)),
    )
    .await
    .unwrap();
}
