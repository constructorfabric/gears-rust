//! The activation migration on `SQLite`: the two per-instance tables, the
//! `audit_records` rebuild and what the rebuild must carry over.

use sea_orm_migration::MigratorTrait;
use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};

use super::super::Migrator;
use super::super::migrations_tests::{before, from};

/// This migration's name in the list.
const ACTIVATION: &str = "m20261031_000001_activation_state";

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

async fn audit_columns(db: &DatabaseConnection) -> Vec<String> {
    names(db, "SELECT name FROM pragma_table_info('audit_records');").await
}

async fn index_sql(db: &DatabaseConnection, index: &str) -> String {
    let sql = names(
        db,
        &format!("SELECT sql AS name FROM sqlite_master WHERE name = '{index}';"),
    )
    .await;
    assert_eq!(sql.len(), 1, "`{index}` exists");
    sql[0].clone()
}

fn subscription(id: &str, subscriber: &str, instance: &str, key: &str, tenant: &str) -> String {
    format!(
        "INSERT INTO subscriptions (id, subscriber, instance_id, key, tenant, created_at)
         VALUES ('{id}', '{subscriber}', '{instance}', '{key}', '{tenant}', 'now');"
    )
}

fn state(
    subscriber: &str,
    instance: &str,
    key: &str,
    tenant: &str,
    status: Option<&str>,
) -> String {
    let (column, value) = status.map_or((String::new(), String::new()), |s| {
        (", status".to_owned(), format!(", '{s}'"))
    });
    format!(
        "INSERT INTO activation_state
           (subscriber, instance_id, key, tenant, change_ref, changed_at, expected_value,
            updated_at{column})
         VALUES ('{subscriber}', '{instance}', '{key}', '{tenant}', 'ref', 'now', 'true',
                 'now'{value});"
    )
}

fn audit(id: &str, operation: &str) -> String {
    audit_with_outcome(id, operation, "success")
}

fn audit_with_outcome(id: &str, operation: &str, outcome: &str) -> String {
    format!(
        "INSERT INTO audit_records (id, resource, declaration_key, tenant_id, operation, actor, \
         actor_classification, outcome, request_id, occurred_at)
         VALUES ('{id}', 'cf.settings:k@t', 'k', 't', '{operation}', 'who', 'public', \
         '{outcome}', 'req', 'now');"
    )
}

#[tokio::test]
async fn a_subscription_is_unique_per_subscriber_instance_key_and_tenant() {
    let db = migrated().await;
    run(&db, &subscription("a", "gear", "i1", "k", "t"))
        .await
        .unwrap();
    assert!(
        run(&db, &subscription("b", "gear", "i1", "k", "t"))
            .await
            .is_err(),
        "uq_sub_subscriber_instance_key_tenant refuses the same instance twice"
    );
    run(&db, &subscription("c", "gear", "i2", "k", "t"))
        .await
        .expect("another instance of the subscriber is another subscription");
    run(&db, &subscription("d", "gear", "i1", "k", "other"))
        .await
        .expect("another tenant is another subscription");
    run(&db, &subscription("e", "other-gear", "i1", "k", "t"))
        .await
        .expect("another subscriber is another subscription");
    let indexes = indexes_of(&db, "subscriptions").await;
    for expected in ["idx_sub_key", "idx_sub_silent"] {
        assert!(indexes.contains(&expected.to_owned()), "{indexes:?}");
    }
}

#[tokio::test]
async fn a_subscription_starts_not_silent_and_its_label_is_bounded() {
    let db = migrated().await;
    run(&db, &subscription("a", "gear", "i1", "k", "t"))
        .await
        .unwrap();
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT instance_label, silent_since FROM subscriptions;",
        ))
        .await
        .unwrap();
    for column in ["instance_label", "silent_since"] {
        assert_eq!(
            rows[0].try_get::<Option<String>>("", column).unwrap(),
            None,
            "{column} is nullable and starts absent"
        );
    }
    let label = |id: &str, instance: &str, label: &str| {
        format!(
            "INSERT INTO subscriptions (id, subscriber, instance_id, instance_label, key, tenant,
                                        created_at)
             VALUES ('{id}', 'gear', '{instance}', '{label}', 'k', 't', 'now');"
        )
    };
    run(&db, &label("b", "i2", &"\u{e9}".repeat(128)))
        .await
        .expect("128 characters fit, whatever their bytes");
    assert!(
        run(&db, &label("c", "i3", &"x".repeat(129))).await.is_err(),
        "a label above 128 characters is refused"
    );
}

#[tokio::test]
async fn the_silent_index_is_partial_on_a_set_silent_since() {
    let db = migrated().await;
    let sql = index_sql(&db, "idx_sub_silent").await;
    assert!(
        sql.contains("(key, tenant, silent_since)")
            && sql.contains("WHERE silent_since IS NOT NULL"),
        "the index is partial over the silent subscriptions: {sql}"
    );
}

#[tokio::test]
async fn an_activation_state_is_unique_per_instance_awaiting_by_default_and_checked() {
    let db = migrated().await;
    run(&db, &state("gear", "i1", "k", "t", None))
        .await
        .unwrap();
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT status, deliveries, silent_since, applied_value, detail, reported_at,
                    last_notified_at
               FROM activation_state;",
        ))
        .await
        .unwrap();
    assert_eq!(
        rows[0].try_get::<String>("", "status").unwrap(),
        "awaiting",
        "the default status"
    );
    assert_eq!(
        rows[0].try_get::<i32>("", "deliveries").unwrap(),
        0,
        "no delivery counted yet"
    );
    for column in [
        "silent_since",
        "applied_value",
        "detail",
        "reported_at",
        "last_notified_at",
    ] {
        assert_eq!(
            rows[0].try_get::<Option<String>>("", column).unwrap(),
            None,
            "{column} is nullable and starts absent"
        );
    }
    assert!(
        run(&db, &state("gear", "i1", "k", "t", Some("failed")))
            .await
            .is_err(),
        "uq_activation_state keeps one row per instance's subscription"
    );
    run(&db, &state("gear", "i2", "k", "t", None))
        .await
        .expect("a sibling instance holds its own row");
    assert!(
        run(&db, &state("gear", "i1", "k", "u", Some("expired")))
            .await
            .is_err(),
        "a status outside the vocabulary is refused"
    );
    for status in ["succeeded", "failed", "silent"] {
        run(&db, &state("gear", "i1", status, "t", Some(status)))
            .await
            .unwrap_or_else(|e| panic!("`{status}` is in the vocabulary: {e}"));
    }
    assert!(
        run(
            &db,
            "INSERT INTO activation_state
               (subscriber, instance_id, key, tenant, change_ref, expected_value, updated_at)
             VALUES ('gear', 'i3', 'k', 't', 'ref', 'true', 'now');"
        )
        .await
        .is_err(),
        "`changed_at` has no default: the change sets it"
    );
    let indexes = indexes_of(&db, "activation_state").await;
    for expected in ["idx_activation_key_tenant", "idx_activation_awaiting"] {
        assert!(indexes.contains(&expected.to_owned()), "{indexes:?}");
    }
}

#[tokio::test]
async fn the_awaiting_index_is_partial_on_the_awaiting_status() {
    let db = migrated().await;
    let sql = index_sql(&db, "idx_activation_awaiting").await;
    assert!(
        sql.contains("WHERE status = 'awaiting'"),
        "the index is partial: {sql}"
    );
}

#[tokio::test]
async fn audit_records_take_activation_with_its_nullable_columns() {
    let db = migrated().await;
    run(&db, &audit("act", "activation")).await.unwrap();
    run(
        &db,
        "INSERT INTO audit_records (id, resource, declaration_key, tenant_id, operation, actor, \
         actor_classification, outcome, request_id, occurred_at, change_ref, detail, \
         instance_id, instance_label)
         VALUES ('with-ref', 'cf.settings:k@t', 'k', 't', 'activation', 'gear', 'public', \
         'failure', 'req', 'now', 'ref', 'could not apply', 'i1', 'host-1');",
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
    let sql = index_sql(&db, "idx_audit_change_ref").await;
    assert!(
        sql.contains("WHERE change_ref IS NOT NULL"),
        "the index is partial: {sql}"
    );
}

#[tokio::test]
async fn a_silent_outcome_is_taken_on_an_activation_record_only() {
    let db = migrated().await;
    run(&db, &audit_with_outcome("silent", "activation", "silent"))
        .await
        .expect("the record of a call of silence");
    assert!(
        run(&db, &audit_with_outcome("other", "change", "silent"))
            .await
            .is_err(),
        "`silent` is no outcome of any other operation"
    );
    assert!(
        run(&db, &audit_with_outcome("unknown", "activation", "lost"))
            .await
            .is_err(),
        "an outcome outside the vocabulary is still refused"
    );
}

#[tokio::test]
async fn the_rebuild_keeps_the_rows_and_the_append_only_trigger() {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    Migrator::up(&db, Some(before(ACTIVATION)))
        .await
        .expect("everything before the activation migration applies");
    run(&db, &audit("old", "change")).await.unwrap();

    Migrator::up(&db, None)
        .await
        .expect("the migration applies");

    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT id, change_ref, detail, instance_id, instance_label FROM audit_records;",
        ))
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "a record from before survives the rebuild");
    assert_eq!(rows[0].try_get::<String>("", "id").unwrap(), "old");
    for column in ["change_ref", "detail", "instance_id", "instance_label"] {
        assert_eq!(
            rows[0].try_get::<Option<String>>("", column).unwrap(),
            None,
            "{column} is absent on a record from before"
        );
    }
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
    run(&db, &subscription("a", "gear", "i1", "k", "t"))
        .await
        .unwrap();

    Migrator::down(&db, Some(from(ACTIVATION)))
        .await
        .expect("back to before the activation migration");

    assert!(
        run(&db, &subscription("b", "gear", "i1", "k", "u"))
            .await
            .is_err()
    );
    assert!(
        run(&db, &state("gear", "i1", "k", "t", None))
            .await
            .is_err()
    );
    assert!(
        run(&db, &audit("act", "activation")).await.is_err(),
        "an earlier build's vocabulary refuses `activation`"
    );
    assert!(
        run(&db, &audit_with_outcome("silent", "change", "silent"))
            .await
            .is_err(),
        "an earlier build's outcome rule refuses `silent`"
    );
    let kept = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT id FROM audit_records;",
        ))
        .await
        .unwrap();
    assert_eq!(kept.len(), 1, "the rollback keeps what was recorded");
    let columns = audit_columns(&db).await;
    for column in ["change_ref", "detail", "instance_id", "instance_label"] {
        assert!(
            !columns.contains(&column.to_owned()),
            "`audit_records.{column}` is dropped by the rollback: {columns:?}"
        );
    }
    let indexes = indexes_of(&db, "audit_records").await;
    for index in [
        "idx_audit_scoped",
        "idx_audit_retention",
        "idx_audit_default_horizon",
    ] {
        assert!(
            indexes.contains(&index.to_owned()),
            "the earlier `{index}` is recreated on the rebuilt table: {indexes:?}"
        );
    }
    assert!(
        !indexes.contains(&"idx_audit_change_ref".to_owned()),
        "{indexes:?}"
    );
    for index in [
        "idx_sub_key",
        "idx_sub_silent",
        "idx_activation_key_tenant",
        "idx_activation_awaiting",
    ] {
        assert!(
            names(
                &db,
                &format!("SELECT name FROM sqlite_master WHERE name = '{index}';")
            )
            .await
            .is_empty(),
            "`{index}` goes with its table"
        );
    }
    assert_eq!(
        names(
            &db,
            "SELECT name FROM sqlite_master
             WHERE type = 'trigger' AND tbl_name = 'audit_records';"
        )
        .await,
        ["trg_audit_records_append_only"],
        "the earlier append-only trigger is back on the rebuilt table, and it is the only one"
    );
    run(
        &db,
        "UPDATE audit_records SET actor = 'x' WHERE id = 'old';",
    )
    .await
    .expect_err("the append-only trigger survives the rollback rebuild");
    let kept = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT id, actor, operation, outcome FROM audit_records;",
        ))
        .await
        .unwrap();
    assert_eq!(kept.len(), 1, "the refused UPDATE changed nothing");
    assert_eq!(kept[0].try_get::<String>("", "actor").unwrap(), "who");

    Migrator::up(&db, None).await.expect("re-applied");
    run(&db, &audit("act", "activation")).await.unwrap();
    run(
        &db,
        &audit_with_outcome("act-silent", "activation", "silent"),
    )
    .await
    .unwrap();
}

/// The rollback is clean only while no `activation` record exists: the
/// rebuilt table's narrower `CHECK`s refuse the copy of one, so the step
/// fails, and the audit table it was rebuilding from is left as it was.
#[tokio::test]
async fn rolling_back_is_refused_while_an_activation_record_exists_and_keeps_the_audit_table() {
    let db = migrated().await;
    run(&db, &audit("old", "change")).await.unwrap();
    run(&db, &audit_with_outcome("act", "activation", "silent"))
        .await
        .unwrap();

    Migrator::down(&db, Some(from(ACTIVATION)))
        .await
        .expect_err("the narrowed checks refuse the activation record");

    let kept = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT id FROM audit_records ORDER BY id;",
        ))
        .await
        .unwrap();
    assert_eq!(
        kept.len(),
        2,
        "no audit row is lost by the refused rollback"
    );
    assert!(
        audit_columns(&db).await.contains(&"change_ref".to_owned()),
        "the audit table keeps its activation shape"
    );
    assert_eq!(
        names(
            &db,
            "SELECT name FROM sqlite_master WHERE name = 'subscriptions';"
        )
        .await
        .len(),
        1,
        "the tables are kept: the rebuild comes before the drops"
    );
}
