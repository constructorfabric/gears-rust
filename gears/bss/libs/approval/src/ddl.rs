//! The four approval tables as raw SQL for a gear to splice into its migration chain, and the
//! later `submit_note` column a gear adds by its own forward migration ([`add_submit_note`]).
use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement};
use sea_orm_migration::SchemaManager;

/// Qualifies a trusted migration identifier.
fn t(prefix: &str, schema: Option<&str>, name: &str) -> String {
    schema.map_or_else(
        || format!("{prefix}{name}"),
        |s| format!("{s}.{prefix}{name}"),
    )
}

/// `UP` statements for `PostgreSQL` or `SQLite`; each uses `IF NOT EXISTS`.
///
/// `prefix` and `schema` must be trusted, unquoted SQL identifiers from the gear's
/// migration code, never request input. The schema must already exist on `PostgreSQL`.
/// `SQLite` ignores `schema`.
#[must_use]
pub fn up(prefix: &str, schema: Option<&str>, backend: DbBackend) -> Vec<String> {
    let pg = backend == DbBackend::Postgres;
    let (uuid, ts, date, json, boolean, bigint, false_lit, btree) = if pg {
        (
            "uuid",
            "timestamptz",
            "date",
            "jsonb",
            "boolean",
            "bigint",
            "false",
            " USING btree",
        )
    } else {
        (
            "text", "text", "text", "text", "integer", "integer", "0", "",
        )
    };
    let schema = if pg { schema } else { None };
    let policy = t(prefix, schema, "approval_policy");
    let unit = t(prefix, schema, "approval_unit");
    let item = t(prefix, schema, "approval_unit_item");
    let decision = t(prefix, schema, "approval_decision");
    vec![
        format!(
            "CREATE TABLE IF NOT EXISTS {policy} (tenant_id {uuid} NOT NULL, kind text NOT NULL, quorum integer NOT NULL CHECK (quorum >= 0), PRIMARY KEY (tenant_id, kind))"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS {unit} (id {uuid} PRIMARY KEY, tenant_id {uuid} NOT NULL, kind text NOT NULL, ref_type text NOT NULL, ref_id {uuid} NOT NULL, state text NOT NULL CHECK (state IN ('pending','approved','rejected','withdrawn')), common_effective_date {date} NULL, quorum_required integer NOT NULL, generation integer NOT NULL DEFAULT 1, submitted_by {uuid} NOT NULL, submitted_at {ts} NOT NULL, decided_at {ts} NULL, decided_note text NULL, snapshot {json} NOT NULL, snapshot_hash text NOT NULL, version {bigint} NOT NULL DEFAULT 1)"
        ),
        format!(
            "CREATE INDEX IF NOT EXISTS ix_{prefix}approval_unit_queue ON {unit}{btree} (tenant_id, state, kind, submitted_at)"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS {item} (unit_id {uuid} NOT NULL REFERENCES {unit}(id), tenant_id {uuid} NOT NULL, item_type text NOT NULL, item_id {uuid} NOT NULL, created_by {uuid} NOT NULL, before_json {json} NULL, after_json {json} NOT NULL, PRIMARY KEY (unit_id, item_type, item_id))"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS {decision} (unit_id {uuid} NOT NULL REFERENCES {unit}(id), tenant_id {uuid} NOT NULL, actor {uuid} NOT NULL, generation integer NOT NULL, decision text NOT NULL CHECK (decision IN ('approve','reject')), note text NULL, at {ts} NOT NULL, stale {boolean} NOT NULL DEFAULT {false_lit}, PRIMARY KEY (unit_id, actor, generation))"
        ),
    ]
}

/// Drops tables in foreign-key order, qualifying names with `schema` when supplied.
///
/// Identifiers follow the same trusted-input contract as [`up`]. Pass `None` for
/// `SQLite`; [`apply_down`] handles that backend choice automatically.
#[must_use]
pub fn down(prefix: &str, schema: Option<&str>) -> Vec<String> {
    [
        "approval_decision",
        "approval_unit_item",
        "approval_unit",
        "approval_policy",
    ]
    .iter()
    .map(|n| format!("DROP TABLE IF EXISTS {}", t(prefix, schema, n)))
    .collect()
}

/// The unit table's `submit_note` column (products P-D-219, pricing D-445): the `ALTER TABLE …
/// ADD COLUMN submit_note text` a gear's own FORWARD migration runs. It is not part of [`up`],
/// which is the body of migrations already deployed and stays as it shipped, so a fresh chain
/// and an upgraded one both get the column from that later migration.
///
/// Postgres says `IF NOT EXISTS`; `SQLite` has no such clause there, so
/// [`apply_add_submit_note`] reads the catalog first. Identifiers follow [`up`]'s contract.
#[must_use]
pub fn add_submit_note(prefix: &str, schema: Option<&str>, backend: DbBackend) -> String {
    if backend == DbBackend::Postgres {
        let unit = t(prefix, schema, "approval_unit");
        format!("ALTER TABLE {unit} ADD COLUMN IF NOT EXISTS submit_note text")
    } else {
        let unit = t(prefix, None, "approval_unit");
        format!("ALTER TABLE {unit} ADD COLUMN submit_note text")
    }
}

/// The reverse of [`add_submit_note`]: `DROP COLUMN submit_note` (`IF EXISTS` on Postgres).
#[must_use]
pub fn drop_submit_note(prefix: &str, schema: Option<&str>, backend: DbBackend) -> String {
    if backend == DbBackend::Postgres {
        let unit = t(prefix, schema, "approval_unit");
        format!("ALTER TABLE {unit} DROP COLUMN IF EXISTS submit_note")
    } else {
        let unit = t(prefix, None, "approval_unit");
        format!("ALTER TABLE {unit} DROP COLUMN submit_note")
    }
}

/// Whether the unit table has `submit_note`, on `SQLite`.
async fn sqlite_has_submit_note(manager: &SchemaManager<'_>, prefix: &str) -> Result<bool, DbErr> {
    let rows = manager
        .get_connection()
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            format!(
                "SELECT name AS v FROM pragma_table_info('{prefix}approval_unit') \
                 WHERE name = 'submit_note'"
            ),
        ))
        .await?;
    Ok(!rows.is_empty())
}

/// Runs [`add_submit_note`] for the manager's backend; on `SQLite` only when the column is
/// missing, so a replay changes nothing. A gear calls this from its own forward migration.
///
/// # Errors
/// Returns the database error; the gear owns the migration transaction.
pub async fn apply_add_submit_note(
    manager: &SchemaManager<'_>,
    prefix: &str,
    schema: Option<&str>,
) -> Result<(), DbErr> {
    let backend = manager.get_database_backend();
    if backend == DbBackend::Sqlite && sqlite_has_submit_note(manager, prefix).await? {
        return Ok(());
    }
    manager
        .get_connection()
        .execute_raw(Statement::from_string(
            backend,
            add_submit_note(prefix, schema, backend),
        ))
        .await?;
    Ok(())
}

/// Runs [`drop_submit_note`] for the manager's backend; on `SQLite` only when the column is
/// there, so a replay changes nothing.
///
/// # Errors
/// Returns the database error; the gear owns the migration transaction.
pub async fn apply_drop_submit_note(
    manager: &SchemaManager<'_>,
    prefix: &str,
    schema: Option<&str>,
) -> Result<(), DbErr> {
    let backend = manager.get_database_backend();
    if backend == DbBackend::Sqlite && !sqlite_has_submit_note(manager, prefix).await? {
        return Ok(());
    }
    manager
        .get_connection()
        .execute_raw(Statement::from_string(
            backend,
            drop_submit_note(prefix, schema, backend),
        ))
        .await?;
    Ok(())
}

/// Runs [`up`] for the manager's backend. A gear calls this from its own migration's `up`.
///
/// # Errors
/// Returns the first database error; the gear owns the migration transaction.
pub async fn apply_up(
    manager: &SchemaManager<'_>,
    prefix: &str,
    schema: Option<&str>,
) -> Result<(), DbErr> {
    let backend = manager.get_database_backend();
    for sql in up(prefix, schema, backend) {
        manager
            .get_connection()
            .execute_raw(Statement::from_string(backend, sql))
            .await?;
    }
    Ok(())
}

/// Runs [`down`] for the manager's backend, ignoring the schema on `SQLite`.
///
/// # Errors
/// Returns the first database error; the gear owns the migration transaction.
pub async fn apply_down(
    manager: &SchemaManager<'_>,
    prefix: &str,
    schema: Option<&str>,
) -> Result<(), DbErr> {
    let backend = manager.get_database_backend();
    let schema = if backend == DbBackend::Postgres {
        schema
    } else {
        None
    };
    for sql in down(prefix, schema) {
        manager
            .get_connection()
            .execute_raw(Statement::from_string(backend, sql))
            .await?;
    }
    Ok(())
}
