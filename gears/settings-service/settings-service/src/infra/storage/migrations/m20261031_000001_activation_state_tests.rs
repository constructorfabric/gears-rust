//! The activation migration on `SQLite`: the two tables, the `audit_records`
//! rebuild and what the rebuild must carry over.

use sea_orm_migration::MigratorTrait;
use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};

use super::super::Migrator;

async fn run(db: &DatabaseConnection, sql: &str) -> Result<(), sea_orm_migration::sea_orm::DbErr> {
    db.execute_unprepared(sql).await.map(|_| ())
}

async fn migrated() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    Migrator::up(&db, None).await.expect("migrations apply");
    db
}

async fn names(db: &DatabaseConnection, sql: &str) -> Vec<String> {
    db.query_all_raw(Statement::from_string(db.get_database_backend(), sql))
        .await
        .expect("listing")
        .iter()
        .map(|r| r.try_get::<String>("", "name").expect("name"))
        .collect()
}

async fn indexes_of(db: &DatabaseConnection, table: &str) -> Vec<String> {
    names(
        db,
        &format!("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = '{table}';"),
    )
    .await
}

fn subscription(id: &str, subscriber: &str, key: &str, tenant: &str) -> String {
    format!(
        "INSERT INTO subscriptions (id, subscriber, key, tenant, created_at)
         VALUES ('{id}', '{subscriber}', '{key}', '{tenant}', 'now');"
    )
}

fn state(subscriber: &str, key: &str, tenant: &str, status: Option<&str>) -> String {
    let (column, value) = status.map_or((String::new(), String::new()), |s| {
        (", status".to_owned(), format!(", '{s}'"))
    });
    format!(
        "INSERT INTO activation_state
           (subscriber, key, tenant, change_ref, expected_value, updated_at{column})
         VALUES ('{subscriber}', '{key}', '{tenant}', 'ref', 'true', 'now'{value});"
    )
}

fn audit(id: &str, operation: &str) -> String {
    format!(
        "INSERT INTO audit_records (id, resource, declaration_key, tenant_id, operation, actor, \
         actor_classification, outcome, request_id, occurred_at)
         VALUES ('{id}', 'cf.settings:k@t', 'k', 't', '{operation}', 'who', 'public', \
         'success', 'req', 'now');"
    )
}

#[tokio::test]
async fn a_subscription_is_unique_per_subscriber_key_and_tenant() {
    let db = migrated().await;
    run(&db, &subscription("a", "gear", "k", "t"))
        .await
        .unwrap();
    assert!(
        run(&db, &subscription("b", "gear", "k", "t"))
            .await
            .is_err(),
        "uq_sub_subscriber_key_tenant refuses the same triple twice"
    );
    run(&db, &subscription("c", "gear", "k", "other"))
        .await
        .expect("another tenant is another subscription");
    run(&db, &subscription("d", "other-gear", "k", "t"))
        .await
        .expect("another subscriber is another subscription");
    assert!(
        indexes_of(&db, "subscriptions")
            .await
            .contains(&"idx_sub_key".to_owned())
    );
}

#[tokio::test]
async fn an_activation_state_is_unique_awaiting_by_default_and_checked() {
    let db = migrated().await;
    run(&db, &state("gear", "k", "t", None)).await.unwrap();
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT status, applied_value, detail, reported_at, last_notified_at
               FROM activation_state;",
        ))
        .await
        .unwrap();
    assert_eq!(
        rows[0].try_get::<String>("", "status").unwrap(),
        "awaiting",
        "the default status"
    );
    for column in ["applied_value", "detail", "reported_at", "last_notified_at"] {
        assert_eq!(
            rows[0].try_get::<Option<String>>("", column).unwrap(),
            None,
            "{column} is nullable and starts absent"
        );
    }
    assert!(
        run(&db, &state("gear", "k", "t", Some("failed")))
            .await
            .is_err(),
        "uq_activation_state keeps one row per subscription"
    );
    assert!(
        run(&db, &state("gear", "k", "u", Some("expired")))
            .await
            .is_err(),
        "a status outside the vocabulary is refused"
    );
    for status in ["succeeded", "failed"] {
        run(&db, &state("gear", status, "t", Some(status)))
            .await
            .unwrap_or_else(|e| panic!("`{status}` is in the vocabulary: {e}"));
    }
    let indexes = indexes_of(&db, "activation_state").await;
    for expected in ["idx_activation_key_tenant", "idx_activation_awaiting"] {
        assert!(indexes.contains(&expected.to_owned()), "{indexes:?}");
    }
}

#[tokio::test]
async fn the_awaiting_index_is_partial_on_the_awaiting_status() {
    let db = migrated().await;
    let sql = names(
        &db,
        "SELECT sql AS name FROM sqlite_master WHERE name = 'idx_activation_awaiting';",
    )
    .await;
    assert_eq!(sql.len(), 1);
    assert!(
        sql[0].contains("WHERE status = 'awaiting'"),
        "the index is partial: {}",
        sql[0]
    );
}

#[tokio::test]
async fn audit_records_take_activation_with_a_nullable_change_ref_and_detail() {
    let db = migrated().await;
    run(&db, &audit("act", "activation")).await.unwrap();
    run(
        &db,
        "INSERT INTO audit_records (id, resource, declaration_key, tenant_id, operation, actor, \
         actor_classification, outcome, request_id, occurred_at, change_ref, detail)
         VALUES ('with-ref', 'cf.settings:k@t', 'k', 't', 'activation', 'gear', 'public', \
         'failure', 'req', 'now', 'ref', 'could not apply');",
    )
    .await
    .unwrap();
    assert!(run(&db, &audit("bad", "delete")).await.is_err());
    let indexes = indexes_of(&db, "audit_records").await;
    for expected in [
        "idx_audit_scoped",
        "idx_audit_retention",
        "idx_audit_default_horizon",
        "idx_audit_change_ref",
    ] {
        assert!(indexes.contains(&expected.to_owned()), "{indexes:?}");
    }
    let sql = names(
        &db,
        "SELECT sql AS name FROM sqlite_master WHERE name = 'idx_audit_change_ref';",
    )
    .await;
    assert!(
        sql[0].contains("WHERE change_ref IS NOT NULL"),
        "the index is partial: {}",
        sql[0]
    );
}

#[tokio::test]
async fn the_rebuild_keeps_the_rows_and_the_append_only_trigger() {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    let total = u32::try_from(Migrator::migrations().len()).expect("a small list");
    Migrator::up(&db, Some(total - 1))
        .await
        .expect("everything before the activation migration applies");
    run(&db, &audit("old", "change")).await.unwrap();

    Migrator::up(&db, None)
        .await
        .expect("the migration applies");

    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT id, change_ref, detail FROM audit_records;",
        ))
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "a record from before survives the rebuild");
    assert_eq!(rows[0].try_get::<String>("", "id").unwrap(), "old");
    assert_eq!(
        rows[0].try_get::<Option<String>>("", "change_ref").unwrap(),
        None
    );
    assert!(
        run(
            &db,
            "UPDATE audit_records SET actor = 'someone' WHERE id = 'old';"
        )
        .await
        .is_err(),
        "the append-only trigger is back on the rebuilt table"
    );
}

#[tokio::test]
async fn rolling_back_drops_the_tables_and_narrows_the_vocabulary() {
    let db = migrated().await;
    run(&db, &audit("old", "change")).await.unwrap();
    run(&db, &subscription("a", "gear", "k", "t"))
        .await
        .unwrap();

    Migrator::down(&db, Some(1)).await.expect("one step back");

    assert!(
        run(&db, &subscription("b", "gear", "k", "u"))
            .await
            .is_err()
    );
    assert!(run(&db, &state("gear", "k", "t", None)).await.is_err());
    assert!(
        run(&db, &audit("act", "activation")).await.is_err(),
        "an earlier build's vocabulary refuses `activation`"
    );
    let kept = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT id FROM audit_records;",
        ))
        .await
        .unwrap();
    assert_eq!(kept.len(), 1, "the rollback keeps what was recorded");
    assert!(
        !indexes_of(&db, "audit_records")
            .await
            .contains(&"idx_audit_change_ref".to_owned())
    );

    Migrator::up(&db, None).await.expect("re-applied");
    run(&db, &audit("act", "activation")).await.unwrap();
}
