// Created: 2026-10-03 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! `m0002_value_versions` has no `down`: it is irreversible by design.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbErr};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};

use super::super::Migrator;

/// A fresh in-memory `SQLite` database with every credstore migration applied.
async fn migrated_db() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    Migrator::up(&db, None).await.expect("migrations apply");
    db
}

fn assert_irreversible(err: &DbErr) {
    let DbErr::Migration(message) = err else {
        panic!("expected DbErr::Migration, got {err:?}");
    };
    assert!(
        message.contains("m0002_value_versions is irreversible"),
        "{message}"
    );
    assert!(message.contains("snapshot"), "{message}");
}

/// Nothing was rolled back: the schema is still the post-`up` one.
async fn assert_schema_is_post_up(db: &DatabaseConnection) {
    let manager = SchemaManager::new(db);
    for table in [
        "credstore_secrets",
        "credstore_write_intents",
        "credstore_store_cleanup",
    ] {
        assert!(
            manager.has_table(table).await.expect("has_table"),
            "{table} must still exist"
        );
    }
    assert!(
        manager
            .has_column("credstore_secrets", "value_version")
            .await
            .expect("has_column")
    );
    assert!(
        !manager
            .has_column("credstore_secrets", "value_fp")
            .await
            .expect("has_column")
    );
    db.execute_unprepared("SELECT 1 FROM credstore_write_intents")
        .await
        .expect("intent table is queryable");
    db.execute_unprepared("SELECT 1 FROM credstore_store_cleanup")
        .await
        .expect("cleanup table is queryable");
}

// -- the intent journal and the cleanup debts -------------------------------

mod bookkeeping {
    use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
    use sea_orm_migration::SchemaManager;
    use uuid::Uuid;

    use super::migrated_db;

    async fn index_names(db: &DatabaseConnection, table: &str) -> Vec<String> {
        db.query_all_raw(Statement::from_string(
            db.get_database_backend(),
            format!("SELECT name FROM pragma_index_list('{table}')"),
        ))
        .await
        .expect("index list")
        .iter()
        .map(|r| r.try_get_by_index::<String>(0).expect("index name"))
        .collect()
    }

    fn blob(id: Uuid) -> String {
        format!("x'{}'", id.simple())
    }

    #[tokio::test]
    async fn intents_carry_the_reference_and_only_point_lookup_indexes() {
        let db = migrated_db().await;
        assert!(
            SchemaManager::new(&db)
                .has_column("credstore_write_intents", "reference")
                .await
                .expect("has_column")
        );

        let indexes = index_names(&db, "credstore_write_intents").await;
        assert!(indexes.contains(&"idx_credstore_write_intents_record".to_owned()));
        assert!(indexes.contains(&"idx_credstore_write_intents_ref".to_owned()));
        assert!(
            !indexes.contains(&"idx_credstore_write_intents_lease".to_owned()),
            "nothing scans the table, so there is no lease index: {indexes:?}"
        );
    }

    #[tokio::test]
    async fn intent_reference_is_bounded_like_the_records_reference() {
        let db = migrated_db().await;
        let insert = |reference: String| {
            format!(
                "INSERT INTO credstore_write_intents \
                 (attempt_id, tenant_id, record_id, reference, lease_until) \
                 VALUES ({}, {}, {}, '{reference}', '2030-01-01T00:00:00.000Z')",
                blob(Uuid::new_v4()),
                blob(Uuid::new_v4()),
                blob(Uuid::new_v4())
            )
        };
        db.execute_unprepared(&insert("openai-key".to_owned()))
            .await
            .expect("a normal reference is stored");
        db.execute_unprepared(&insert(String::new()))
            .await
            .expect_err("an empty reference violates the CHECK");
        db.execute_unprepared(&insert("x".repeat(256)))
            .await
            .expect_err("a 256-char reference violates the CHECK");
    }

    #[tokio::test]
    async fn cleanup_debts_have_the_record_index_and_a_database_clock_default() {
        let db = migrated_db().await;
        let indexes = index_names(&db, "credstore_store_cleanup").await;
        assert!(indexes.contains(&"idx_credstore_store_cleanup_record".to_owned()));

        db.execute_unprepared(&format!(
            "INSERT INTO credstore_store_cleanup (id, tenant_id, record_id, op) \
             VALUES ({}, {}, {}, 1)",
            blob(Uuid::new_v4()),
            blob(Uuid::new_v4()),
            blob(Uuid::new_v4())
        ))
        .await
        .expect("a purge debt needs no created_at");
        let created = db
            .query_one_raw(Statement::from_string(
                db.get_database_backend(),
                "SELECT created_at FROM credstore_store_cleanup".to_owned(),
            ))
            .await
            .expect("select")
            .expect("row")
            .try_get_by_index::<String>(0)
            .expect("created_at");
        assert!(!created.is_empty());
    }

    #[tokio::test]
    async fn cleanup_debt_codes_and_shape_are_checked() {
        let db = migrated_db().await;
        let insert = |op: i16, selector: &str, version: &str| {
            format!(
                "INSERT INTO credstore_store_cleanup \
                 (id, tenant_id, record_id, op, selector, version) \
                 VALUES ({}, {}, {}, {op}, {selector}, {version})",
                blob(Uuid::new_v4()),
                blob(Uuid::new_v4()),
                blob(Uuid::new_v4())
            )
        };
        // purge: no selector, no version.
        db.execute_unprepared(&insert(1, "NULL", "NULL"))
            .await
            .expect("purge");
        // destroy: below and exact, with a version.
        db.execute_unprepared(&insert(2, "1", "'7'"))
            .await
            .expect("destroy below");
        db.execute_unprepared(&insert(2, "2", "'7'"))
            .await
            .expect("destroy exact");
        // Codes outside the documented sets.
        db.execute_unprepared(&insert(3, "NULL", "NULL"))
            .await
            .expect_err("op 3 is not a code");
        db.execute_unprepared(&insert(2, "3", "'7'"))
            .await
            .expect_err("selector 3 is not a code");
        // Shape: purge with a selector or a version, destroy without either.
        db.execute_unprepared(&insert(1, "1", "NULL"))
            .await
            .expect_err("purge carries no selector");
        db.execute_unprepared(&insert(1, "NULL", "'7'"))
            .await
            .expect_err("purge carries no version");
        db.execute_unprepared(&insert(2, "NULL", "'7'"))
            .await
            .expect_err("destroy needs a selector");
        db.execute_unprepared(&insert(2, "1", "NULL"))
            .await
            .expect_err("destroy needs a version");
    }
}

#[tokio::test]
async fn down_is_irreversible_and_leaves_the_schema_untouched() {
    let db = migrated_db().await;

    let err = super::Migration
        .down(&SchemaManager::new(&db))
        .await
        .expect_err("m0002 cannot be rolled back");

    assert_irreversible(&err);
    assert_schema_is_post_up(&db).await;
}

#[tokio::test]
async fn migrator_down_one_step_fails_and_applies_nothing() {
    let db = migrated_db().await;

    let err = Migrator::down(&db, Some(1))
        .await
        .expect_err("rolling back m0002 must fail");

    assert_irreversible(&err);
    assert_schema_is_post_up(&db).await;
}

// -- guard: credentials written before this release need the migration tool --

mod guard {
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbErr};
    use sea_orm_migration::MigratorTrait;
    use uuid::Uuid;

    use super::super::super::Migrator;
    use super::super::{
        VALUE_MIGRATION_COPY_DONE_PHASES, VALUE_MIGRATION_STATE_TABLE, VALUES_NOT_MIGRATED,
    };

    /// A database in the shipped (`m0001` only) state.
    async fn shipped_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite connects");
        Migrator::up(&db, Some(1)).await.expect("m0001 applies");
        db
    }

    async fn insert_row(db: &DatabaseConnection, status: i16) {
        let sql = format!(
            "INSERT INTO credstore_secrets \
             (id, tenant_id, reference, sharing, owner_id, status, value_fp, fp_key_id) \
             VALUES (x'{}', x'{}', 'ref-{status}', 2, x'{}', {status}, x'00', 1)",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple(),
            Uuid::nil().simple()
        );
        db.execute_unprepared(&sql).await.expect("row inserts");
    }

    /// Creates the tool's header table the way the tool does (the columns the
    /// guard reads) holding one header row.
    async fn mark(db: &DatabaseConnection, phase: &str, discard_values: bool) {
        db.execute_unprepared(&format!(
            "CREATE TABLE {VALUE_MIGRATION_STATE_TABLE} \
             (phase TEXT NOT NULL, discard_values BOOLEAN NOT NULL)"
        ))
        .await
        .expect("marker table creates");
        db.execute_unprepared(&format!(
            "INSERT INTO {VALUE_MIGRATION_STATE_TABLE} (phase, discard_values) \
             VALUES ('{phase}', {})",
            i32::from(discard_values)
        ))
        .await
        .expect("marker row inserts");
    }

    async fn is_migrated(db: &DatabaseConnection) -> bool {
        db.execute_unprepared("SELECT value_version FROM credstore_secrets")
            .await
            .is_ok()
    }

    fn assert_refused(result: &Result<(), DbErr>) {
        let Err(DbErr::Migration(message)) = result else {
            panic!("expected the guard's DbErr::Migration, got {result:?}");
        };
        assert_eq!(message, VALUES_NOT_MIGRATED);
        assert!(message.contains("credstore-value-migration migrate"));
        assert!(message.contains("--discard-values"));
    }

    #[tokio::test]
    async fn active_rows_without_the_tool_marker_fail_the_migration_and_change_nothing() {
        let db = shipped_db().await;
        insert_row(&db, 2).await;

        assert_refused(&Migrator::up(&db, None).await);
        assert!(!is_migrated(&db).await, "the schema must be untouched");
    }

    #[tokio::test]
    async fn the_marker_must_name_a_phase_past_copying() {
        let db = shipped_db().await;
        insert_row(&db, 2).await;
        mark(&db, "copying", false).await;

        // Header says copying is not finished: still refused.
        assert_refused(&Migrator::up(&db, None).await);
        db.execute_unprepared(&format!(
            "UPDATE {VALUE_MIGRATION_STATE_TABLE} SET phase = 'verifying'"
        ))
        .await
        .expect("update");
        assert_refused(&Migrator::up(&db, None).await);
        assert!(!is_migrated(&db).await);
    }

    #[tokio::test]
    async fn every_phase_past_copying_lets_the_migration_run() {
        for phase in VALUE_MIGRATION_COPY_DONE_PHASES {
            let db = shipped_db().await;
            insert_row(&db, 2).await;
            mark(&db, phase, false).await;

            Migrator::up(&db, None)
                .await
                .unwrap_or_else(|e| panic!("phase {phase}: {e}"));
            assert!(is_migrated(&db).await, "phase {phase}");
        }
    }

    #[tokio::test]
    async fn the_discard_decision_lets_the_migration_run() {
        let db = shipped_db().await;
        insert_row(&db, 2).await;
        mark(&db, "verifying", true).await;

        Migrator::up(&db, None)
            .await
            .expect("discard_values passes");
        assert!(is_migrated(&db).await);
    }

    #[tokio::test]
    async fn an_empty_marker_table_is_not_a_marker() {
        let db = shipped_db().await;
        insert_row(&db, 2).await;
        db.execute_unprepared(&format!(
            "CREATE TABLE {VALUE_MIGRATION_STATE_TABLE} \
             (phase TEXT NOT NULL, discard_values BOOLEAN NOT NULL)"
        ))
        .await
        .expect("marker table creates");

        assert_refused(&Migrator::up(&db, None).await);
    }

    #[tokio::test]
    async fn a_fresh_installation_passes_without_the_tool() {
        let db = shipped_db().await;
        Migrator::up(&db, None)
            .await
            .expect("no rows: nothing to move");
        assert!(is_migrated(&db).await);
    }

    #[tokio::test]
    async fn rows_that_were_never_active_do_not_trigger_the_guard() {
        // `provisioning` and `deprovisioning` rows carry no value: m0002 drops them.
        let db = shipped_db().await;
        insert_row(&db, 1).await;
        insert_row(&db, 3).await;

        Migrator::up(&db, None)
            .await
            .expect("only retired statuses: passes");
        assert!(is_migrated(&db).await);
    }
}
