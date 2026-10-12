//! D-427 on `SQLite`: the forward migration `m20260926_000013_model_on_the_entry` moves the pricing
//! model from the price to the entry, inside the toolkit runner's transaction.
//!
//! A file database is migrated by the gear's whole list without 000013 — the chain the deployment runs
//! today — and seeded with what a live database holds: a book; five entries (a usage entry priced
//! graduated in three states, a recurring flat entry with an approved and a pending price, a
//! one-time and a usage entry without prices, a recurring entry priced per unit); a pending
//! approval unit; a plan with a draft revision and three items (two naming entries, one included);
//! and reference ops whose stored create input predates 000013 (no `model`): a done create, a
//! create still reserving with its receipt and its Idempotency-Key claim, and a rereserve. Rows are
//! written with the encodings the application writes (a `Uuid` is a 16-byte blob on `SQLite`), so
//! the backfill's join runs on real data, and its effect is read back through the application's
//! own door and machine, not only as a column. Then the whole list runs again through
//! `run_migrations_for_testing`, which applies 000013 alone.
//!
//! The proof of the structure is the schema dump of `schema_dump` (columns, foreign keys, index
//! DDL) plus the `sqlite_master` text of the two tables, the only place `SQLite` keeps a CHECK.
//! The Postgres twin is `postgres_entry_model_migration.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod entry_support;
mod schema_dump;

use bss_pricing::infra::reference_ticker::system_actor;
use bss_pricing::infra::reference_work::{self, Caller, WallClock};
use bss_pricing::infra::storage::entity::reference_op;
use bss_pricing::infra::storage::repo::{idempotency_repo as idem, reference_op_repo};
use bss_pricing::module::BssPricingGear;
use entry_support::{Script, app_for, request, state_on, user_of};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use uuid::Uuid;

const MIGRATION: &str = "m20260926_000013_model_on_the_entry";
const GUARD: &str = "m0000_pricing_refuse_a_legacy_or_stale_schema";

const TENANT: Uuid = Uuid::from_u128(0x7e0a_0000_0000_0000_0000_0000_0000_0001);
const AUTHOR: Uuid = Uuid::from_u128(0xa0);
const BOOK: Uuid = Uuid::from_u128(0xb0);
/// Usage, priced graduated: approved, draft and rejected.
const STORAGE: Uuid = Uuid::from_u128(0xe1);
/// Recurring monthly, priced flat: approved and pending.
const SEATS: Uuid = Uuid::from_u128(0xe2);
/// One-time, no price.
const SETUP: Uuid = Uuid::from_u128(0xe3);
/// Usage, no price.
const CALLS: Uuid = Uuid::from_u128(0xe4);
/// Recurring yearly, priced per unit.
const SUPPORT: Uuid = Uuid::from_u128(0xe5);
/// The entry the create op still reserving will write.
const LATE: Uuid = Uuid::from_u128(0xe6);
const UNIT: Uuid = Uuid::from_u128(0x401);
const PLAN: Uuid = Uuid::from_u128(0x501);
const REVISION: Uuid = Uuid::from_u128(0x601);
const OP_DONE: Uuid = Uuid::from_u128(0x801);
const OP_CREATE: Uuid = Uuid::from_u128(0x802);
const OP_REREREVE: Uuid = Uuid::from_u128(0x803);
const KEY: &str = "k-before";
/// The entry a create stored before 000013 would write for STORAGE's key (its op, its key).
const TWIN: Uuid = Uuid::from_u128(0xe7);
const OP_TWIN: Uuid = Uuid::from_u128(0x804);
const TWIN_KEY: &str = "k-twin";

/// The SKU of an entry: its id with a marker bit.
fn sku(entry: Uuid) -> Uuid {
    Uuid::from_u128(entry.as_u128() | 0x5c00)
}
/// A `Uuid` as the application stores it on `SQLite`: a 16-byte blob.
fn x(id: Uuid) -> String {
    format!("X'{}'", id.simple())
}
/// Every other timestamp is the book's, which the application's repository wrote.
const TS: &str = "(SELECT created_at FROM pricing_price_book)";

/// The create input an op stored before 000013: no `model`.
fn old_outcome(book: Uuid, sku: Uuid) -> String {
    json!({
        "target": {"price_book_entry": {"book_id": book, "input": {
            "sku_id": sku, "period": null, "dimension_key": null, "invoice_line_override": null
        }}},
        "correlation": Uuid::from_u128(0xc0),
        "refusal": null,
        "receipt": null,
    })
    .to_string()
}

/// A file database, so the runner's pool, the repositories and a raw connection see one schema.
struct Lite {
    /// The database's own temporary directory, removed with the `Lite` (the file, its `-wal` and
    /// its `-shm`).
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
}

impl Lite {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("pricing-entry-model-")
            .tempdir()
            .unwrap();
        let path = dir.path().join("db.sqlite3");
        Self { _dir: dir, path }
    }

    fn dsn(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }

    async fn pool(&self) -> toolkit_db::Db {
        connect_db(
            &self.dsn(),
            ConnectOpts {
                max_conns: Some(1),
                min_conns: Some(1),
                ..ConnectOpts::default()
            },
        )
        .await
        .unwrap()
    }

    /// The gear's whole migration list through the toolkit runner; `without` leaves one out, as
    /// the deployed database's chain does before this run.
    async fn migrate(&self, without: Option<&str>) -> Result<MigrationResult, MigrationError> {
        let chain = BssPricingGear::default()
            .migrations()
            .into_iter()
            .filter(|m| {
                Some(m.name()) != without
                    && m.name() != "m20260930_000018_usage_rating_policy"
                    && !m.name().contains("000021")
                    && !m.name().contains("000022")
                    && !m.name().contains("000023")
            })
            .collect();
        run_migrations_for_testing(&self.pool().await, chain).await
    }

    async fn raw(&self) -> DatabaseConnection {
        Database::connect(self.dsn()).await.unwrap()
    }

    async fn exec(&self, statements: &[String]) {
        let raw = self.raw().await;
        for sql in statements {
            raw.execute_raw(Statement::from_string(DbBackend::Sqlite, sql.clone()))
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e}"));
        }
        raw.close().await.unwrap();
    }

    async fn refused(&self, sql: &str) -> String {
        let raw = self.raw().await;
        let error = raw
            .execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
            .await
            .expect_err(sql)
            .to_string();
        raw.close().await.unwrap();
        error
    }

    /// One text column `v` per row.
    async fn strings(&self, sql: &str) -> Vec<String> {
        let raw = self.raw().await;
        let rows = raw
            .query_all_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .iter()
            .map(|row| row.try_get::<String>("", "v").unwrap())
            .collect();
        raw.close().await.unwrap();
        rows
    }

    /// The canonical schema dump, each line prefixed with the stanza it belongs to.
    async fn dump(&self) -> Vec<String> {
        let raw = self.raw().await;
        let dump = schema_dump::sqlite_dump(&raw).await;
        raw.close().await.unwrap();
        stanza_lines(&dump)
    }

    /// The `CREATE TABLE` text `SQLite` keeps for a table (its CHECKs live only there).
    async fn ddl(&self, table: &str) -> String {
        self.strings(&format!(
            "SELECT sql AS v FROM sqlite_master WHERE type = 'table' AND name = '{table}'"
        ))
        .await
        .pop()
        .unwrap()
    }

    /// Every row of every `pricing_*` table as JSON of its columns, `model` left out, blobs as
    /// hex; sorted.
    async fn rows_without_model(&self) -> Vec<String> {
        let tables = self
            .strings(
                "SELECT name AS v FROM sqlite_master WHERE type = 'table' AND name LIKE 'pricing\\_%' ESCAPE '\\' ORDER BY name",
            )
            .await;
        let mut rows = Vec::new();
        for table in tables {
            let columns = self
                .strings(&format!(
                    "SELECT name AS v FROM pragma_table_info('{table}') WHERE name <> 'model' ORDER BY cid"
                ))
                .await;
            let pairs = columns
                .iter()
                .map(|c| {
                    format!(
                        "'{c}', CASE typeof(\"{c}\") WHEN 'blob' THEN 'x:' || hex(\"{c}\") ELSE \"{c}\" END"
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            rows.extend(
                self.strings(&format!(
                    "SELECT '{table} ' || json_object({pairs}) AS v FROM {table}"
                ))
                .await,
            );
        }
        rows.sort();
        rows
    }

    /// `(entry, model)` of every entry, the entry as its hyphenated id.
    async fn models(&self) -> Vec<(Uuid, String)> {
        let mut found: Vec<(Uuid, String)> = self
            .strings("SELECT lower(hex(id)) || ' ' || model AS v FROM pricing_price_book_entry")
            .await
            .iter()
            .map(|line| {
                let (id, model) = line.split_once(' ').unwrap();
                (Uuid::parse_str(id).unwrap(), model.to_owned())
            })
            .collect();
        found.sort();
        found
    }

    /// The migration names the runner recorded.
    async fn history(&self) -> Vec<String> {
        let ledger = self
            .strings(
                "SELECT name AS v FROM sqlite_master WHERE type = 'table' AND name LIKE 'toolkit_migrations%'",
            )
            .await;
        assert_eq!(ledger.len(), 1, "one history table: {ledger:?}");
        let mut names = self
            .strings(&format!(r#"SELECT version AS v FROM "{}""#, ledger[0]))
            .await;
        names.sort();
        names
    }

    async fn forget(&self, name: &str) {
        let ledger = self
            .strings(
                "SELECT name AS v FROM sqlite_master WHERE type = 'table' AND name LIKE 'toolkit_migrations%'",
            )
            .await;
        self.exec(&[format!(
            r#"DELETE FROM "{}" WHERE version = '{name}'"#,
            ledger[0]
        )])
        .await;
    }

    /// The book, the reference ops and the Idempotency-Key claim through the application's own
    /// repositories (their tables do not change), so every encoding is the one it writes.
    async fn seed_through_repositories(&self) {
        let provider = DBProvider::<toolkit_db::DbError>::new(self.pool().await);
        let conn = provider.conn().unwrap();
        let scope = AccessScope::for_tenant(TENANT);
        let now = time::OffsetDateTime::now_utc();
        let at = time::Date::from_calendar_date(2026, time::Month::September, 1)
            .unwrap()
            .with_hms(9, 0, 0)
            .unwrap()
            .assume_utc();
        // The book's table gained the archive mark later (000023, D-522), so the book is written
        // in the shape this chain holds: as the repository writes it, without those columns.
        self.exec(&[format!(
            "INSERT INTO pricing_price_book (id, tenant_id, code, name, currency, valid_from, \
             valid_until, description, version, created_at, updated_at) VALUES ({}, {}, \
             'standard', 'Standard', 'EUR', NULL, NULL, NULL, 1, '2026-09-01T09:00:00Z', \
             '2026-09-01T09:00:00Z')",
            x(BOOK),
            x(TENANT)
        )])
        .await;
        let op = |op_id, kind: &str, entry, reservation, key: Option<&str>, state: &str| {
            reference_op::Model {
                op_id,
                tenant_id: TENANT,
                kind: kind.into(),
                ref_kind: "price_book_entry".into(),
                ref_id: entry,
                sku_id: sku(entry),
                reservation_id: reservation,
                idempotency_key: key.map(str::to_owned),
                state: state.into(),
                outcome: Some(old_outcome(BOOK, sku(entry))),
                attempts: 0,
                next_attempt_at: at,
                last_error: None,
                created_by: AUTHOR,
                created_at: at,
                updated_at: at,
            }
        };
        for m in [
            op(
                OP_DONE,
                "create",
                STORAGE,
                Some(Uuid::from_u128(0x9e1)),
                None,
                "done",
            ),
            op(
                OP_CREATE,
                "create",
                LATE,
                Some(Uuid::from_u128(0x9e6)),
                Some(KEY),
                "reserving",
            ),
            op(OP_REREREVE, "rereserve", STORAGE, None, None, "reserving"),
        ] {
            reference_op_repo::insert(&conn, &scope, m).await.unwrap();
        }
        let endpoint = format!("/bss-pricing/v1/price-books/{BOOK}/entries");
        idem::claim_idempotency_key(
            &conn,
            &scope,
            TENANT,
            &endpoint,
            KEY,
            &[0],
            now,
            now + time::Duration::hours(24),
        )
        .await
        .unwrap();
        idem::bind_op(&conn, &scope, TENANT, &endpoint, KEY, OP_CREATE)
            .await
            .unwrap();
    }

    /// A create stored before 000013 (no `model`) for STORAGE's key — its book, SKU, charge kind
    /// and period, the whole key then — still reserving, its receipt held and its Idempotency-Key
    /// bound: the rollout stopped it before its Tx B.
    async fn seed_a_create_for_a_taken_key(&self) {
        let provider = DBProvider::<toolkit_db::DbError>::new(self.pool().await);
        let conn = provider.conn().unwrap();
        let scope = AccessScope::for_tenant(TENANT);
        let now = time::OffsetDateTime::now_utc();
        reference_op_repo::insert(
            &conn,
            &scope,
            reference_op::Model {
                op_id: OP_TWIN,
                tenant_id: TENANT,
                kind: "create".into(),
                ref_kind: "price_book_entry".into(),
                ref_id: TWIN,
                sku_id: sku(STORAGE),
                reservation_id: Some(Uuid::from_u128(0x9e7)),
                idempotency_key: Some(TWIN_KEY.into()),
                state: "reserving".into(),
                outcome: Some(old_outcome(BOOK, sku(STORAGE))),
                attempts: 0,
                next_attempt_at: now,
                last_error: None,
                created_by: AUTHOR,
                created_at: now,
                updated_at: now,
            },
        )
        .await
        .unwrap();
        let endpoint = format!("/bss-pricing/v1/price-books/{BOOK}/entries");
        idem::claim_idempotency_key(
            &conn,
            &scope,
            TENANT,
            &endpoint,
            TWIN_KEY,
            &[1],
            now,
            now + time::Duration::hours(24),
        )
        .await
        .unwrap();
        idem::bind_op(&conn, &scope, TENANT, &endpoint, TWIN_KEY, OP_TWIN)
            .await
            .unwrap();
    }
}

/// Every line of a dump prefixed with its stanza header (`TABLE x`, `INDEX y`, …). The runner's
/// ledger (`toolkit_migrations…`, which the bare-manager dump of a fresh chain never creates) is
/// bookkeeping, not schema, and is left out.
fn stanza_lines(dump: &str) -> Vec<String> {
    let mut stanza = String::new();
    let mut lines = Vec::new();
    for line in dump.lines() {
        if !line.starts_with(' ') {
            line.clone_into(&mut stanza);
        }
        if stanza.contains("toolkit_migrations") {
            continue;
        }
        if line.starts_with(' ') {
            lines.push(format!("{stanza} ::{line}"));
        } else {
            lines.push(line.to_owned());
        }
    }
    lines
}

fn entry_row(id: Uuid, kind: &str, period: Option<&str>) -> String {
    let period = period.map_or_else(|| "NULL".to_owned(), |p| format!("'{p}'"));
    format!(
        "INSERT INTO pricing_price_book_entry (id,tenant_id,book_id,sku_id,charge_kind,period,dimension_key,invoice_line_override,reservation_id,reference_state,version,created_at,updated_at) \
         VALUES ({},{},{},{},'{kind}',{period},NULL,NULL,{},'confirmed',1,{TS},{TS})",
        x(id),
        x(TENANT),
        x(BOOK),
        x(sku(id)),
        x(Uuid::from_u128(id.as_u128() | 0x9000)),
    )
}

/// A price of the chain before 000013, which still has its own `model` column.
#[expect(
    clippy::too_many_arguments,
    reason = "one stored row, column for column"
)]
fn price_row(
    n: u128,
    entry: Uuid,
    version_no: i32,
    model: &str,
    money: &Value,
    from: &str,
    state: &str,
    unit: Option<Uuid>,
) -> String {
    let unit = unit.map_or_else(|| "NULL".to_owned(), x);
    let approved_at = if state == "approved" { TS } else { "NULL" };
    format!(
        "INSERT INTO pricing_price (id,tenant_id,price_book_entry_id,version_no,dim_value,model,price_json,min_fee,eligibility,effective_from,effective_to,keep_for_bound,closed_explicitly,temporary_until,paired_price_id,return_of_price_id,state,pending_unit_id,approved_by_unit_id,note,created_by,approved_at,version,created_at,updated_at) \
         VALUES ({},{},{},{version_no},NULL,'{model}','{money}',NULL,'all','{from}',NULL,0,0,NULL,NULL,NULL,'{state}',{unit},NULL,'seed',{},{approved_at},1,{TS},{TS})",
        x(Uuid::from_u128(n)),
        x(TENANT),
        x(entry),
        x(AUTHOR),
    )
}

fn graduated() -> Value {
    json!({"tiers":[{"up_to":"1000","rate":"0.010"},{"up_to":null,"rate":"0.008"}]})
}

/// Entries, prices of several states, a pending unit, a plan with items, on the chain before
/// 000013.
fn seed_rows() -> Vec<String> {
    vec![
        entry_row(STORAGE, "usage", None),
        entry_row(SEATS, "recurring", Some("month")),
        entry_row(SETUP, "one_time", None),
        entry_row(CALLS, "usage", None),
        entry_row(SUPPORT, "recurring", Some("year")),
        format!(
            "INSERT INTO pricing_approval_unit (id,tenant_id,kind,ref_type,ref_id,state,common_effective_date,quorum_required,generation,submitted_by,submitted_at,decided_at,decided_note,snapshot,snapshot_hash,version) \
             VALUES ({},{},'prices','price_book',{},'pending',NULL,1,1,{},{TS},NULL,NULL,'{{}}','seed',1)",
            x(UNIT),
            x(TENANT),
            x(BOOK),
            x(AUTHOR)
        ),
        price_row(
            0x11,
            STORAGE,
            1,
            "graduated",
            &graduated(),
            "2026-01-01",
            "approved",
            None,
        ),
        price_row(
            0x12,
            STORAGE,
            2,
            "graduated",
            &graduated(),
            "2031-01-01",
            "draft",
            None,
        ),
        price_row(
            0x13,
            STORAGE,
            3,
            "graduated",
            &graduated(),
            "2031-02-01",
            "rejected",
            None,
        ),
        price_row(
            0x21,
            SEATS,
            1,
            "flat",
            &json!({"amount":"10.00"}),
            "2026-01-01",
            "approved",
            None,
        ),
        price_row(
            0x22,
            SEATS,
            2,
            "flat",
            &json!({"amount":"12.00"}),
            "2031-01-01",
            "pending",
            Some(UNIT),
        ),
        price_row(
            0x51,
            SUPPORT,
            1,
            "per_unit",
            &json!({"rate":"2.00"}),
            "2026-01-01",
            "approved",
            None,
        ),
        format!(
            "INSERT INTO pricing_plan (id,tenant_id,code,name,published_rev,version,created_by,created_at,updated_at) \
             VALUES ({},{},'pro','Pro',NULL,1,{},{TS},{TS})",
            x(PLAN),
            x(TENANT),
            x(AUTHOR)
        ),
        format!(
            "INSERT INTO pricing_plan_revision (id,tenant_id,plan_id,rev_no,book_id,state,available_from,pending_unit_id,approved_by_unit_id,published_at,version,created_by,created_at,updated_at) \
             VALUES ({},{},{},1,{},'draft',NULL,NULL,NULL,NULL,1,{},{TS},{TS})",
            x(REVISION),
            x(TENANT),
            x(PLAN),
            x(BOOK),
            x(AUTHOR)
        ),
        item_row(0x701, sku(STORAGE), Some(STORAGE), "paid", None),
        item_row(0x702, sku(SEATS), Some(SEATS), "paid", None),
        item_row(0x703, Uuid::from_u128(0x5c99), None, "included", Some("5")),
    ]
}

fn item_row(n: u128, sku: Uuid, entry: Option<Uuid>, treatment: &str, qty: Option<&str>) -> String {
    let entry = entry.map_or_else(|| "NULL".to_owned(), x);
    let qty = qty.map_or_else(|| "NULL".to_owned(), |q| format!("'{q}'"));
    format!(
        "INSERT INTO pricing_plan_item (id,tenant_id,revision_id,sku_id,price_book_entry_id,treatment,included_qty,qty_min,reservation_id,reference_state,version,created_by,created_at,updated_at) \
         VALUES ({},{},{},{},{entry},'{treatment}',{qty},NULL,{},'confirmed',1,{},{TS},{TS})",
        x(Uuid::from_u128(n)),
        x(TENANT),
        x(REVISION),
        x(sku),
        x(Uuid::from_u128(n | 0x9000)),
        x(AUTHOR)
    )
}

/// The deployed database before this run, seeded.
async fn seeded() -> Lite {
    let db = Lite::new();
    let before = db.migrate(Some(MIGRATION)).await.unwrap();
    assert!(
        !before.applied_names.iter().any(|n| n == MIGRATION),
        "the chain before this run"
    );
    db.seed_through_repositories().await;
    db.exec(&seed_rows()).await;
    db
}

#[tokio::test]
async fn the_forward_migration_moves_the_model_to_the_entry_and_keeps_every_row() {
    let db = seeded().await;
    let dump_before = db.dump().await;
    let rows_before = db.rows_without_model().await;
    let price_ddl_before = db.ddl("pricing_price").await;
    let entry_ddl_before = db.ddl("pricing_price_book_entry").await;

    let result = db.migrate(None).await.unwrap();

    assert_eq!(result.applied_names, [MIGRATION], "only 000013 was pending");
    // The backfill: the one model of all an entry's prices, whatever their state; the charge
    // kind's default without a price.
    assert_eq!(
        db.models().await,
        vec![
            (STORAGE, "graduated".to_owned()),
            (SEATS, "flat".to_owned()),
            (SETUP, "flat".to_owned()),
            (CALLS, "per_unit".to_owned()),
            (SUPPORT, "per_unit".to_owned()),
        ]
    );
    assert_eq!(
        db.rows_without_model().await,
        rows_before,
        "every row survives; only the model moved"
    );
    assert!(
        db.strings("SELECT \"table\" AS v FROM pragma_foreign_key_check")
            .await
            .is_empty()
    );
    // The structure: exactly the model column moved and the key index took it.
    let dump_after = db.dump().await;
    let removed: Vec<&String> = dump_before
        .iter()
        .filter(|l| !dump_after.contains(l))
        .collect();
    let added: Vec<&String> = dump_after
        .iter()
        .filter(|l| !dump_before.contains(l))
        .collect();
    let show = |lines: &[&String]| {
        lines
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    };
    eprintln!(
        "D-427 SQLite dump diff\nremoved:\n  {}\nadded:\n  {}",
        show(&removed),
        show(&added)
    );
    assert_eq!(
        removed,
        [
            "INDEX pricing_price_book_entry_key ::  DDL CREATE UNIQUE INDEX pricing_price_book_entry_key ON pricing_price_book_entry (book_id, sku_id, charge_kind, coalesce(period, ''))",
            "TABLE pricing_price ::  COLUMN model TEXT NOT NULL DEFAULT - PK 0",
        ]
    );
    assert_eq!(
        added,
        [
            "INDEX pricing_price_book_entry_key ::  DDL CREATE UNIQUE INDEX pricing_price_book_entry_key ON pricing_price_book_entry (book_id, sku_id, charge_kind, coalesce(period, ''), model)",
            "TABLE pricing_price_book_entry ::  COLUMN model TEXT NOT NULL DEFAULT 'flat' PK 0",
        ]
    );
    // The CHECKs, which only the table text carries on SQLite: the price keeps every one but the
    // model's; the entry gains the model's.
    let price_check =
        "model text NOT NULL CHECK (model IN ('flat','per_unit','graduated','volume','package')), ";
    assert_eq!(
        price_ddl_before.matches(price_check).count(),
        1,
        "{price_ddl_before}"
    );
    assert_eq!(
        db.ddl("pricing_price").await,
        price_ddl_before.replace(price_check, ""),
        "DROP COLUMN removes the model and its CHECK, nothing else"
    );
    let entry_ddl_after = db.ddl("pricing_price_book_entry").await;
    assert_eq!(
        entry_ddl_after.replace(
            ", model text NOT NULL DEFAULT 'flat' CHECK (model IN ('flat','per_unit','graduated','volume','package'))",
            ""
        ),
        entry_ddl_before,
        "ADD COLUMN appends the model with its CHECK: {entry_ddl_after}"
    );
    // An upgraded database and a fresh one hold the same schema.
    let fresh = Database::connect("sqlite::memory:").await.unwrap();
    let manager = sea_orm_migration::SchemaManager::new(&fresh);
    for migration in schema_dump::name_ordered_chain().into_iter().filter(|m| {
        m.name() != "m20260930_000018_usage_rating_policy"
            && !m.name().contains("000021")
            && !m.name().contains("000022")
            && !m.name().contains("000023")
    }) {
        migration.up(&manager).await.unwrap();
    }
    assert_eq!(
        dump_after,
        stanza_lines(&schema_dump::sqlite_dump(&fresh).await)
    );
    // The guard, made pending again, passes the upgraded database.
    db.forget(GUARD).await;
    assert_eq!(db.migrate(None).await.unwrap().applied_names, [GUARD]);
}

/// What the migration wrote, read through the application: the entry reads carry the backfilled
/// model and every price echoes its entry's.
#[tokio::test]
async fn the_application_reads_the_backfilled_models() {
    let db = seeded().await;
    db.migrate(None).await.unwrap();
    // Current application entities require the complete schema after the historical migration assertions.
    run_migrations_for_testing(&db.pool().await, BssPricingGear::default().migrations())
        .await
        .unwrap();
    let state = state_on(
        DBProvider::new(db.pool().await),
        Arc::new(Script::default()),
    )
    .await;
    let app = app_for(state, TENANT);
    let ctx = user_of(TENANT);
    let (status, export, _) = request(
        &app,
        &ctx,
        "GET",
        &format!("/price-books/{BOOK}/export"),
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "{export}");
    let mut seen = Vec::new();
    for e in export["entries"].as_array().unwrap() {
        let model = e["entry"]["model"].as_str().unwrap().to_owned();
        for p in e["prices"].as_array().unwrap() {
            assert_eq!(p["model"], model.as_str(), "a price echoes its entry: {p}");
        }
        seen.push((
            e["entry"]["id"].as_str().unwrap().to_owned(),
            model,
            e["prices"].as_array().unwrap().len(),
        ));
    }
    seen.sort();
    let mut want = vec![
        (STORAGE.to_string(), "graduated".to_owned(), 3),
        (SEATS.to_string(), "flat".to_owned(), 2),
        (SETUP.to_string(), "flat".to_owned(), 0),
        (CALLS.to_string(), "per_unit".to_owned(), 0),
        (SUPPORT.to_string(), "per_unit".to_owned(), 1),
    ];
    want.sort();
    assert_eq!(seen, want);
}

/// The op-input choice (D-427): an op stored before 000013 has no `model`, and it resolves to the
/// model the migration gives an entry without prices — the charge kind's default. The create
/// still reserving (its receipt held) is resumed by the ticker after the migration and writes
/// its entry with that model, answering its Idempotency-Key; the rereserve of an entry whose
/// prices gave it another model leaves that model alone.
#[tokio::test]
async fn an_entry_op_stored_before_the_migration_resumes_after_it() {
    let db = seeded().await;
    db.migrate(None).await.unwrap();
    let script = Arc::new(Script::default());
    // Current application entities require the complete schema after the historical migration assertions.
    run_migrations_for_testing(&db.pool().await, BssPricingGear::default().migrations())
        .await
        .unwrap();
    let state = state_on(DBProvider::new(db.pool().await), script.clone()).await;
    let system = system_actor(TENANT).unwrap();

    let receipt = reference_work::drive(
        &state,
        &system,
        OP_CREATE,
        Arc::new(WallClock),
        Caller::Ticker,
    )
    .await
    .unwrap()
    .expect("the create finished with its receipt");
    assert_eq!(receipt.status, 201, "{}", receipt.body);
    let body: Value = serde_json::from_str(&receipt.body).unwrap();
    assert_eq!(body["id"], LATE.to_string());
    assert_eq!(body["charge_kind"], "usage");
    assert_eq!(
        body["model"], "per_unit",
        "the usage default, as the backfill gives"
    );
    assert_eq!(body["reference_state"], "confirmed");
    assert!(
        db.models().await.contains(&(LATE, "per_unit".to_owned())),
        "the entry row carries it"
    );
    assert_eq!(
        db.strings(&format!(
            "SELECT state || ' ' || response_status AS v FROM pricing_idempotency WHERE client_key = '{KEY}'"
        ))
        .await,
        ["answered 201"]
    );

    reference_work::drive(
        &state,
        &system,
        OP_REREREVE,
        Arc::new(WallClock),
        Caller::Ticker,
    )
    .await
    .unwrap();
    assert_eq!(
        db.strings(&format!(
            "SELECT state AS v FROM pricing_reference_op WHERE op_id = {}",
            x(OP_REREREVE)
        ))
        .await,
        ["done"]
    );
    assert!(
        db.models()
            .await
            .contains(&(STORAGE, "graduated".to_owned())),
        "a rereserve never writes the model: the entry keeps its backfilled one"
    );
    assert_eq!(
        db.strings(&format!(
            "SELECT reference_state AS v FROM pricing_price_book_entry WHERE id = {}",
            x(STORAGE)
        ))
        .await,
        ["confirmed"]
    );
}

/// A create stored before 000013 was posted under the key of its day — book, SKU, charge kind,
/// period — and has no `model`. When an entry already holds that key (STORAGE, backfilled
/// `graduated`, not the usage default), the create takes that entry's model, so its insert meets
/// the key and the op ends `ENTRY_KEY_TAKEN`, as the contract it was called under answers; it
/// does not write a second entry beside STORAGE in a model nobody chose.
#[tokio::test]
async fn an_in_flight_create_for_a_taken_key_meets_it_after_the_model_moved() {
    let db = seeded().await;
    db.seed_a_create_for_a_taken_key().await;
    db.migrate(None).await.unwrap();
    assert!(
        db.models()
            .await
            .contains(&(STORAGE, "graduated".to_owned())),
        "the key holder's model is not the usage default"
    );
    let script = Arc::new(Script::default());
    // Current application entities require the complete schema after the historical migration assertions.
    run_migrations_for_testing(&db.pool().await, BssPricingGear::default().migrations())
        .await
        .unwrap();
    let state = state_on(DBProvider::new(db.pool().await), script.clone()).await;
    let system = system_actor(TENANT).unwrap();

    let receipt = reference_work::drive(
        &state,
        &system,
        OP_TWIN,
        Arc::new(WallClock),
        Caller::Ticker,
    )
    .await
    .unwrap()
    .expect("the create finished with its answer");

    assert_eq!(receipt.status, 409, "{}", receipt.body);
    assert!(receipt.body.contains("ENTRY_KEY_TAKEN"), "{}", receipt.body);
    assert_eq!(
        db.strings(&format!(
            "SELECT lower(hex(id)) AS v FROM pricing_price_book_entry WHERE sku_id = {}",
            x(sku(STORAGE))
        ))
        .await,
        [STORAGE.simple().to_string()],
        "no second entry for the key"
    );
    assert_eq!(
        db.strings(&format!(
            "SELECT state AS v FROM pricing_reference_op WHERE op_id = {}",
            x(OP_TWIN)
        ))
        .await,
        ["done"]
    );
    assert_eq!(
        db.strings(&format!(
            "SELECT state || ' ' || response_status AS v FROM pricing_idempotency WHERE client_key = '{TWIN_KEY}'"
        ))
        .await,
        ["answered 409"]
    );
    assert!(
        Script::count(&script.releases) >= 1,
        "the refused create released its reservation"
    );
}

/// Two models on one entry (legal before D-427, through a draft or a rejected price too) make the
/// migration fail, naming the entries, and change nothing.
#[tokio::test]
async fn two_models_on_one_entry_fail_the_migration_naming_the_entry_and_change_nothing() {
    let db = seeded().await;
    db.exec(&[
        price_row(
            0x31,
            CALLS,
            1,
            "per_unit",
            &json!({"rate":"1.00"}),
            "2026-01-01",
            "approved",
            None,
        ),
        price_row(
            0x32,
            CALLS,
            2,
            "graduated",
            &graduated(),
            "2031-01-01",
            "rejected",
            None,
        ),
        price_row(
            0x33,
            SUPPORT,
            2,
            "flat",
            &json!({"amount":"3.00"}),
            "2031-01-01",
            "draft",
            None,
        ),
    ])
    .await;
    let dump_before = db.dump().await;
    let history_before = db.history().await;
    let columns = "SELECT name AS v FROM pragma_table_info('pricing_price') ORDER BY cid";
    let price_columns = db.strings(columns).await;

    let error = db.migrate(None).await.unwrap_err().to_string();

    assert!(error.contains(MIGRATION), "{error}");
    assert!(
        error.contains(&CALLS.to_string()) && error.contains(&SUPPORT.to_string()),
        "names both entries: {error}"
    );
    assert!(
        !error.contains(&STORAGE.to_string()),
        "an entry of one model is not named: {error}"
    );
    assert_eq!(db.dump().await, dump_before, "the schema did not change");
    assert_eq!(db.strings(columns).await, price_columns);
    assert_eq!(db.history().await, history_before, "000013 is not recorded");
    assert_eq!(
        db.strings("SELECT CAST(count(*) AS text) AS v FROM pricing_price WHERE model IS NOT NULL")
            .await,
        ["9"],
        "every price keeps its model"
    );
}

/// After the migration the key takes the model: two entries of one SKU, kind and period with
/// different models live in one book, the same model twice is the key's refusal, and the model
/// is NOT NULL and checked; the price has no model column any more.
#[tokio::test]
async fn after_the_migration_the_entry_key_takes_the_model() {
    let db = seeded().await;
    db.migrate(None).await.unwrap();
    let insert = |id: u128, model: &str| {
        format!(
            "INSERT INTO pricing_price_book_entry (id,tenant_id,book_id,sku_id,charge_kind,period,dimension_key,invoice_line_override,reservation_id,reference_state,version,created_at,updated_at,model) \
             VALUES ({},{},{},{},'usage',NULL,NULL,NULL,{},'confirmed',1,{TS},{TS},{model})",
            x(Uuid::from_u128(id)),
            x(TENANT),
            x(BOOK),
            x(sku(CALLS)),
            x(Uuid::from_u128(id | 0x9000)),
        )
    };
    // CALLS (usage, per_unit) is already in the book: graduated beside it is another entry.
    db.exec(&[insert(0xf1, "'graduated'")]).await;
    for (sql, refusal) in [
        (
            insert(0xf2, "'per_unit'"),
            "UNIQUE constraint failed: index 'pricing_price_book_entry_key'",
        ),
        (insert(0xf3, "'stair'"), "CHECK constraint failed"),
        (
            insert(0xf4, "NULL"),
            "NOT NULL constraint failed: pricing_price_book_entry.model",
        ),
        (
            price_row(
                0x99,
                CALLS,
                9,
                "per_unit",
                &json!({"rate":"1.00"}),
                "2031-01-01",
                "draft",
                None,
            ),
            "has no column named model",
        ),
    ] {
        let error = db.refused(&sql).await;
        assert!(error.contains(refusal), "{sql}\n{error}");
    }
}
