//! P-D-220 on Postgres: `m20260928_000010_clear_retired_defaults` clears every retired default of
//! `bss.products_category`. The twin of `retired_default_migration.rs`, with the same shape: the
//! gear's whole list without 000010, categories seeded (a tenant whose default was retired before
//! P-D-220, with an active category; a tenant with an active default, with a retired category that
//! is not the default), the structure captured, then the whole list again through the real
//! runner, which applies 000010 alone. The structure is `information_schema.columns`,
//! `pg_constraint` and `pg_indexes` of the table, and none of it may differ. The retired default
//! reads `is_default` false, one version higher and its `updated_at` the migration's instant; no
//! audit row is written; every other row is kept; the gear's repository reads the cleared row; an
//! upgraded database equals a fresh one; a replay applies nothing.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod pg_support;

use bss_products::gear::BssProductsGear;
use bss_products::infra::storage::repo;
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::DBProvider;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use toolkit_db::secure::AccessScope;
use uuid::Uuid;

const MIGRATION: &str = "m20260928_000010_clear_retired_defaults";
const T1: Uuid = Uuid::from_u128(0x7e0a_0000_0000_0000_0000_0000_0010_0001);
const T2: Uuid = Uuid::from_u128(0x7e0a_0000_0000_0000_0000_0000_0010_0002);
const RETIRED_DEFAULT: Uuid = Uuid::from_u128(0x0010_0001);
const T1_ACTIVE: Uuid = Uuid::from_u128(0x0010_0002);
const ACTIVE_DEFAULT: Uuid = Uuid::from_u128(0x0010_0003);
const T2_RETIRED: Uuid = Uuid::from_u128(0x0010_0004);

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

/// The structure of the category table, one fact per line, sorted.
async fn structure(pg: &Pg) -> Vec<String> {
    let mut facts = Vec::new();
    for sql in [
        "SELECT 'column ' || column_name || ' ' || data_type || ' nullable=' || is_nullable || \
         ' default=' || coalesce(column_default, '(none)') || ' position=' || ordinal_position AS v \
         FROM information_schema.columns WHERE table_schema = 'bss' \
         AND table_name = 'products_category'",
        "SELECT 'constraint ' || con.conname || ' ' || con.contype::text || ' ' || \
         pg_get_constraintdef(con.oid) AS v FROM pg_constraint con \
         JOIN pg_class c ON c.oid = con.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = 'bss' AND c.relname = 'products_category'",
        "SELECT 'index ' || indexname || ' ' || indexdef AS v FROM pg_indexes \
         WHERE schemaname = 'bss' AND tablename = 'products_category'",
    ] {
        facts.extend(strings(pg, sql).await);
    }
    facts.sort();
    facts
}

fn seed() -> Vec<String> {
    [
        (RETIRED_DEFAULT, T1, "legacy", true, "retired", 3),
        (T1_ACTIVE, T1, "hosting", false, "active", 1),
        (ACTIVE_DEFAULT, T2, "general", true, "active", 2),
        (T2_RETIRED, T2, "old", false, "retired", 2),
    ]
    .into_iter()
    .map(|(id, tenant, code, is_default, status, version)| {
        format!(
            "INSERT INTO bss.products_category (id, tenant_id, code, name, is_default, sort_order, \
             status, version, created_at, updated_at) VALUES ('{id}', '{tenant}', '{code}', \
             'Category {code}', {is_default}, 0, '{status}', {version}, \
             '2026-09-27T09:00:00.123456Z', '2026-09-27T09:00:00.123456Z')"
        )
    })
    .collect()
}

/// Every category as JSON, by id.
async fn rows(pg: &Pg) -> Vec<(String, serde_json::Value)> {
    strings(
        pg,
        "SELECT row_to_json(t)::text AS v FROM (SELECT * FROM bss.products_category ORDER BY id) t",
    )
    .await
    .into_iter()
    .map(|text| {
        let row: serde_json::Value = serde_json::from_str(&text).unwrap();
        (row["id"].as_str().unwrap().to_owned(), row)
    })
    .collect()
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn every_retired_default_is_cleared_and_every_other_category_kept_on_postgres() {
    let pg = Pg::empty().await;
    let before_run = migrate(&pg, Some(MIGRATION)).await.unwrap();
    assert!(!before_run.applied_names.iter().any(|n| n == MIGRATION));
    for sql in seed() {
        exec(&pg, &sql).await;
    }
    let structure_before = structure(&pg).await;
    let rows_before = rows(&pg).await;
    assert_eq!(rows_before.len(), 4, "{rows_before:#?}");
    let audit = "SELECT count(*)::text AS v FROM bss.products_audit_log";
    assert_eq!(strings(&pg, audit).await, ["0"]);
    let started = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap();

    let result = migrate(&pg, None).await.unwrap();

    assert_eq!(result.applied_names, [MIGRATION], "only 000010 was pending");
    let finished = OffsetDateTime::now_utc();
    assert_eq!(structure(&pg).await, structure_before, "data only");
    let rows_after = rows(&pg).await;
    assert_eq!(rows_after.len(), rows_before.len());
    for ((id, before), (id_after, after)) in rows_before.iter().zip(&rows_after) {
        assert_eq!(id, id_after);
        if *id != RETIRED_DEFAULT.to_string() {
            assert_eq!(after, before, "{id} is kept");
            continue;
        }
        assert_eq!(after["is_default"], false, "{after}");
        assert_eq!(after["version"], 4, "{after}");
        assert_eq!(after["status"], "retired", "{after}");
        let written = strings(
            &pg,
            &format!(
                "SELECT to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') \
                 AS v FROM bss.products_category WHERE id = '{RETIRED_DEFAULT}'"
            ),
        )
        .await;
        let written = OffsetDateTime::parse(&written[0], &Rfc3339).unwrap();
        assert!(
            started <= written && written <= finished,
            "{written} is the migration's instant"
        );
        let (mut kept_before, mut kept_after) = (before.clone(), after.clone());
        for column in ["is_default", "version", "updated_at"] {
            kept_before.as_object_mut().unwrap().remove(column);
            kept_after.as_object_mut().unwrap().remove(column);
        }
        assert_eq!(kept_after, kept_before, "every other column is kept");
    }
    assert_eq!(
        strings(&pg, audit).await,
        ["0"],
        "a migration writes no audit row"
    );

    // The gear reads the cleared category, and its tenant holds no default.
    let provider = DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let conn = provider.conn().unwrap();
    let mut t1: Vec<(Uuid, bool, i64)> =
        repo::list_categories(&conn, &AccessScope::for_tenant(T1), T1)
            .await
            .unwrap()
            .into_iter()
            .map(|c| (c.id, c.is_default, c.version))
            .collect();
    t1.sort();
    assert_eq!(t1, [(RETIRED_DEFAULT, false, 4), (T1_ACTIVE, false, 1)]);
    let t2 = repo::find_category(&conn, &AccessScope::for_tenant(T2), T2, ACTIVE_DEFAULT)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((t2.is_default, t2.version), (true, 2));
    let untouched = repo::find_category(&conn, &AccessScope::for_tenant(T2), T2, T2_RETIRED)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((untouched.is_default, untouched.version), (false, 2));

    // An upgraded database and a fresh one hold the same table; a replay applies nothing.
    let fresh = Pg::empty().await;
    migrate(&fresh, None).await.unwrap();
    assert_eq!(structure(&fresh).await, structure_before);
    assert!(migrate(&pg, None).await.unwrap().applied_names.is_empty());
    assert_eq!(rows(&pg).await, rows_after);
}
