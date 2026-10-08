//! D-446 on `SQLite`: the forward migration `m20260929_000017_revision_scheduled` widens the
//! revision state CHECK with `scheduled` by rebuilding the family `pricing_plan_revision` +
//! `pricing_plan_item` inside the toolkit runner's transaction (no PRAGMA; P-D-196's precedent),
//! and adds the partial unique index `pricing_plan_revision_scheduled`.
//!
//! The deployed database is the chain without 000017, with two plans, revisions in every state the
//! old CHECK admits (superseded, published, pending, draft) and items with and without an entry,
//! written through the gear's repositories. The whole list then runs through the real runner and
//! applies 000017 alone: the schema dump differs by the CHECK and the new index only; each family
//! table's text differs by the CHECK only (the quotes a rename writes aside); every row of both
//! tables survives; every foreign key holds and points at the rebuilt parent; the CHECK admits
//! `scheduled` and still refuses a stranger; the new index refuses a second scheduled revision; an
//! upgraded database equals a fresh one, a replay applies nothing, and the schema guard passes the
//! rebuilt family. The Postgres twin is `postgres_revision_scheduled_migration.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod scheduled_support;
mod schema_dump;

use bss_pricing::infra::storage::repo::{plan_item_repo, plan_revision_repo};
use bss_pricing::module::BssPricingGear;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use serde_json::Value;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use uuid::Uuid;

const MIGRATION: &str = "m20260929_000017_revision_scheduled";
const GUARD: &str = "m0000_pricing_refuse_a_legacy_or_stale_schema";
const TENANT: Uuid = Uuid::from_u128(0x7e0a_0000_0000_0000_0000_0000_0017_0001);
/// The parent first, then its only child.
const FAMILY: [&str; 2] = ["pricing_plan_revision", "pricing_plan_item"];
const OLD_CHECK: &str = "CHECK (state IN ('draft','pending','published','superseded'))";
const NEW_CHECK: &str = "CHECK (state IN ('draft','pending','scheduled','published','superseded'))";

/// A file database in its own temporary directory, so the runner's pool, the repositories and a
/// raw connection see one schema, and the directory goes with the test.
struct Lite {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
}
impl Lite {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("pricing-revision-scheduled-")
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
    async fn migrate(&self, without: Option<&str>) -> Result<MigrationResult, MigrationError> {
        let chain = BssPricingGear::default()
            .migrations()
            .into_iter()
            .filter(|m| Some(m.name()) != without)
            .collect();
        run_migrations_for_testing(&self.pool().await, chain).await
    }
    async fn raw(&self) -> DatabaseConnection {
        Database::connect(self.dsn()).await.unwrap()
    }
    async fn exec(&self, sql: &str) -> Result<(), String> {
        let raw = self.raw().await;
        let result = raw
            .execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
            .await
            .map(|_| ())
            .map_err(|e| e.to_string());
        raw.close().await.unwrap();
        result
    }
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
    async fn dump(&self) -> Vec<String> {
        let raw = self.raw().await;
        let dump = schema_dump::sqlite_dump(&raw).await;
        raw.close().await.unwrap();
        stanza_lines(&dump)
    }
    /// A table's stored text with the double quotes a rename writes removed.
    async fn ddl(&self, table: &str) -> String {
        self.strings(&format!(
            "SELECT replace(sql, '\"', '') AS v FROM sqlite_master WHERE type = 'table' \
             AND name = '{table}'"
        ))
        .await
        .pop()
        .unwrap()
    }
    /// Every foreign key of the family, as `pragma_foreign_key_list` reports it.
    async fn foreign_keys(&self) -> Vec<String> {
        let mut keys = Vec::new();
        for table in FAMILY {
            keys.extend(
                self.strings(&format!(
                    "SELECT '{table}: ' || \"from\" || ' -> ' || \"table\" || '.' || \"to\" || ' ' \
                     || on_update || ' ' || on_delete AS v FROM pragma_foreign_key_list('{table}') \
                     ORDER BY \"from\""
                ))
                .await,
            );
        }
        keys
    }
    /// Every row of the family: every column, blobs as hex, as JSON, by id.
    async fn rows(&self) -> Vec<Value> {
        let mut rows = Vec::new();
        for table in FAMILY {
            let columns = self
                .strings(&format!(
                    "SELECT name AS v FROM pragma_table_info('{table}') ORDER BY cid"
                ))
                .await;
            let pairs = columns
                .iter()
                .map(|c| {
                    format!(
                        "'{c}', CASE typeof(\"{c}\") WHEN 'blob' THEN 'x:' || hex(\"{c}\") \
                         ELSE \"{c}\" END"
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            rows.extend(
                self.strings(&format!(
                    "SELECT json_object('table', '{table}', {pairs}) AS v FROM {table} ORDER BY id"
                ))
                .await
                .iter()
                .map(|r| serde_json::from_str::<Value>(r).unwrap()),
            );
        }
        rows
    }
    /// Drop `name` from the runner's history table, so the next run applies it again.
    async fn forget(&self, name: &str) {
        let ledger = self
            .strings(
                "SELECT name AS v FROM sqlite_master WHERE type = 'table' \
                 AND name LIKE 'toolkit_migrations%'",
            )
            .await;
        assert_eq!(ledger.len(), 1, "one history table: {ledger:?}");
        self.exec(&format!(
            r#"DELETE FROM "{}" WHERE version = '{name}'"#,
            ledger[0]
        ))
        .await
        .unwrap();
    }
}
/// Each dump line prefixed with the stanza it belongs to; the runner's ledger left out.
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
/// The deployed database before this run: the chain without 000017, the family seeded.
async fn seeded() -> (Lite, scheduled_support::Family) {
    let db = Lite::new();
    let before = db.migrate(Some(MIGRATION)).await.unwrap();
    assert!(
        !before.applied_names.iter().any(|n| n == MIGRATION),
        "the chain before this run"
    );
    let family = scheduled_support::seed(db.pool().await, TENANT).await;
    (db, family)
}

#[tokio::test]
async fn the_family_rebuild_keeps_every_row_and_key_and_widens_only_the_state_check() {
    let (db, family) = seeded().await;
    let dump_before = db.dump().await;
    let ddl_before = [db.ddl(FAMILY[0]).await, db.ddl(FAMILY[1]).await];
    let keys_before = db.foreign_keys().await;
    let rows_before = db.rows().await;
    assert_eq!(rows_before.len(), 4 + family.items, "{rows_before:#?}");
    assert_eq!(
        ddl_before[0].matches(OLD_CHECK).count(),
        1,
        "{}",
        ddl_before[0]
    );

    let result = db.migrate(None).await.unwrap();
    assert_eq!(result.applied_names, [MIGRATION], "only 000017 was pending");

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
        "000017 SQLite dump diff\nremoved:\n  {}\nadded:\n  {}",
        show(&removed),
        show(&added)
    );
    assert_eq!(
        removed,
        [format!(
            "TABLE pricing_plan_revision ::  CONSTRAINT chk_pricing_plan_revision_state {OLD_CHECK}"
        )
        .as_str()]
    );
    assert_eq!(
        added,
        [
            "INDEX pricing_plan_revision_scheduled",
            "INDEX pricing_plan_revision_scheduled ::  ON pricing_plan_revision",
            "INDEX pricing_plan_revision_scheduled ::  DDL CREATE UNIQUE INDEX \
             pricing_plan_revision_scheduled ON pricing_plan_revision (plan_id) WHERE state = \
             'scheduled'",
            format!(
                "TABLE pricing_plan_revision ::  CONSTRAINT chk_pricing_plan_revision_state {NEW_CHECK}"
            )
            .as_str(),
        ]
    );
    // The tables are the old text again, the widened CHECK aside: 000011's and 000012's verbatim.
    let ddl_after = [db.ddl(FAMILY[0]).await, db.ddl(FAMILY[1]).await];
    eprintln!(
        "000017 SQLite table text after:\n{}\n{}",
        ddl_after[0], ddl_after[1]
    );
    assert_eq!(ddl_after[0], ddl_before[0].replace(OLD_CHECK, NEW_CHECK));
    assert_eq!(ddl_after[1], ddl_before[1]);
    assert_eq!(db.foreign_keys().await, keys_before, "every key as it was");
    assert!(
        keys_before.contains(
            &"pricing_plan_item: revision_id -> pricing_plan_revision.id NO ACTION NO ACTION"
                .to_owned()
        ),
        "{keys_before:#?}"
    );
    assert_eq!(db.rows().await, rows_before, "every row survives");
    assert!(
        db.strings("SELECT \"table\" AS v FROM pragma_foreign_key_check")
            .await
            .is_empty(),
        "no row breaks a key"
    );
    // An upgraded database and a fresh one hold the same schema; a replay applies nothing.
    let fresh = Database::connect("sqlite::memory:").await.unwrap();
    assert_eq!(
        dump_after,
        stanza_lines(&schema_dump::migrate_and_dump_sqlite(&fresh).await)
    );
    assert!(db.migrate(None).await.unwrap().applied_names.is_empty());
}

/// What the rebuild promises, exercised: the gear reads the family written before it, schedules
/// through its own write, and the keys, the CHECK and all three partial indexes still refuse.
#[tokio::test]
async fn after_the_rebuild_the_gear_schedules_and_every_key_check_and_index_holds() {
    let (db, family) = seeded().await;
    db.migrate(None).await.unwrap();
    let provider = DBProvider::<toolkit_db::DbError>::new(db.pool().await);
    let conn = provider.conn().unwrap();
    let scope = AccessScope::for_tenant(TENANT);
    let states: Vec<(i32, String)> =
        plan_revision_repo::for_plan(&conn, &scope, TENANT, family.alpha)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.rev_no, r.state))
            .collect();
    assert_eq!(
        states,
        [
            (1, "superseded".to_owned()),
            (2, "published".to_owned()),
            (3, "pending".to_owned())
        ]
    );
    let beta = plan_revision_repo::for_plan(&conn, &scope, TENANT, family.beta)
        .await
        .unwrap();
    assert_eq!(
        (beta.len(), beta[0].id, beta[0].state.as_str()),
        (1, family.draft, "draft")
    );
    assert_eq!(
        plan_item_repo::for_revision(&conn, &scope, TENANT, family.published)
            .await
            .unwrap()
            .len(),
        2
    );
    // The CHECK admits `scheduled`, through the gear's own write.
    plan_revision_repo::schedule(
        &conn,
        &scope,
        TENANT,
        family.pending,
        family.pending_unit,
        scheduled_support::at(14),
    )
    .await
    .unwrap();
    drop(provider);
    let hex = |id: Uuid| format!("x'{}'", id.simple());
    for (sql, refusal) in [
        (
            format!(
                "UPDATE pricing_plan_revision SET state = 'scheduled' WHERE id = {}",
                hex(family.published)
            ),
            "UNIQUE constraint failed: pricing_plan_revision.plan_id",
        ),
        (
            format!(
                "UPDATE pricing_plan_revision SET state = 'draft' WHERE id = {}",
                hex(family.pending)
            ),
            "",
        ),
        (
            format!(
                "UPDATE pricing_plan_revision SET state = 'pending' WHERE id = {}",
                hex(family.superseded)
            ),
            "UNIQUE constraint failed: pricing_plan_revision.plan_id",
        ),
        (
            format!(
                "UPDATE pricing_plan_revision SET state = 'published' WHERE id = {}",
                hex(family.superseded)
            ),
            "UNIQUE constraint failed: pricing_plan_revision.plan_id",
        ),
        (
            format!(
                "UPDATE pricing_plan_revision SET state = 'retired' WHERE id = {}",
                hex(family.draft)
            ),
            "CHECK constraint failed",
        ),
        (
            format!(
                "INSERT INTO pricing_plan_item (id, tenant_id, revision_id, sku_id, treatment, \
                 reference_state, created_by, created_at, updated_at) VALUES ({}, {}, {}, {}, \
                 'included', 'unreserved', {}, '2026-09-29T00:00:00Z', '2026-09-29T00:00:00Z')",
                hex(Uuid::new_v4()),
                hex(TENANT),
                hex(Uuid::new_v4()),
                hex(Uuid::new_v4()),
                hex(Uuid::new_v4())
            ),
            "FOREIGN KEY constraint failed",
        ),
        (
            format!(
                "DELETE FROM pricing_plan_revision WHERE id = {}",
                hex(family.draft)
            ),
            "FOREIGN KEY constraint failed",
        ),
    ] {
        match db.exec(&sql).await {
            Ok(()) => assert!(refusal.is_empty(), "{sql} was not refused"),
            Err(error) => assert!(
                !refusal.is_empty() && error.contains(refusal),
                "{sql}\n{error}"
            ),
        }
    }
    // The schema guard reads the rebuilt family and passes it.
    db.forget(GUARD).await;
    assert_eq!(db.migrate(None).await.unwrap().applied_names, [GUARD]);
}
