//! Raw access to `credstore_secrets` for both supported backends.

use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, Value};
use uuid::Uuid;

use crate::error::MigrationError;

/// Which of the two schema generations a database is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schema {
    /// Shipped `m0001`: has `value_fp`, no `value_version`.
    Shipped,
    /// After `m0002`: has `value_version`, no `value_fp`.
    ValueVersions,
}

pub fn backend(db: &DatabaseConnection) -> Result<DatabaseBackend, MigrationError> {
    match db.get_database_backend() {
        b @ (DatabaseBackend::Postgres | DatabaseBackend::Sqlite) => Ok(b),
        _ => Err(MigrationError::UnsupportedBackend),
    }
}

/// Positional placeholder `n` (1-based) for `backend`.
pub fn ph(backend: DatabaseBackend, n: usize) -> String {
    match backend {
        DatabaseBackend::Postgres => format!("${n}"),
        _ => format!("?{n}"),
    }
}

pub fn stmt(backend: DatabaseBackend, sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(backend, sql, values)
}

async fn has_column(
    db: &DatabaseConnection,
    backend: DatabaseBackend,
    column: &str,
) -> Result<bool, MigrationError> {
    let sql = match backend {
        DatabaseBackend::Postgres => {
            "SELECT 1 AS present FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = 'credstore_secrets' \
             AND column_name = $1"
        }
        _ => "SELECT 1 AS present FROM pragma_table_info('credstore_secrets') WHERE name = ?1",
    };
    let row = db
        .query_one_raw(stmt(backend, sql, vec![column.into()]))
        .await?;
    Ok(row.is_some())
}

/// Determines the schema generation, failing when the table is missing or in
/// neither known shape.
pub async fn detect_schema(
    db: &DatabaseConnection,
    backend: DatabaseBackend,
) -> Result<Schema, MigrationError> {
    let fp = has_column(db, backend, "value_fp").await?;
    let version = has_column(db, backend, "value_version").await?;
    match (fp, version) {
        (true, false) => Ok(Schema::Shipped),
        (false, true) => Ok(Schema::ValueVersions),
        (true, true) => Err(MigrationError::WrongSchema(
            "credstore_secrets has both value_fp and value_version: m0002 did not complete"
                .to_owned(),
        )),
        (false, false) => Err(MigrationError::WrongSchema(
            "credstore_secrets is missing or has neither value_fp nor value_version".to_owned(),
        )),
    }
}

/// A row of the shipped `credstore_secrets`.
#[derive(Debug, Clone)]
pub struct ShippedRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub reference: String,
    pub sharing: i16,
    pub owner_id: Uuid,
    pub status: i16,
    pub value_fp: Option<Vec<u8>>,
    pub fp_key_id: Option<i16>,
    pub secret_type_uuid: Uuid,
}

pub async fn load_shipped_rows(
    db: &DatabaseConnection,
    backend: DatabaseBackend,
) -> Result<Vec<ShippedRow>, MigrationError> {
    let sql = "SELECT id, tenant_id, reference, sharing, owner_id, status, value_fp, \
               fp_key_id, secret_type_uuid FROM credstore_secrets \
               ORDER BY tenant_id, reference, id";
    let rows = db.query_all_raw(stmt(backend, sql, vec![])).await?;
    rows.iter()
        .map(|r| {
            Ok(ShippedRow {
                id: r.try_get("", "id")?,
                tenant_id: r.try_get("", "tenant_id")?,
                reference: r.try_get("", "reference")?,
                sharing: r.try_get("", "sharing")?,
                owner_id: r.try_get("", "owner_id")?,
                status: r.try_get("", "status")?,
                value_fp: r.try_get("", "value_fp")?,
                fp_key_id: r.try_get("", "fp_key_id")?,
                secret_type_uuid: r.try_get("", "secret_type_uuid")?,
            })
        })
        .collect()
}
