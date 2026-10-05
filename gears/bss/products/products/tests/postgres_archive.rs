#![allow(clippy::expect_used, clippy::unwrap_used)]
//! P-D-263 on `PostgreSQL`: `m20261003_000014_archive_mark` adds the archive mark to SKUs and
//! categories and takes it away again, and the SKU list, its counts and the category list hide an
//! archived row by default on the engine production runs. The `SQLite` twin of the migration is
//! its `_tests.rs`; the doors' twin is `src/api/rest/archive_tests.rs`.
mod pg_support;

use bss_products::{
    domain::{category::NewCategory, sku::NewSku},
    gear::BssProductsGear,
    infra::storage::repo::{self, HeadWrite, SkuListFilter},
};
use bss_products_sdk::models::{Lifecycle, SkuType};
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use time::OffsetDateTime;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::AccessScope;
use toolkit_odata::ODataQuery;
use uuid::Uuid;

const MIGRATION: &str = "m20261003_000014_archive_mark";

async fn strings(pg: &Pg, sql: &str) -> Vec<String> {
    let raw = pg.raw().await;
    let rows = raw
        .query_all_raw(Statement::from_string(DbBackend::Postgres, sql.to_owned()))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
    let out = rows
        .iter()
        .map(|row| row.try_get::<String>("", "v").unwrap())
        .collect();
    raw.close().await.unwrap();
    out
}

async fn columns(pg: &Pg, table: &str) -> Vec<String> {
    strings(
        pg,
        &format!(
            "SELECT column_name::text AS v FROM information_schema.columns \
             WHERE table_schema = 'bss' AND table_name = '{table}' ORDER BY column_name"
        ),
    )
    .await
}

/// Run `sql` on the test's database; `Err` is the server's refusal.
async fn run(pg: &Pg, sql: &str) -> Result<(), sea_orm::DbErr> {
    let raw = pg.raw().await;
    let result = raw
        .execute_raw(Statement::from_string(DbBackend::Postgres, sql.to_owned()))
        .await
        .map(|_| ());
    raw.close().await.unwrap();
    result
}

/// The archive-mark CHECKs on the two tables.
async fn mark_checks(pg: &Pg) -> Vec<String> {
    strings(
        pg,
        "SELECT conname::text AS v FROM pg_constraint WHERE contype = 'c' \
         AND conname LIKE '%archive_mark' ORDER BY conname",
    )
    .await
}

async fn index_names(pg: &Pg) -> Vec<String> {
    strings(
        pg,
        "SELECT indexname::text AS v FROM pg_indexes WHERE schemaname = 'bss' \
         AND tablename = 'products_sku' ORDER BY indexname",
    )
    .await
}

/// Up adds the four columns, the partial index in the default page's order and the CHECKs that
/// pair each mark; a half mark is refused; down takes them away; up runs again.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_archive_mark_migrates_up_and_down_on_postgres() {
    let pg = Pg::empty().await;
    let db = pg.db().await;
    let without: Vec<_> = BssProductsGear::default()
        .migrations()
        .into_iter()
        .filter(|m| m.name() != MIGRATION)
        .collect();
    run_migrations_for_testing(&db, without).await.unwrap();
    let sku_before = columns(&pg, "products_sku").await;
    let category_before = columns(&pg, "products_category").await;
    let indexes_before = index_names(&pg).await;
    let applied = run_migrations_for_testing(&db, BssProductsGear::default().migrations())
        .await
        .unwrap();
    assert_eq!(applied.applied_names, [MIGRATION]);
    for table in ["products_sku", "products_category"] {
        let cols = columns(&pg, table).await;
        for name in ["archived_at", "archived_by"] {
            assert!(cols.iter().any(|c| c == name), "{table}: {cols:?}");
        }
    }
    let types = strings(
        &pg,
        "SELECT data_type::text AS v FROM information_schema.columns WHERE table_schema = 'bss' \
         AND table_name = 'products_sku' AND column_name IN ('archived_at','archived_by') \
         ORDER BY column_name",
    )
    .await;
    assert_eq!(types, ["timestamp with time zone", "uuid"]);
    assert_eq!(
        strings(
            &pg,
            "SELECT indexdef::text AS v FROM pg_indexes WHERE schemaname = 'bss' \
             AND indexname = 'ix_products_sku_unarchived'",
        )
        .await,
        [
            "CREATE INDEX ix_products_sku_unarchived ON bss.products_sku USING btree (tenant_id, code) \
          WHERE (archived_at IS NULL)"
        ]
    );
    assert_eq!(
        mark_checks(&pg).await,
        [
            "chk_products_category_archive_mark",
            "chk_products_sku_archive_mark"
        ]
    );
    let tenant = Uuid::new_v4();
    for half in ["now(), NULL", &format!("NULL, '{}'", Uuid::new_v4())] {
        let refused = run(
            &pg,
            &format!(
                "INSERT INTO bss.products_category (id, tenant_id, code, name, is_default, \
                 sort_order, status, version, created_at, updated_at, archived_at, archived_by) \
                 VALUES ('{}', '{tenant}', 'half', 'Half', false, 0, 'retired', 1, now(), now(), \
                 {half})",
                Uuid::new_v4()
            ),
        )
        .await;
        assert!(refused.is_err(), "a half category mark is refused: {half}");
        let refused = run(
            &pg,
            &format!(
                "INSERT INTO bss.products_sku (id, tenant_id, code, name, type, description, \
                 sellable, lifecycle, created_by, created_at, updated_at, archived_at, \
                 archived_by) VALUES ('{}', '{tenant}', 'half', 'Half', 'recurring', '', true, \
                 'retired', '{tenant}', now(), now(), {half})",
                Uuid::new_v4()
            ),
        )
        .await;
        assert!(refused.is_err(), "a half SKU mark is refused: {half}");
    }
    let raw = pg.raw().await;
    let manager = sea_orm_migration::SchemaManager::new(&raw);
    let migration = BssProductsGear::default()
        .migrations()
        .into_iter()
        .find(|m| m.name() == MIGRATION)
        .unwrap();
    migration.down(&manager).await.unwrap();
    assert_eq!(columns(&pg, "products_sku").await, sku_before);
    assert_eq!(columns(&pg, "products_category").await, category_before);
    assert_eq!(index_names(&pg).await, indexes_before);
    assert!(mark_checks(&pg).await.is_empty());
    migration.up(&manager).await.unwrap();
    raw.close().await.unwrap();
    assert!(
        columns(&pg, "products_sku")
            .await
            .iter()
            .any(|c| c == "archived_by")
    );
}

/// On the engine production runs: an archived retired SKU leaves the page and every count but
/// `archived`; `archived eq true` pages it; a stale revision writes nothing; the category list
/// hides an archived category, and `archived eq true` shows it.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_archived_row_leaves_its_list_and_its_counts_on_postgres() {
    let pg = Pg::applied().await;
    let db = pg.db().await;
    let conn = db.conn().unwrap();
    let tenant = Uuid::new_v4();
    let actor = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let now = OffsetDateTime::now_utc();
    let mut ids = Vec::new();
    for (code, lifecycle) in [("LIVE", Lifecycle::Published), ("GONE", Lifecycle::Retired)] {
        let s = repo::insert_sku(
            &conn,
            &scope,
            tenant,
            NewSku {
                code: code.into(),
                name: code.into(),
                r#type: SkuType::Recurring,
                category_id: None,
                description: String::new(),
                sellable: true,
                gl_code: None,
                tax_category: None,
                invoice_line_template: None,
                billing_timing: None,
                usage_type_ref: None,
                unit: None,
            },
            actor,
            now,
        )
        .await
        .unwrap();
        repo::set_lifecycle(
            &conn,
            &scope,
            tenant,
            s.id,
            &[Lifecycle::Draft],
            lifecycle,
            now,
        )
        .await
        .unwrap();
        ids.push(s.id);
    }
    let gone = repo::find_sku(&conn, &scope, tenant, ids[1])
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        repo::set_sku_archived(
            &conn,
            &scope,
            tenant,
            gone.id,
            gone.revision + 7,
            Some(actor),
            now
        )
        .await
        .unwrap(),
        HeadWrite::Unmatched
    ));
    let HeadWrite::Written(archived) = repo::set_sku_archived(
        &conn,
        &scope,
        tenant,
        gone.id,
        gone.revision,
        Some(actor),
        now,
    )
    .await
    .unwrap() else {
        panic!("the archive matched")
    };
    assert_eq!(archived.archived_by, Some(actor));
    assert!(archived.archived_at.is_some());
    assert_eq!(archived.revision, gone.revision + 1);

    let page = |filter: Option<&'static str>| {
        let conn = db.conn().unwrap();
        let scope = scope.clone();
        async move {
            let mut query = ODataQuery::default();
            if let Some(raw) = filter {
                query =
                    query.with_filter(toolkit_odata::parse_filter_string(raw).unwrap().into_expr());
            }
            let page = repo::page_skus(
                &conn,
                &scope,
                tenant,
                DbBackend::Postgres,
                &SkuListFilter::default(),
                &query,
            )
            .await
            .unwrap();
            page.items.into_iter().map(|s| s.code).collect::<Vec<_>>()
        }
    };
    assert_eq!(page(None).await, ["LIVE"]);
    assert_eq!(page(Some("archived eq false")).await, ["LIVE"]);
    assert_eq!(page(Some("archived eq true")).await, ["GONE"]);
    let counts = repo::count_skus(
        &conn,
        &scope,
        tenant,
        DbBackend::Postgres,
        &SkuListFilter::default(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        (
            counts.all,
            counts.published,
            counts.retired,
            counts.archived
        ),
        (1, 1, 0, 1)
    );

    let mut categories = Vec::new();
    for code in ["kept", "shelved"] {
        categories.push(
            repo::insert_category(
                &conn,
                &scope,
                tenant,
                NewCategory {
                    code: code.into(),
                    name: code.into(),
                    is_default: false,
                    sort_order: 0,
                },
                now,
            )
            .await
            .unwrap(),
        );
    }
    let shelved = &categories[1];
    let HeadWrite::Written(_) = repo::set_category_archived(
        &conn,
        &scope,
        tenant,
        shelved.id,
        shelved.version,
        Some(actor),
        now,
    )
    .await
    .unwrap() else {
        panic!("the category archive matched")
    };
    let listed = |filter: Option<&'static str>| {
        let conn = db.conn().unwrap();
        let scope = scope.clone();
        async move {
            let mut query = ODataQuery::default();
            if let Some(raw) = filter {
                query =
                    query.with_filter(toolkit_odata::parse_filter_string(raw).unwrap().into_expr());
            }
            repo::page_categories(&conn, &scope, tenant, &query)
                .await
                .unwrap()
                .items
                .into_iter()
                .map(|c| c.code)
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(listed(None).await, ["kept"]);
    assert_eq!(listed(Some("archived eq true")).await, ["shelved"]);
}

/// The default SKU page (no `$filter`, code order, a page of 51, as `page_skus` asks it): the plan
/// of the tenant's statement on a table where most of the tenant's SKUs are archived.
const DEFAULT_PAGE: &str = "SELECT * FROM bss.products_sku WHERE tenant_id IN ('{T}') \
     AND tenant_id = '{T}' AND archived_at IS NULL ORDER BY code ASC, id ASC LIMIT 51";

/// P-D-263: the default page walks `ix_products_sku_unarchived` in code order and reads no archived
/// row. 40 tenants of 500 SKUs; the read tenant has 4500 more, archived and retired.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_default_sku_page_walks_the_unarchived_index_on_postgres() {
    let pg = Pg::applied().await;
    let read = Uuid::from_u128(0x7e7a_0001);
    run(
        &pg,
        &format!(
            "INSERT INTO bss.products_sku (id, tenant_id, code, name, type, description, sellable, \
             lifecycle, created_by, created_at, updated_at) \
             SELECT gen_random_uuid(), \
                    CASE WHEN t = 0 THEN '{read}'::uuid ELSE gen_random_uuid() END, \
                    'C-' || t || '-' || n, 'N-' || t || '-' || n, 'recurring', '', true, \
                    'published', '{read}', now(), now() \
             FROM generate_series(0, 39) AS t, generate_series(1, 500) AS n"
        ),
    )
    .await
    .unwrap();
    run(
        &pg,
        &format!(
            "INSERT INTO bss.products_sku (id, tenant_id, code, name, type, description, sellable, \
             lifecycle, created_by, created_at, updated_at, archived_at, archived_by) \
             SELECT gen_random_uuid(), '{read}', 'A-' || n, 'Archived ' || n, 'recurring', '', \
                    true, 'retired', '{read}', now(), now(), now(), '{read}' \
             FROM generate_series(1, 4500) AS n"
        ),
    )
    .await
    .unwrap();
    run(&pg, "ANALYZE bss.products_sku").await.unwrap();
    let raw = pg.raw().await;
    let plan: Vec<String> = raw
        .query_all_raw(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF, SUMMARY OFF) {}",
                DEFAULT_PAGE.replace("{T}", &read.to_string())
            ),
        ))
        .await
        .unwrap()
        .iter()
        .map(|row| row.try_get::<String>("", "QUERY PLAN").unwrap())
        .collect();
    raw.close().await.unwrap();
    let plan = plan.join("\n");
    eprintln!("the default SKU page on Postgres:\n{plan}");
    assert!(plan.contains("ix_products_sku_unarchived"), "{plan}");
    assert!(!plan.contains("Seq Scan"), "{plan}");
    assert!(!plan.contains("Rows Removed by Filter"), "{plan}");
}

/// P-D-263 on Postgres: an archived SKU that a pending unit still locks counts in `archived` only;
/// `in_review` counts the locked SKUs that are not archived. The `SQLite` twin is
/// `archive_tests::an_archived_locked_sku_is_not_in_review`.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_archived_locked_sku_is_not_in_review_on_postgres() {
    let pg = Pg::applied().await;
    let db = pg.db().await;
    let conn = db.conn().unwrap();
    let tenant = Uuid::new_v4();
    let actor = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let now = OffsetDateTime::now_utc();
    let mut ids = Vec::new();
    for (code, lifecycle) in [("GONE", Lifecycle::Retired), ("LIVE", Lifecycle::Published)] {
        let s = repo::insert_sku(
            &conn,
            &scope,
            tenant,
            NewSku {
                code: code.into(),
                name: code.into(),
                r#type: SkuType::Recurring,
                category_id: None,
                description: String::new(),
                sellable: true,
                gl_code: None,
                tax_category: None,
                invoice_line_template: None,
                billing_timing: None,
                usage_type_ref: None,
                unit: None,
            },
            actor,
            now,
        )
        .await
        .unwrap();
        repo::set_lifecycle(
            &conn,
            &scope,
            tenant,
            s.id,
            &[Lifecycle::Draft],
            lifecycle,
            now,
        )
        .await
        .unwrap();
        let revision = repo::find_sku(&conn, &scope, tenant, s.id)
            .await
            .unwrap()
            .unwrap()
            .revision;
        assert!(
            repo::try_lock_sku(&conn, &scope, tenant, s.id, Uuid::new_v4(), revision)
                .await
                .unwrap()
        );
        ids.push(s.id);
    }
    let gone = repo::find_sku(&conn, &scope, tenant, ids[0])
        .await
        .unwrap()
        .unwrap();
    assert!(gone.pending_unit_id.is_some());
    let HeadWrite::Written(_) = repo::set_sku_archived(
        &conn,
        &scope,
        tenant,
        gone.id,
        gone.revision,
        Some(actor),
        now,
    )
    .await
    .unwrap() else {
        panic!("the archive matched")
    };
    let counts = repo::count_skus(
        &conn,
        &scope,
        tenant,
        DbBackend::Postgres,
        &SkuListFilter::default(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        (
            counts.all,
            counts.published,
            counts.in_review,
            counts.archived
        ),
        (1, 1, 1, 1),
        "{counts:?}"
    );
}
