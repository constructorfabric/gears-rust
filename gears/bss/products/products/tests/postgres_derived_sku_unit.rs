//! P-D-259 on Postgres: `m20261002_000013_derived_sku_unit` nulls a derived SKU's stored unit
//! only when it equals the version's `output_unit`, then the CHECK holds. The twin is the
//! migration's `_tests.rs` on SQLite.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod pg_support;

use bss_products::gear::BssProductsGear;
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};

const MIGRATION: &str = "m20261002_000013_derived_sku_unit";
const TENANT: &str = "00000000-0000-0000-0000-0000000000a1";
const TYPE_ID: &str = "00000000-0000-0000-0000-0000000000d1";
const ACTOR: &str = "00000000-0000-0000-0000-000000000007";
const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

async fn migrate(pg: &Pg, without: Option<&str>) -> Result<MigrationResult, MigrationError> {
    let db = pg.db().await;
    let chain = BssProductsGear::default()
        .migrations()
        .into_iter()
        .filter(|migration| Some(migration.name()) != without)
        .collect();
    run_migrations_for_testing(&db, chain).await
}

async fn exec(pg: &Pg, statements: &[&str]) {
    let raw = pg.raw().await;
    for sql in statements {
        raw.execute_raw(Statement::from_string(
            DbBackend::Postgres,
            (*sql).to_owned(),
        ))
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    raw.close().await.unwrap();
}

async fn unit_of(pg: &Pg, id: &str) -> Option<String> {
    let raw = pg.raw().await;
    let rows = raw
        .query_all_raw(Statement::from_string(
            DbBackend::Postgres,
            format!("SELECT unit AS v FROM bss.products_sku WHERE id = '{id}'"),
        ))
        .await
        .unwrap();
    let unit = rows.first().unwrap().try_get("", "v").unwrap();
    raw.close().await.unwrap();
    unit
}

fn seed(sku_id: &str, code: &str, reference: &str, unit: &str) -> Vec<String> {
    vec![
        format!(
            "INSERT INTO bss.products_derived_usage_type \
             (tenant_id, id, code, name, created_by, created_at) VALUES \
             ('{TENANT}','{TYPE_ID}','meter','Meter','{ACTOR}','2026-10-02T00:00:00Z')"
        ),
        format!(
            "INSERT INTO bss.products_derived_usage_type_version \
             (tenant_id, type_id, version, declaration_json, digest, created_by, created_at) \
             VALUES ('{TENANT}','{TYPE_ID}',1,'{{\"output_unit\":\"GB\"}}','{DIGEST}','{ACTOR}',\
             '2026-10-02T00:00:00Z')"
        ),
        format!(
            "INSERT INTO bss.products_sku \
             (id, tenant_id, code, name, type, lifecycle, usage_type_ref, unit, created_by, \
             created_at, updated_at) VALUES \
             ('{sku_id}','{TENANT}','{code}','{code}','usage','published','{reference}','{unit}',\
             '{ACTOR}','2026-10-02T00:00:00Z','2026-10-02T00:00:00Z')"
        ),
    ]
}

/// An agreeing stored unit is nulled. A raw SKU keeps its unit. A later derived write of a unit
/// is refused by `chk_products_sku_derived_unit`.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_agreeing_derived_sku_loses_its_stored_unit_on_postgres() {
    let pg = Pg::empty().await;
    let before = migrate(&pg, Some(MIGRATION)).await.unwrap();
    assert!(!before.applied_names.iter().any(|name| name == MIGRATION));
    let rows = seed(
        "00000000-0000-0000-0000-0000000000b1",
        "agree",
        "products.derived/meter@1",
        "GB",
    );
    let mut statements: Vec<&str> = rows.iter().map(String::as_str).collect();
    let raw = format!(
        "INSERT INTO bss.products_sku \
         (id, tenant_id, code, name, type, lifecycle, usage_type_ref, unit, created_by, \
         created_at, updated_at) VALUES \
         ('00000000-0000-0000-0000-0000000000b3','{TENANT}','raw','raw','usage','published',\
         'usage:storage','GB','{ACTOR}','2026-10-02T00:00:00Z','2026-10-02T00:00:00Z')"
    );
    statements.push(&raw);
    exec(&pg, &statements).await;

    let result = migrate(&pg, None).await.unwrap();
    assert_eq!(result.applied_names, [MIGRATION]);
    assert_eq!(
        unit_of(&pg, "00000000-0000-0000-0000-0000000000b1").await,
        None
    );
    assert_eq!(
        unit_of(&pg, "00000000-0000-0000-0000-0000000000b3")
            .await
            .as_deref(),
        Some("GB")
    );
    let refused = {
        let conn = pg.raw().await;
        let error = conn
            .execute_raw(Statement::from_string(
                DbBackend::Postgres,
                seed(
                    "00000000-0000-0000-0000-0000000000b4",
                    "later",
                    "products.derived/meter@1",
                    "GB",
                )[2]
                .clone(),
            ))
            .await
            .unwrap_err()
            .to_string();
        conn.close().await.unwrap();
        error
    };
    assert!(
        refused.contains("chk_products_sku_derived_unit"),
        "{refused}"
    );
}

/// A stored unit that is not the version's output unit refuses the migration and stays.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_disagreeing_derived_sku_refuses_the_migration_on_postgres() {
    let pg = Pg::empty().await;
    migrate(&pg, Some(MIGRATION)).await.unwrap();
    let rows = seed(
        "00000000-0000-0000-0000-0000000000b2",
        "disagree",
        "products.derived/meter@1",
        "MB",
    );
    exec(&pg, &rows.iter().map(String::as_str).collect::<Vec<_>>()).await;
    let error = migrate(&pg, None).await.unwrap_err().to_string();
    assert!(
        error.contains("00000000-0000-0000-0000-0000000000b2"),
        "{error}"
    );
    assert_eq!(
        unit_of(&pg, "00000000-0000-0000-0000-0000000000b2")
            .await
            .as_deref(),
        Some("MB")
    );
}
