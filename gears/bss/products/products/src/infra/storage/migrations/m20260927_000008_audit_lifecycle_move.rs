//! P-D-213: an audit row carries the SKU lifecycle move its act made — `from_lifecycle` and
//! `to_lifecycle` on `products_audit_log`.
//!
//! A forward migration: the chain is deployed and no shipped migration is edited again. It adds
//! two nullable `text` columns, each with a named `CHECK` holding it to the five lifecycles, and
//! redefines the append-only guard of `m20260925_000004` so the one admitted `UPDATE` — the
//! platform's one-way seal — still requires every record column unchanged, the two new ones
//! included. Every other statement of the guard is unchanged: DELETE is refused, and so is every
//! UPDATE that is not the seal.
//!
//! - Postgres: `ADD COLUMN IF NOT EXISTS` twice, then `CREATE OR REPLACE FUNCTION
//!   bss.products_audit_log_append_only()` with the two columns added to the seal's
//!   unchanged-list. The trigger calls the function by name and is not touched.
//! - `SQLite`: `ADD COLUMN` for each column the table does not have yet (`SQLite` has no `IF NOT
//!   EXISTS` there, and `up` must replay), then `trg_products_audit_log_seal_unchanged` dropped
//!   and created again with the two columns in its unchanged-list. `trg_products_audit_log_no_delete`
//!   and `trg_products_audit_log_no_update` are not touched.
//!
//! A row written before this migration reads `NULL` for both: the act's move was not recorded.
//! No row is backfilled — the table is append-only, and no guess is made about a past move.
//!
//! `down()` is irreversible: dropping the columns would erase recorded moves from an append-only
//! record.
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// The two columns, each with its named `CHECK`, as both dialects spell them after `ADD COLUMN`.
const COLUMNS: [(&str, &str); 2] = [
    (
        "from_lifecycle",
        "from_lifecycle text CONSTRAINT chk_products_audit_log_from_lifecycle CHECK \
         (from_lifecycle IN ('draft','published','deprecated','retiring','retired'))",
    ),
    (
        "to_lifecycle",
        "to_lifecycle text CONSTRAINT chk_products_audit_log_to_lifecycle CHECK \
         (to_lifecycle IN ('draft','published','deprecated','retiring','retired'))",
    ),
];

/// `000004`'s function with `from_lifecycle` and `to_lifecycle` in the seal's unchanged-list.
const PG_FUNCTION: &str =
    "CREATE OR REPLACE FUNCTION bss.products_audit_log_append_only() RETURNS trigger AS $$
        BEGIN
          IF TG_OP = 'DELETE' THEN
            RAISE EXCEPTION 'products_audit_log is append-only: DELETE is not permitted';
          END IF;

          IF OLD.seal_state = 'unsealed'
             AND NEW.seal_state = 'sealed'
             AND NEW.chain_id IS NOT NULL
             AND NEW.seq IS NOT NULL
             AND NEW.row_hash IS NOT NULL
             AND NEW.audit_id IS NOT DISTINCT FROM OLD.audit_id
             AND NEW.tenant_id IS NOT DISTINCT FROM OLD.tenant_id
             AND NEW.actor_ref IS NOT DISTINCT FROM OLD.actor_ref
             AND NEW.action IS NOT DISTINCT FROM OLD.action
             AND NEW.subject_kind IS NOT DISTINCT FROM OLD.subject_kind
             AND NEW.subject_id IS NOT DISTINCT FROM OLD.subject_id
             AND NEW.subject_revision IS NOT DISTINCT FROM OLD.subject_revision
             AND NEW.error_code IS NOT DISTINCT FROM OLD.error_code
             AND NEW.attempted_key IS NOT DISTINCT FROM OLD.attempted_key
             AND NEW.reason IS NOT DISTINCT FROM OLD.reason
             AND NEW.correlation_id IS NOT DISTINCT FROM OLD.correlation_id
             AND NEW.written_at IS NOT DISTINCT FROM OLD.written_at
             AND NEW.session_id IS NOT DISTINCT FROM OLD.session_id
             AND NEW.ceremony_ref IS NOT DISTINCT FROM OLD.ceremony_ref
             AND NEW.from_lifecycle IS NOT DISTINCT FROM OLD.from_lifecycle
             AND NEW.to_lifecycle IS NOT DISTINCT FROM OLD.to_lifecycle
          THEN
            RETURN NEW;
          END IF;

          RAISE EXCEPTION 'products_audit_log is append-only: % is not permitted', TG_OP;
        END;
     $$ LANGUAGE plpgsql";

/// `000004`'s seal trigger with `from_lifecycle` and `to_lifecycle` in its unchanged-list.
const SQLITE_SEAL_TRIGGER: [&str; 2] = [
    "DROP TRIGGER IF EXISTS trg_products_audit_log_seal_unchanged",
    "CREATE TRIGGER trg_products_audit_log_seal_unchanged BEFORE UPDATE ON products_audit_log FOR EACH ROW WHEN (
            OLD.seal_state IS 'unsealed'
            AND NEW.seal_state IS 'sealed'
            AND NEW.chain_id IS NOT NULL
            AND NEW.seq IS NOT NULL
            AND NEW.row_hash IS NOT NULL
        ) AND NOT (
            NEW.audit_id IS OLD.audit_id
            AND NEW.tenant_id IS OLD.tenant_id
            AND NEW.actor_ref IS OLD.actor_ref
            AND NEW.action IS OLD.action
            AND NEW.subject_kind IS OLD.subject_kind
            AND NEW.subject_id IS OLD.subject_id
            AND NEW.subject_revision IS OLD.subject_revision
            AND NEW.error_code IS OLD.error_code
            AND NEW.attempted_key IS OLD.attempted_key
            AND NEW.reason IS OLD.reason
            AND NEW.correlation_id IS OLD.correlation_id
            AND NEW.written_at IS OLD.written_at
            AND NEW.session_id IS OLD.session_id
            AND NEW.ceremony_ref IS OLD.ceremony_ref
            AND NEW.from_lifecycle IS OLD.from_lifecycle
            AND NEW.to_lifecycle IS OLD.to_lifecycle
        ) BEGIN SELECT RAISE(ABORT, 'products_audit_log is append-only: UPDATE is not permitted'); END",
];

/// The statements for this database: on `SQLite`, an `ADD COLUMN` only for a column the table
/// does not have yet, so a replay adds nothing.
async fn statements(manager: &SchemaManager<'_>) -> Result<(Vec<String>, Vec<String>), DbErr> {
    let pg: Vec<String> = COLUMNS
        .iter()
        .map(|(_, spec)| {
            format!("ALTER TABLE bss.products_audit_log ADD COLUMN IF NOT EXISTS {spec}")
        })
        .chain(std::iter::once(PG_FUNCTION.to_owned()))
        .collect();
    let mut sqlite = Vec::new();
    if manager.get_database_backend() == DatabaseBackend::Sqlite {
        let present: Vec<String> = manager
            .get_connection()
            .query_all_raw(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT name AS v FROM pragma_table_info('products_audit_log')".to_owned(),
            ))
            .await?
            .iter()
            .map(|row| row.try_get::<String>("", "v"))
            .collect::<Result<_, _>>()?;
        sqlite.extend(
            COLUMNS
                .iter()
                .filter(|(name, _)| !present.iter().any(|p| p == name))
                .map(|(_, spec)| format!("ALTER TABLE products_audit_log ADD COLUMN {spec}")),
        );
        sqlite.extend(SQLITE_SEAL_TRIGGER.map(str::to_owned));
    }
    Ok((pg, sqlite))
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let (pg, sqlite) = statements(manager).await?;
        let pg: Vec<&str> = pg.iter().map(String::as_str).collect();
        let sqlite: Vec<&str> = sqlite.iter().map(String::as_str).collect();
        super::exec_backend(self.name(), manager, &pg, &sqlite).await
    }

    /// Irreversible: dropping the columns would erase the recorded moves from an append-only
    /// record.
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(format!(
            "{}: irreversible \u{2014} the audit log is append-only, and dropping from_lifecycle \
             and to_lifecycle would erase the lifecycle moves it recorded (P-D-213)",
            self.name()
        )))
    }
}

#[cfg(test)]
#[path = "m20260927_000008_audit_lifecycle_move_tests.rs"]
mod tests;
