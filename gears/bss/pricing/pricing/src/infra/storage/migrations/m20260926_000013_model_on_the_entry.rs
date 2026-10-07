//! D-427: the pricing model belongs to the price book entry, fixed for its life and part of its
//! key. A FORWARD migration (D-412 is closed: the chain is deployed and no shipped migration is
//! edited again).
//!
//! It runs inside the toolkit runner's transaction on both dialects, so it uses no PRAGMA and no
//! table rebuild: `ADD COLUMN` on the entry and `DROP COLUMN` on the price.
//!
//! 0. On Postgres, lock both tables ACCESS EXCLUSIVE before anything else: the check and the
//!    backfill are separate statements of a READ COMMITTED transaction, so without the lock a
//!    price in a second model committed between them would be backfilled away by `min()` instead
//!    of refused. A writer still open waits here, and its price, once committed, meets the check.
//!    `SQLite`'s writer lock already serialises the transaction.
//! 1. Refuse, naming them, the entries whose prices of ANY state (draft, pending, approved,
//!    rejected) carry two or more models: which one the entry keeps is the owner's call. The check
//!    runs before any statement that changes the schema, so a refusal changes nothing.
//! 2. Add `pricing_price_book_entry.model`. `SQLite` adds it `NOT NULL DEFAULT 'flat'` with its
//!    CHECK in one statement (a `NOT NULL` column needs a default there; the default stays in the
//!    schema and no writer relies on it). Postgres adds it nullable, then sets it `NOT NULL` after
//!    the backfill and adds the named CHECK `pricing_price_book_entry_model_check`.
//! 3. Backfill each entry with the one model of all its prices, or, with no price, its charge
//!    kind's default (`domain::price_book_entry::default_model`: per unit for usage, flat for
//!    recurring and one-time).
//! 4. Recreate the key index `pricing_price_book_entry_key` (name kept) with `model` last.
//! 5. Drop `pricing_price.model` (and with it its CHECK); every other index and CHECK of the
//!    table stays, which the schema goldens and the upgrade tests show.
//!
//! `down` refuses: a data move is not reversed by a migration.
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sea_orm_migration::prelude::*;
use uuid::Uuid;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Postgres: the migration's first statement, so the two-model check and the backfill judge the
/// same prices (the review's migrations lens, L1).
const PG_LOCK: &str =
    "LOCK TABLE bss.pricing_price_book_entry, bss.pricing_price IN ACCESS EXCLUSIVE MODE";

const PG_UP: &[&str] = &[
    r"ALTER TABLE bss.pricing_price_book_entry ADD COLUMN model text",
    r"UPDATE bss.pricing_price_book_entry SET model = coalesce(
  (SELECT min(p.model) FROM bss.pricing_price p
    WHERE p.price_book_entry_id = pricing_price_book_entry.id
      AND p.tenant_id = pricing_price_book_entry.tenant_id),
  CASE charge_kind WHEN 'usage' THEN 'per_unit' ELSE 'flat' END)",
    r"ALTER TABLE bss.pricing_price_book_entry ALTER COLUMN model SET NOT NULL",
    r"ALTER TABLE bss.pricing_price_book_entry ADD CONSTRAINT pricing_price_book_entry_model_check
  CHECK (model IN ('flat','per_unit','graduated','volume','package'))",
    r"DROP INDEX bss.pricing_price_book_entry_key",
    r"CREATE UNIQUE INDEX pricing_price_book_entry_key ON bss.pricing_price_book_entry (book_id, sku_id, charge_kind, coalesce(period, ''), model)",
    r"ALTER TABLE bss.pricing_price DROP COLUMN model",
];
// A `Uuid` is a 16-byte blob on `SQLite`: the backfill's join compares the application's own
// blobs on both sides, never a blob with text.
const SQLITE_UP: &[&str] = &[
    r"ALTER TABLE pricing_price_book_entry ADD COLUMN model text NOT NULL DEFAULT 'flat' CHECK (model IN ('flat','per_unit','graduated','volume','package'))",
    r"UPDATE pricing_price_book_entry SET model = coalesce(
  (SELECT min(p.model) FROM pricing_price p
    WHERE p.price_book_entry_id = pricing_price_book_entry.id
      AND p.tenant_id = pricing_price_book_entry.tenant_id),
  CASE charge_kind WHEN 'usage' THEN 'per_unit' ELSE 'flat' END)",
    r"DROP INDEX pricing_price_book_entry_key",
    r"CREATE UNIQUE INDEX pricing_price_book_entry_key ON pricing_price_book_entry (book_id, sku_id, charge_kind, coalesce(period, ''), model)",
    r"ALTER TABLE pricing_price DROP COLUMN model",
];

/// The entries whose prices, in any state, carry more than one model, as hyphenated ids.
async fn entries_with_two_models(manager: &SchemaManager<'_>) -> Result<Vec<String>, DbErr> {
    let backend = manager.get_database_backend();
    let sql = match backend {
        DatabaseBackend::Sqlite => {
            "SELECT CASE typeof(price_book_entry_id) WHEN 'blob' THEN lower(hex(price_book_entry_id)) \
             ELSE price_book_entry_id END AS v FROM pricing_price GROUP BY price_book_entry_id \
             HAVING count(DISTINCT model) > 1 ORDER BY price_book_entry_id"
        }
        DatabaseBackend::Postgres => {
            "SELECT price_book_entry_id::text AS v FROM bss.pricing_price GROUP BY \
             price_book_entry_id HAVING count(DISTINCT model) > 1 ORDER BY price_book_entry_id"
        }
        _ => {
            return Err(DbErr::Migration(format!(
                "{backend:?} is not a supported backend for bss-pricing"
            )));
        }
    };
    manager
        .get_connection()
        .query_all_raw(Statement::from_string(backend, sql.to_owned()))
        .await?
        .iter()
        .map(|row| {
            row.try_get::<String>("", "v")
                .map(|id| Uuid::parse_str(&id).map_or(id, |parsed| parsed.to_string()))
        })
        .collect()
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() == DatabaseBackend::Postgres {
            super::exec_backend(self.name(), manager, &[PG_LOCK], &[]).await?;
        }
        let conflicts = entries_with_two_models(manager).await?;
        if !conflicts.is_empty() {
            return Err(DbErr::Migration(format!(
                "{}: the prices of {} {} carry more than one model, so the model cannot move to \
                 the entry (D-427); nothing was changed, and which model each entry keeps is the \
                 owner's call",
                self.name(),
                if conflicts.len() == 1 {
                    "entry"
                } else {
                    "entries"
                },
                conflicts.join(", ")
            )));
        }
        super::exec_backend(self.name(), manager, PG_UP, SQLITE_UP).await
    }

    /// Irreversible by name: the chain test's one named exception.
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(format!(
            "{}: irreversible — the model moved from the price to its entry (D-427); a database \
             goes back only through a backup taken before it",
            self.name()
        )))
    }
}
