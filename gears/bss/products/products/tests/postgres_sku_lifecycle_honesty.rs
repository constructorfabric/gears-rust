//! Migration `m20261001_000011_sku_lifecycle_honesty` on Postgres, and the effective lifecycle
//! the counts group by (P-D-248, P-D-249).
//!
//! A seeded `retiring` row becomes its prior lifecycle with `retire_pending`. `fence_prior_lifecycle`
//! is gone. The lifecycle CHECK refuses `retiring`. `retire_pending` requires `fenced_at`, and
//! `lifecycle_next` is a pair that is never `retired`. A due next is the lifecycle the list and the
//! counts use, and `GROUP BY 1` does not raise 42803 on that branch.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod pg_support;

use bss_products::domain::{category::NewCategory, sku::NewSku};
use bss_products::gear::BssProductsGear;
use bss_products::infra::storage::repo::{self, SkuCounts, SkuListFilter};
use bss_products_sdk::models::{Lifecycle, SkuType};
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use toolkit_db::secure::AccessScope;
use toolkit_odata::ODataQuery;
use uuid::Uuid;

const MIGRATION: &str = "m20261001_000011_sku_lifecycle_honesty";
const T1: &str = "00000000-0000-0000-0000-000000000011";
const CAT: &str = "00000000-0000-0000-0000-0000000000c1";
const RETIRING: &str = "00000000-0000-0000-0000-0000000000a1";
const LIVE: &str = "00000000-0000-0000-0000-0000000000a2";
const ACTOR: &str = "00000000-0000-0000-0000-0000000000b1";

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
    try_exec(pg, sql)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn try_exec(pg: &Pg, sql: &str) -> Result<(), String> {
    let raw = pg.raw().await;
    let result = raw
        .execute_raw(Statement::from_string(DbBackend::Postgres, sql.to_owned()))
        .await
        .map(|_| ())
        .map_err(|e| e.to_string());
    raw.close().await.unwrap();
    result
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

fn seed() -> Vec<String> {
    vec![
        format!(
            "INSERT INTO bss.products_category (id, tenant_id, code, name, status, created_at, updated_at) \
             VALUES ('{CAT}', '{T1}', 'storage', 'Storage', 'active', '2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z')"
        ),
        format!(
            "INSERT INTO bss.products_sku (id, tenant_id, code, name, type, category_id, lifecycle, \
             fence_prior_lifecycle, fenced_at, fence_op_id, created_by, created_at, updated_at) \
             VALUES ('{RETIRING}', '{T1}', 'RET', 'Retiring', 'recurring', '{CAT}', 'retiring', \
             'published', '2026-09-01T00:00:00Z', '{ACTOR}', '{ACTOR}', '2026-09-01T00:00:00Z', \
             '2026-09-01T00:00:00Z')"
        ),
        format!(
            "INSERT INTO bss.products_sku (id, tenant_id, code, name, type, category_id, lifecycle, \
             created_by, created_at, updated_at) VALUES ('{LIVE}', '{T1}', 'LIVE', 'Live', 'recurring', \
             '{CAT}', 'deprecated', '{ACTOR}', '2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z')"
        ),
    ]
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_legacy_retiring_row_converts_and_the_checks_hold_on_postgres() {
    let pg = Pg::empty().await;
    let before = migrate(&pg, Some(MIGRATION)).await.unwrap();
    assert!(!before.applied_names.iter().any(|n| n == MIGRATION));
    for sql in seed() {
        exec(&pg, &sql).await;
    }
    let result = migrate(&pg, None).await.unwrap();
    assert_eq!(result.applied_names, [MIGRATION]);

    assert_eq!(
        strings(
            &pg,
            &format!(
                "SELECT lifecycle || ' ' || retire_pending::text AS v FROM bss.products_sku \
                 WHERE id = '{RETIRING}'"
            ),
        )
        .await,
        ["published true"]
    );
    assert_eq!(
        strings(
            &pg,
            &format!(
                "SELECT lifecycle || ' ' || retire_pending::text AS v FROM bss.products_sku \
                 WHERE id = '{LIVE}'"
            ),
        )
        .await,
        ["deprecated false"]
    );
    assert!(
        strings(
            &pg,
            "SELECT column_name AS v FROM information_schema.columns \
             WHERE table_schema = 'bss' AND table_name = 'products_sku' \
             AND column_name = 'fence_prior_lifecycle'",
        )
        .await
        .is_empty()
    );

    for sql in [
        format!("UPDATE bss.products_sku SET lifecycle = 'retiring' WHERE id = '{LIVE}'"),
        format!("UPDATE bss.products_sku SET retire_pending = true WHERE id = '{LIVE}'"),
        format!(
            "UPDATE bss.products_sku SET lifecycle_next = 'retired', lifecycle_next_from = '2026-11-01' \
             WHERE id = '{LIVE}'"
        ),
        format!("UPDATE bss.products_sku SET lifecycle_next = 'deprecated' WHERE id = '{LIVE}'"),
    ] {
        assert!(
            try_exec(&pg, &sql).await.is_err(),
            "{sql} must fail a CHECK"
        );
    }
    exec(
        &pg,
        &format!(
            "UPDATE bss.products_sku SET lifecycle_next = 'deprecated', lifecycle_next_from = '2026-11-01' \
             WHERE id = '{LIVE}'"
        ),
    )
    .await;
    assert_eq!(
        strings(
            &pg,
            &format!("SELECT lifecycle_next AS v FROM bss.products_sku WHERE id = '{LIVE}'"),
        )
        .await,
        ["deprecated"]
    );
}

/// A due `lifecycle_next` is the lifecycle in force on the list and in the counts. The counts'
/// `GROUP BY 1` is the proof Postgres does not split the `CASE` into two expressions.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_due_lifecycle_next_counts_and_filters_as_the_effective_lifecycle_on_postgres() {
    let pg = Pg::applied().await;
    let db = pg.db().await;
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let now = bss_products::infra::storage::stored_now();
    let category = repo::insert_category(
        &db.conn().unwrap(),
        &scope,
        tenant,
        NewCategory {
            code: "hosting".into(),
            name: "Hosting".into(),
            is_default: false,
            sort_order: 0,
        },
        now,
    )
    .await
    .unwrap()
    .id;
    let head = |code: &str| {
        let code = code.to_owned();
        NewSku {
            code: code.clone(),
            name: code,
            r#type: SkuType::Recurring,
            category_id: Some(category),
            description: String::new(),
            sellable: true,
            gl_code: None,
            tax_category: None,
            invoice_line_template: None,
            billing_timing: None,
            usage_type_ref: None,
            unit: None,
        }
    };
    let due = repo::insert_sku(
        &db.conn().unwrap(),
        &scope,
        tenant,
        head("DUE"),
        tenant,
        now,
    )
    .await
    .unwrap()
    .id;
    let stay = repo::insert_sku(
        &db.conn().unwrap(),
        &scope,
        tenant,
        head("STAY"),
        tenant,
        now,
    )
    .await
    .unwrap()
    .id;
    for id in [due, stay] {
        repo::set_lifecycle(
            &db.conn().unwrap(),
            &scope,
            tenant,
            id,
            &[Lifecycle::Draft],
            Lifecycle::Published,
            now,
        )
        .await
        .unwrap();
    }
    repo::set_lifecycle_next(
        &db.conn().unwrap(),
        &scope,
        tenant,
        due,
        &[Lifecycle::Published],
        Lifecycle::Deprecated,
        now.date(),
        now,
    )
    .await
    .unwrap();

    let counts = repo::count_skus(
        &db.conn().unwrap(),
        &scope,
        tenant,
        DbBackend::Postgres,
        &SkuListFilter::default(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        counts,
        SkuCounts {
            all: 2,
            draft: 0,
            published: 1,
            deprecated: 1,
            retired: 0,
            in_review: 0,
            archived: 0,
        }
    );
    let filter = ODataQuery::default().with_filter(
        toolkit_odata::parse_filter_string("lifecycle eq 'deprecated'")
            .unwrap()
            .into_expr(),
    );
    let page = repo::page_skus(
        &db.conn().unwrap(),
        &scope,
        tenant,
        DbBackend::Postgres,
        &SkuListFilter::default(),
        &filter,
    )
    .await
    .unwrap();
    let codes: Vec<_> = page.items.iter().map(|s| s.code.as_str()).collect();
    assert_eq!(codes, ["DUE"]);
    assert_eq!(page.items[0].lifecycle, Lifecycle::Deprecated);
}
