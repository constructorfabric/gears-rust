use sea_orm_migration::{MigratorTrait, SchemaManager};

use super::Migrator;

const TOOLKIT_OUTBOX_MIGRATION: &str =
    "m001_create_toolkit_outbox_schema__event_broker__ob_default";

/// The `default` outbox group's tables come from the declared migrations
/// under its fixed prefix, never from toolkit-db's unprefixed default.
#[test]
fn the_outbox_tables_are_declared_under_the_default_groups_prefix() {
    let names: Vec<String> = Migrator::migrations()
        .iter()
        .map(|migration| migration.name().to_owned())
        .collect();
    assert_eq!(
        names,
        vec![
            "m20260819_000001_spec_cache",
            "m20260819_000002_storage_facade",
            "m20260819_000003_backend_binding",
            TOOLKIT_OUTBOX_MIGRATION,
        ]
    );
}

/// `(name, declared type, notnull, pk)` of every column of `table`, in
/// declaration order, as `SQLite` reports them.
async fn table_info(
    conn: &sea_orm::DatabaseConnection,
    table: &str,
) -> Vec<(String, String, bool, i64)> {
    use sea_orm::{ConnectionTrait, Statement};

    conn.query_all_raw(Statement::from_string(
        sea_orm::DbBackend::Sqlite,
        format!("PRAGMA table_info({table})"),
    ))
    .await
    .expect("read the table's columns")
    .into_iter()
    .map(|row| {
        (
            row.try_get::<String>("", "name").expect("name"),
            row.try_get::<String>("", "type").expect("type"),
            row.try_get::<i64>("", "notnull").expect("notnull") == 1,
            row.try_get::<i64>("", "pk").expect("pk"),
        )
    })
    .collect()
}

async fn column_info(
    conn: &sea_orm::DatabaseConnection,
    table: &str,
    column: &str,
) -> Vec<(String, String, bool, i64)> {
    table_info(conn, table)
        .await
        .into_iter()
        .filter(|(name, ..)| name == column)
        .collect()
}

fn col(name: &str, ty: &str, notnull: bool, pk: i64) -> (String, String, bool, i64) {
    (name.to_owned(), ty.to_owned(), notnull, pk)
}

/// UUIDs and timestamps are native and strings are sized from the values
/// written to them (`columns.rs`), so on `SQLite` every column of the backend
/// binding tables, and every enum column, declares exactly this.
#[tokio::test]
async fn the_binding_and_identity_tables_have_typed_columns() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let dsn = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("schema.db").to_string_lossy()
    );
    let db = toolkit_db::connect_db(&dsn, toolkit_db::ConnectOpts::default())
        .await
        .expect("connect sqlite");
    toolkit_db::migration_runner::run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("migrations");
    let conn = sea_orm::Database::connect(&dsn)
        .await
        .expect("connect sqlite");

    assert_eq!(
        table_info(&conn, "event_broker__topic_binding").await,
        vec![
            col("topic_uuid", "uuid_text", true, 1),
            col("topic_id", "varchar(1024)", true, 0),
            col("log_id", "uuid_text", true, 0),
            col("state", "varchar(12)", true, 0),
            col("attempt_at", "varchar(24)", true, 0),
            col("partitions", "INTEGER", false, 0),
            col("retention_max_age_ms", "INTEGER", false, 0),
            col("retention_max_bytes", "INTEGER", false, 0),
            col("retention_version", "INTEGER", true, 0),
            col("retention_confirmed", "boolean", true, 0),
        ]
    );
    assert_eq!(
        table_info(&conn, "event_broker__outbox_identity").await,
        vec![
            col("name", "varchar(63)", true, 1),
            col("outbox_id", "varchar(16)", true, 0),
            col("created_at", "timestamp_with_timezone_text", true, 0),
        ]
    );
    assert_eq!(
        column_info(&conn, "event_broker_spec_cache", "kind").await,
        vec![col("kind", "varchar(10)", true, 0)]
    );
    assert_eq!(
        column_info(&conn, "event_broker_consumer_group", "kind").await,
        vec![col("kind", "varchar(9)", true, 0)]
    );
    assert_eq!(
        column_info(&conn, "event_broker_producer", "mode").await,
        vec![col("mode", "varchar(9)", true, 0)]
    );
}

/// `sql` as its statements, with comments stripped, whitespace collapsed and
/// no space inside parentheses, so the document's layout is free.
fn normalised_statements(sql: &str) -> Vec<String> {
    let uncommented: String = sql
        .lines()
        .map(|line| line.split_once("--").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join(" ");
    uncommented
        .split(';')
        .map(|stmt| {
            stmt.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .replace("( ", "(")
                .replace(" )", ")")
        })
        .filter(|stmt| !stmt.is_empty())
        .collect()
}

/// The document is the gear's schema on `PostgreSQL`, so it must equal what
/// every migration the gear declares emits there, index for index. The
/// toolkit outbox tables are toolkit-db's schema, not the document's. Each
/// index is created after a `has_index` probe; the mock answers "absent", as
/// a fresh database does, and the probes are not DDL. The mock's fixed answer
/// counts fail the run loudly when the migrations need more of them.
#[tokio::test]
async fn migration_sql_matches_the_gear_migrations_on_postgres() {
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    let absent = BTreeMap::from([("has_index".to_owned(), Value::Bool(Some(false)))]);
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(std::iter::repeat_n(vec![absent], 16))
        .append_exec_results(std::iter::repeat_n(
            MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            },
            32,
        ))
        .into_connection();
    let manager = SchemaManager::new(&db);
    let (toolkit, gear): (Vec<_>, Vec<_>) = Migrator::migrations()
        .into_iter()
        .partition(|migration| migration.name() == TOOLKIT_OUTBOX_MIGRATION);
    let toolkit_names: Vec<&str> = toolkit.iter().map(|migration| migration.name()).collect();
    assert_eq!(toolkit_names, vec![TOOLKIT_OUTBOX_MIGRATION]);
    for migration in &gear {
        migration
            .up(&manager)
            .await
            .unwrap_or_else(|err| panic!("{}: {err}", migration.name()));
    }

    let emitted: Vec<String> = db
        .into_transaction_log()
        .iter()
        .flat_map(sea_orm::Transaction::statements)
        .filter(|stmt| !stmt.sql.starts_with("SELECT"))
        .flat_map(|stmt| normalised_statements(&stmt.sql))
        .collect();
    assert_eq!(
        emitted,
        normalised_statements(include_str!("../../../../../docs/migration.sql"))
    );
}
