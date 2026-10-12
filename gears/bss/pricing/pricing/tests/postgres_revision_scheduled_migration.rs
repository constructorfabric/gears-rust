//! D-446 on Postgres, tables in schema `bss`: the forward migration
//! `m20260929_000017_revision_scheduled` over a deployed database, the family seeded through
//! the gear's repositories (two plans, revisions superseded, published, pending and draft, items
//! with and without an entry). The twin of `revision_scheduled_migration.rs`: the dump differs by
//! the CHECK and the new index only, every row and key survives, the CHECK admits `scheduled` and
//! refuses a stranger, the new index refuses a second scheduled revision, an upgraded database
//! equals a fresh one, and a replay applies nothing.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod pg_support;
mod scheduled_support;
mod schema_dump;

use bss_pricing::infra::storage::repo::plan_revision_repo;
use bss_pricing::module::BssPricingGear;
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::Value;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::DBProvider;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use toolkit_db::secure::AccessScope;
use uuid::Uuid;

const MIGRATION: &str = "m20260929_000017_revision_scheduled";
const TENANT: Uuid = Uuid::from_u128(0x7e0a_0000_0000_0000_0000_0000_0017_0002);

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
/// Every row of the family, as JSON, parent first, by id.
async fn rows(pg: &Pg) -> Vec<Value> {
    let mut rows = Vec::new();
    for table in ["pricing_plan_revision", "pricing_plan_item"] {
        rows.extend(
            strings(
                pg,
                &format!("SELECT row_to_json(t)::text AS v FROM bss.{table} t ORDER BY id"),
            )
            .await
            .iter()
            .map(|r| serde_json::from_str::<Value>(r).unwrap()),
        );
    }
    rows
}
async fn exec(pg: &Pg, sql: &str) -> Result<(), String> {
    pg.raw()
        .await
        .execute_raw(Statement::from_string(DbBackend::Postgres, sql.to_owned()))
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}
async fn seeded() -> (Pg, scheduled_support::Family) {
    let pg = Pg::empty().await;
    let before = migrate(&pg, Some(MIGRATION)).await.unwrap();
    assert!(
        !before.applied_names.iter().any(|n| n == MIGRATION),
        "the chain before this run"
    );
    let family = scheduled_support::seed(pg.db().await, TENANT).await;
    (pg, family)
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_the_forward_migration_widens_the_state_check_and_keeps_every_row() {
    let (pg, family) = seeded().await;
    let dump_before = dump(&pg).await;
    let rows_before = rows(&pg).await;
    assert_eq!(rows_before.len(), 4 + family.items);

    let result = migrate(&pg, None).await.unwrap();
    assert_eq!(result.applied_names, [MIGRATION], "only 000017 was pending");

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
        "000017 Postgres dump diff\nremoved:\n  {}\nadded:\n  {}",
        show(&removed),
        show(&added)
    );
    assert_eq!(
        removed,
        [
            "CONSTRAINT bss.pricing_plan_revision chk_pricing_plan_revision_state CHECK ((state = \
             ANY (ARRAY['draft'::text, 'pending'::text, 'published'::text, 'superseded'::text])))"
        ]
    );
    assert_eq!(
        added,
        [
            "CONSTRAINT bss.pricing_plan_revision chk_pricing_plan_revision_state CHECK ((state = \
             ANY (ARRAY['draft'::text, 'pending'::text, 'scheduled'::text, 'published'::text, \
             'superseded'::text])))",
            "INDEX bss pricing_plan_revision_scheduled CREATE UNIQUE INDEX \
             pricing_plan_revision_scheduled ON bss.pricing_plan_revision USING btree (plan_id) \
             WHERE (state = 'scheduled'::text)",
        ]
    );
    assert_eq!(rows(&pg).await, rows_before, "every row survives");
    let fresh = Pg::applied().await;
    assert_eq!(dump_after, dump(&fresh).await, "upgraded and fresh agree");
    assert!(migrate(&pg, None).await.unwrap().applied_names.is_empty());

    // The gear schedules through its own write; the CHECK and the three indexes still refuse.
    let provider = DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let conn = provider.conn().unwrap();
    plan_revision_repo::schedule(
        &conn,
        &AccessScope::for_tenant(TENANT),
        TENANT,
        family.pending,
        family.pending_unit,
        scheduled_support::at(14),
    )
    .await
    .unwrap();
    for (sql, refusal) in [
        (
            format!(
                "UPDATE bss.pricing_plan_revision SET state = 'scheduled' WHERE id = '{}'",
                family.published
            ),
            "pricing_plan_revision_scheduled",
        ),
        (
            format!(
                "UPDATE bss.pricing_plan_revision SET state = 'draft' WHERE id = '{}'",
                family.pending
            ),
            "",
        ),
        (
            format!(
                "UPDATE bss.pricing_plan_revision SET state = 'pending' WHERE id = '{}'",
                family.superseded
            ),
            "pricing_plan_revision_open",
        ),
        (
            format!(
                "UPDATE bss.pricing_plan_revision SET state = 'published' WHERE id = '{}'",
                family.superseded
            ),
            "pricing_plan_revision_published",
        ),
        (
            format!(
                "UPDATE bss.pricing_plan_revision SET state = 'retired' WHERE id = '{}'",
                family.draft
            ),
            "chk_pricing_plan_revision_state",
        ),
        (
            format!(
                "DELETE FROM bss.pricing_plan_revision WHERE id = '{}'",
                family.draft
            ),
            "pricing_plan_item_revision_id_fkey",
        ),
    ] {
        match exec(&pg, &sql).await {
            Ok(()) => assert!(refusal.is_empty(), "{sql} was not refused"),
            Err(error) => assert!(
                !refusal.is_empty() && error.contains(refusal),
                "{sql}\n{error}"
            ),
        }
    }
    let states = |plan: Uuid| {
        let pg = pg.clone();
        async move {
            strings(
                &pg,
                &format!(
                    "SELECT state AS v FROM bss.pricing_plan_revision WHERE plan_id = '{plan}' \
                     ORDER BY rev_no"
                ),
            )
            .await
        }
    };
    assert_eq!(
        states(family.alpha).await,
        ["superseded", "published", "draft"]
    );
    assert_eq!(states(family.beta).await, ["draft"]);
}
