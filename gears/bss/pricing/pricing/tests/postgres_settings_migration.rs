//! D-438 on Postgres: the same forward migration as `settings_migration.rs`, tables in schema
//! `bss`. A database is migrated by the gear's whole list without 000014 through the toolkit
//! runner and holds one tenant's settings row with a legacy rounding; the list runs again and
//! applies 000014 alone. The structure is proved by the Postgres schema dump, before and after and
//! against a fresh chain; the row by its columns; and the application's doors read and rewrite it.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod entry_support;
mod pg_support;
mod schema_dump;

use bss_pricing::module::BssPricingGear;
use entry_support::{Script, app_for, request, state_on, user_of};
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::DBProvider;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use uuid::Uuid;

const MIGRATION: &str = "m20260927_000014_settings_currencies_and_author";
const TENANT: Uuid = Uuid::from_u128(0x7e0a_0000_0000_0000_0000_0000_0014_0002);

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
async fn row(pg: &Pg) -> Value {
    serde_json::from_str(
        &strings(
            pg,
            "SELECT row_to_json(s)::text AS v FROM bss.pricing_settings s",
        )
        .await
        .pop()
        .unwrap(),
    )
    .unwrap()
}
async fn seeded() -> Pg {
    let pg = Pg::empty().await;
    let before = migrate(&pg, Some(MIGRATION)).await.unwrap();
    assert!(
        !before.applied_names.iter().any(|n| n == MIGRATION),
        "the chain before this run"
    );
    pg.raw()
        .await
        .execute_raw(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "INSERT INTO bss.pricing_settings (tenant_id, default_timing, default_rounding, \
                 default_gl, default_tax_category, invoice_line_templates, version, created_at, \
                 updated_at) VALUES ('{TENANT}'::uuid, 'arrears', 'bankers', 'GL-1', 'std', \
                 '{{\"usage\":\"{{sku}} usage\"}}'::jsonb, 3, '2026-09-01T09:00:00Z', \
                 '2026-09-02T10:00:00Z')"
            ),
        ))
        .await
        .unwrap();
    pg
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_the_forward_migration_adds_currencies_and_updated_by_and_keeps_the_row() {
    let pg = seeded().await;
    let dump_before = dump(&pg).await;
    let row_before = row(&pg).await;

    let result = migrate(&pg, None).await.unwrap();
    assert_eq!(result.applied_names, [MIGRATION], "only 000014 was pending");

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
        "D-438 Postgres dump diff\nremoved:\n  {}\nadded:\n  {}",
        show(&removed),
        show(&added)
    );
    assert!(removed.is_empty(), "{removed:#?}");
    assert_eq!(
        added,
        [
            "COLUMN bss.pricing_settings currencies jsonb NOT NULL DEFAULT '[]'::jsonb",
            "COLUMN bss.pricing_settings updated_by uuid NULL DEFAULT -",
        ]
    );
    let mut row_after = row(&pg).await;
    let obj = row_after.as_object_mut().unwrap();
    assert_eq!(obj.remove("currencies"), Some(json!([])));
    assert_eq!(obj.remove("updated_by"), Some(json!(null)));
    assert_eq!(row_after, row_before, "every old column as it was");
    let fresh = Pg::applied().await;
    assert_eq!(dump_after, dump(&fresh).await, "upgraded and fresh agree");
    assert!(migrate(&pg, None).await.unwrap().applied_names.is_empty());

    // Through the application: the legacy row reads, keeps its rounding only if it is rewritten
    // with a mode of the set, and the PUT stamps the writer.
    let state = state_on(DBProvider::new(pg.db().await), Arc::new(Script::default())).await;
    let app = app_for(state, TENANT);
    let ctx = user_of(TENANT);
    let (s, read, tag) = request(&app, &ctx, "GET", "/settings", json!({}), None, None).await;
    assert_eq!(s, 200, "{read}");
    assert_eq!(tag, "\"3\"");
    assert_eq!(read["currencies"], json!([]));
    assert!(read["updated_by"].is_null(), "{read}");
    assert!(read["updated_at"].is_string(), "{read}");
    let mut body = read.clone();
    for field in ["version", "updated_at", "updated_by"] {
        body.as_object_mut().unwrap().remove(field);
    }
    let (s, refused, _) = request(
        &app,
        &ctx,
        "PUT",
        "/settings",
        body.clone(),
        Some(&tag),
        None,
    )
    .await;
    assert_eq!(s, 400, "{refused}");
    assert!(
        refused.to_string().contains("ROUNDING_INVALID"),
        "{refused}"
    );
    body["default_rounding"] = json!("half_even");
    body["currencies"] = json!(["EUR", "USD"]);
    let (s, saved, _) = request(&app, &ctx, "PUT", "/settings", body, Some(&tag), None).await;
    assert_eq!(s, 200, "{saved}");
    assert_eq!(saved["currencies"], json!(["EUR", "USD"]));
    assert_eq!(saved["updated_by"], ctx.subject_id().to_string());
    let (s, refused) = {
        let (s, b, _) = request(
            &app,
            &ctx,
            "POST",
            "/price-books",
            json!({"code":"gbp","name":"gbp","currency":"GBP"}),
            None,
            Some("gbp"),
        )
        .await;
        (s, b)
    };
    assert_eq!(s, 409, "{refused}");
    assert!(
        refused.to_string().contains("CURRENCY_NOT_OFFERED"),
        "{refused}"
    );
}
