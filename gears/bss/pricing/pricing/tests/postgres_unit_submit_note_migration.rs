//! D-445 on Postgres, tables in schema `bss`: the forward migration
//! `m20260928_000016_unit_submit_note` over a deployed database, approval units seeded. The
//! twin of `unit_submit_note_migration.rs`: the dump differs by the one column, every unit survives
//! and reads a null note in SQL and through the gear's repository, an upgraded database equals a
//! fresh one, and a replay applies nothing.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod pg_support;
mod schema_dump;

use bss_pricing::infra::storage::repo::approval_repo;
use bss_pricing::module::BssPricingGear;
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::Value;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::DBProvider;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use toolkit_db::secure::AccessScope;
use uuid::Uuid;

const MIGRATION: &str = "m20260928_000016_unit_submit_note";
const TENANT: Uuid = Uuid::from_u128(0x7e0a_0000_0000_0000_0000_0000_0016_0002);
const BOOK: Uuid = Uuid::from_u128(0x0016_0b00);
const PENDING: Uuid = Uuid::from_u128(0x0016_0001);
const REJECTED: Uuid = Uuid::from_u128(0x0016_0002);

async fn migrate(pg: &Pg, without: Option<&str>) -> Result<MigrationResult, MigrationError> {
    let chain = BssPricingGear::default()
        .migrations()
        .into_iter()
        .filter(|m| Some(m.name()) != without)
        .collect();
    run_migrations_for_testing(&pg.db().await, chain).await
}
async fn strings(pg: &Pg, sql: &str) -> Vec<String> {
    pg.raw()
        .await
        .query_all_raw(Statement::from_string(DbBackend::Postgres, sql.to_owned()))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .iter()
        .map(|row| row.try_get::<String>("", "v").unwrap())
        .collect()
}
async fn dump(pg: &Pg) -> Vec<String> {
    schema_dump::postgres_dump(&pg.raw().await)
        .await
        .lines()
        .map(str::to_owned)
        .collect()
}
async fn rows(pg: &Pg) -> Vec<Value> {
    strings(
        pg,
        "SELECT row_to_json(u)::text AS v FROM bss.pricing_approval_unit u ORDER BY id",
    )
    .await
    .iter()
    .map(|r| serde_json::from_str(r).unwrap())
    .collect()
}
async fn exec(pg: &Pg, sql: &str) {
    pg.raw()
        .await
        .execute_raw(Statement::from_string(DbBackend::Postgres, sql.to_owned()))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}
async fn seeded() -> Pg {
    let pg = Pg::empty().await;
    let before = migrate(&pg, Some(MIGRATION)).await.unwrap();
    assert!(
        !before.applied_names.iter().any(|n| n == MIGRATION),
        "the chain before this run"
    );
    for (id, state, decided) in [
        (PENDING, "pending", "NULL, NULL"),
        (REJECTED, "rejected", "'2026-09-27T10:00:00Z', 'too cheap'"),
    ] {
        exec(
            &pg,
            &format!(
                "INSERT INTO bss.pricing_approval_unit (id, tenant_id, kind, ref_type, ref_id, \
                 state, quorum_required, generation, submitted_by, submitted_at, decided_at, \
                 decided_note, snapshot, snapshot_hash, version) VALUES ('{id}', '{TENANT}', \
                 'prices', 'price_book', '{BOOK}', '{state}', 1, 1, gen_random_uuid(), \
                 '2026-09-27T09:00:00.123456Z', {decided}, '{{\"prices\":1}}', 'h', 2)"
            ),
        )
        .await;
        exec(
            &pg,
            &format!(
                "INSERT INTO bss.pricing_approval_unit_item (unit_id, tenant_id, item_type, \
                 item_id, created_by, after_json) VALUES ('{id}', '{TENANT}', 'price', \
                 gen_random_uuid(), gen_random_uuid(), '{{\"amount\":\"10\"}}')"
            ),
        )
        .await;
    }
    exec(
        &pg,
        &format!(
            "INSERT INTO bss.pricing_approval_decision (unit_id, tenant_id, actor, generation, \
             decision, note, at, stale) VALUES ('{REJECTED}', '{TENANT}', gen_random_uuid(), 1, \
             'reject', 'too cheap', '2026-09-27T10:00:00Z', false)"
        ),
    )
    .await;
    pg
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_the_forward_migration_adds_the_note_and_keeps_every_unit() {
    let pg = seeded().await;
    let dump_before = dump(&pg).await;
    let rows_before = rows(&pg).await;
    assert_eq!(rows_before.len(), 2);

    let result = migrate(&pg, None).await.unwrap();
    assert_eq!(result.applied_names, [MIGRATION], "only 000016 was pending");

    let dump_after = dump(&pg).await;
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
        "000016 Postgres dump diff\nremoved:\n  {}\nadded:\n  {}",
        show(&removed),
        show(&added)
    );
    assert!(removed.is_empty(), "{removed:#?}");
    assert_eq!(
        added,
        ["COLUMN bss.pricing_approval_unit submit_note text NULL DEFAULT -"]
    );
    let rows_after: Vec<Value> = rows(&pg)
        .await
        .into_iter()
        .map(|mut row| {
            assert_eq!(
                row.as_object_mut().unwrap().remove("submit_note"),
                Some(Value::Null)
            );
            row
        })
        .collect();
    assert_eq!(rows_after, rows_before, "every old column as it was");
    let fresh = Pg::applied().await;
    assert_eq!(dump_after, dump(&fresh).await, "upgraded and fresh agree");
    assert!(migrate(&pg, None).await.unwrap().applied_names.is_empty());

    // The gear reads a unit written before the migration with no note.
    let provider = DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let conn = provider.conn().unwrap();
    let scope = AccessScope::for_tenant(TENANT);
    let units = approval_repo::list_units(&conn, &scope, TENANT, None, None, Some(BOOK))
        .await
        .unwrap();
    assert_eq!(units.len(), 2);
    assert!(units.iter().all(|u| u.submit_note.is_none()), "{units:?}");
    let rejected = approval_repo::find_unit(&conn, &scope, TENANT, REJECTED)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rejected.decided_note.as_deref(), Some("too cheap"));
}
