//! P-D-219 on Postgres: `m20260928_000009_unit_submit_note` adds `submit_note` to
//! `bss.products_approval_unit`. The twin of `unit_submit_note_migration.rs`, with the same shape:
//! the gear's whole list without 000009, approval units seeded (a pending one and a rejected one,
//! with their item and decision), the structure captured, then the whole list again through the
//! real runner, which applies 000009 alone. The structure is `information_schema.columns`,
//! `pg_constraint` and `pg_indexes` of the table. Exactly one fact may differ: the new column,
//! nullable text at the last position. Every unit survives and reads null, in SQL and through the
//! gear's repository; an upgraded database equals a fresh one; a replay applies nothing.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod pg_support;

use bss_products::gear::BssProductsGear;
use bss_products::infra::storage::repo;
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use toolkit::contracts::DatabaseCapability;
use toolkit_db::DBProvider;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use toolkit_db::secure::AccessScope;
use uuid::Uuid;

const MIGRATION: &str = "m20260928_000009_unit_submit_note";
const T1: Uuid = Uuid::from_u128(0x7e0a_0000_0000_0000_0000_0000_0009_0001);
const PENDING: &str = "00000000-0000-0000-0000-000000090001";
const DECIDED: &str = "00000000-0000-0000-0000-000000090002";
const S1: &str = "00000000-0000-0000-0000-000000090051";

/// The gear's whole list through the runner; `without` leaves one migration out.
async fn migrate(pg: &Pg, without: Option<&str>) -> Result<MigrationResult, MigrationError> {
    let db = pg.db().await;
    let chain = BssProductsGear::default()
        .migrations()
        .into_iter()
        .filter(|m| Some(m.name()) != without)
        .collect();
    run_migrations_for_testing(&db, chain).await
}

async fn exec(pg: &Pg, sql: &str) {
    let raw = pg.raw().await;
    raw.execute_raw(Statement::from_string(DbBackend::Postgres, sql.to_owned()))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
    raw.close().await.unwrap();
}

async fn strings(pg: &Pg, sql: &str) -> Vec<String> {
    let raw = pg.raw().await;
    let rows = raw
        .query_all_raw(Statement::from_string(DbBackend::Postgres, sql.to_owned()))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .iter()
        .map(|row| row.try_get::<String>("", "v").unwrap())
        .collect();
    raw.close().await.unwrap();
    rows
}

/// The structure of the unit table, one fact per line, sorted.
async fn structure(pg: &Pg) -> Vec<String> {
    let mut facts = Vec::new();
    for sql in [
        "SELECT 'column ' || column_name || ' ' || data_type || ' nullable=' || is_nullable || \
         ' default=' || coalesce(column_default, '(none)') || ' position=' || ordinal_position AS v \
         FROM information_schema.columns WHERE table_schema = 'bss' \
         AND table_name = 'products_approval_unit'",
        "SELECT 'constraint ' || con.conname || ' ' || con.contype::text || ' ' || \
         pg_get_constraintdef(con.oid) AS v FROM pg_constraint con \
         JOIN pg_class c ON c.oid = con.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = 'bss' AND c.relname = 'products_approval_unit'",
        "SELECT 'index ' || indexname || ' ' || indexdef AS v FROM pg_indexes \
         WHERE schemaname = 'bss' AND tablename = 'products_approval_unit'",
    ] {
        facts.extend(strings(pg, sql).await);
    }
    facts.sort();
    facts
}

fn seed() -> Vec<String> {
    let unit = |id: &str, state: &str, decided: &str| {
        format!(
            "INSERT INTO bss.products_approval_unit (id, tenant_id, kind, ref_type, ref_id, state, \
             quorum_required, generation, submitted_by, submitted_at, decided_at, decided_note, \
             snapshot, snapshot_hash, version) VALUES ('{id}', '{T1}', 'sku_change', 'sku', '{S1}', \
             '{state}', 1, 1, gen_random_uuid(), '2026-09-27T09:00:00Z', {decided}, '{{\"name\":\"Renamed\"}}', \
             'h', 2)"
        )
    };
    let item = |id: &str| {
        format!(
            "INSERT INTO bss.products_approval_unit_item (unit_id, tenant_id, item_type, item_id, \
             created_by, after_json) VALUES ('{id}', '{T1}', 'sku', '{S1}', gen_random_uuid(), \
             '{{\"name\":\"Renamed\"}}')"
        )
    };
    vec![
        unit(PENDING, "pending", "NULL, NULL"),
        item(PENDING),
        unit(DECIDED, "rejected", "'2026-09-27T10:00:00Z', 'not now'"),
        item(DECIDED),
        format!(
            "INSERT INTO bss.products_approval_decision (unit_id, tenant_id, actor, generation, \
             decision, note, at, stale) VALUES ('{DECIDED}', '{T1}', gen_random_uuid(), 1, 'reject', \
             'not now', '2026-09-27T10:00:00Z', false)"
        ),
    ]
}

/// Every unit's columns as the deployed database's chain has them, in key order.
const RECORD: &str = "id, tenant_id, kind, ref_type, ref_id, state, common_effective_date, \
     quorum_required, generation, submitted_by, submitted_at, decided_at, decided_note, snapshot, \
     snapshot_hash, version";

async fn rows(pg: &Pg) -> Vec<String> {
    strings(
        pg,
        &format!(
            "SELECT row_to_json(t)::text AS v FROM (SELECT {RECORD} FROM bss.products_approval_unit \
             ORDER BY id) t"
        ),
    )
    .await
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_note_arrives_empty_and_every_unit_survives_on_postgres() {
    let pg = Pg::empty().await;
    let before_run = migrate(&pg, Some(MIGRATION)).await.unwrap();
    assert!(!before_run.applied_names.iter().any(|n| n == MIGRATION));
    for sql in seed() {
        exec(&pg, &sql).await;
    }
    let structure_before = structure(&pg).await;
    let rows_before = rows(&pg).await;
    assert_eq!(rows_before.len(), 2, "{rows_before:#?}");

    let result = migrate(&pg, None).await.unwrap();

    assert_eq!(result.applied_names, [MIGRATION], "only 000009 was pending");
    let structure_after = structure(&pg).await;
    let removed: Vec<&String> = structure_before
        .iter()
        .filter(|f| !structure_after.contains(f))
        .collect();
    let added: Vec<&String> = structure_after
        .iter()
        .filter(|f| !structure_before.contains(f))
        .collect();
    let show = |facts: &[&String]| {
        facts
            .iter()
            .map(|f| f.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    };
    eprintln!(
        "P-D-219 Postgres proof: {} structural facts before, {} after, {} rows\nremoved:\n  {}\nadded:\n  {}",
        structure_before.len(),
        structure_after.len(),
        rows_before.len(),
        show(&removed),
        show(&added)
    );
    assert!(removed.is_empty(), "{removed:#?}");
    assert_eq!(
        added,
        ["column submit_note text nullable=YES default=(none) position=17"]
    );
    assert_eq!(rows(&pg).await, rows_before, "every unit survives");
    assert_eq!(
        strings(
            &pg,
            "SELECT coalesce(submit_note, 'null') AS v FROM bss.products_approval_unit ORDER BY id"
        )
        .await,
        ["null", "null"]
    );

    // The gear reads a unit written before the migration with no note.
    let provider = DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let conn = provider.conn().unwrap();
    let scope = AccessScope::for_tenant(T1);
    let units = repo::list_units(&conn, &scope, T1, None, None, None)
        .await
        .unwrap();
    assert_eq!(units.len(), 2);
    assert!(units.iter().all(|u| u.submit_note.is_none()), "{units:?}");
    let decided = repo::find_unit(&conn, &scope, T1, Uuid::parse_str(DECIDED).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decided.decided_note.as_deref(), Some("not now"));

    // An upgraded database and a fresh one hold the same table; a replay applies nothing.
    let fresh = Pg::empty().await;
    migrate(&fresh, None).await.unwrap();
    assert_eq!(structure(&fresh).await, structure_after);
    assert!(migrate(&pg, None).await.unwrap().applied_names.is_empty());
}
