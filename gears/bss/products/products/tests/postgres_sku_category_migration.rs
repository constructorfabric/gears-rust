//! P-D-196 on Postgres: `m20260925_000007_sku_category_optional` drops `products_sku.category_id`'s
//! NOT NULL in place. The twin of `sku_category_migration.rs`, with the same shape: the gear's
//! whole list without 000007, SKUs, versions and references seeded, the structure captured, then
//! the whole list again through the real runner, which applies 000007 alone. The structure is
//! `information_schema.columns`, `pg_constraint`, `pg_trigger` (internal ones included) and
//! `pg_indexes` of the three tables in schema `bss`. Exactly one fact may differ —
//! `category_id`'s nullability — and every row must survive.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod pg_support;

use bss_products::gear::BssProductsGear;
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};

const MIGRATION: &str = "m20260925_000007_sku_category_optional";
const FAMILY: &str = "'products_sku','products_sku_version','products_sku_reference'";

/// The gear's whole list through the runner; `without` leaves one migration out.
async fn migrate(pg: &Pg, without: Option<&str>) -> Result<MigrationResult, MigrationError> {
    let db = pg.db().await;
    let chain = BssProductsGear::default()
        .migrations()
        .into_iter()
        .filter(|m| {
            Some(m.name()) != without && m.name() != "m20261001_000011_sku_lifecycle_honesty"
        })
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
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    raw.close().await.unwrap();
}

async fn refused(pg: &Pg, sql: &str) -> String {
    let raw = pg.raw().await;
    let error = raw
        .execute_raw(Statement::from_string(DbBackend::Postgres, sql.to_owned()))
        .await
        .expect_err(sql)
        .to_string();
    raw.close().await.unwrap();
    error
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

/// The structure of the family, one fact per line, sorted.
async fn structure(pg: &Pg) -> Vec<String> {
    let mut facts = Vec::new();
    for sql in [
        format!(
            "SELECT 'column ' || table_name || '.' || column_name || ' ' || data_type || \
             ' nullable=' || is_nullable || ' default=' || coalesce(column_default, '(none)') || \
             ' position=' || ordinal_position AS v FROM information_schema.columns \
             WHERE table_schema = 'bss' AND table_name IN ({FAMILY})"
        ),
        format!(
            "SELECT 'constraint ' || c.relname || ' ' || con.conname || ' ' || con.contype::text || ' ' || \
             pg_get_constraintdef(con.oid) AS v FROM pg_constraint con \
             JOIN pg_class c ON c.oid = con.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'bss' AND c.relname IN ({FAMILY})"
        ),
        format!(
            "SELECT 'trigger ' || c.relname || ' ' || t.tgname || ' internal=' || t.tgisinternal::text || \
             ' ' || pg_get_triggerdef(t.oid) AS v FROM pg_trigger t \
             JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'bss' AND c.relname IN ({FAMILY})"
        ),
        format!(
            "SELECT 'index ' || tablename || ' ' || indexname || ' ' || indexdef AS v \
             FROM pg_indexes WHERE schemaname = 'bss' AND tablename IN ({FAMILY})"
        ),
    ] {
        facts.extend(strings(pg, &sql).await);
    }
    facts.sort();
    facts
}

/// Every row of the family as JSON of all its columns, in key order.
async fn rows(pg: &Pg) -> Vec<String> {
    let mut rows = Vec::new();
    for (table, key) in [
        ("products_sku", "id"),
        ("products_sku_version", "sku_id, published_version"),
        ("products_sku_reference", "id"),
    ] {
        rows.extend(
            strings(
                pg,
                &format!(
                    "SELECT '{table} ' || row_to_json(t)::text AS v FROM bss.{table} t ORDER BY {key}"
                ),
            )
            .await,
        );
    }
    rows
}

const C1: &str = "00000000-0000-0000-0000-0000000000c1";
const T1: &str = "00000000-0000-0000-0000-0000000000a1";
const S1: &str = "00000000-0000-0000-0000-000000000051";
const S2: &str = "00000000-0000-0000-0000-000000000052";

fn seed() -> Vec<String> {
    vec![
        format!(
            "INSERT INTO bss.products_category (id,tenant_id,code,name,is_default,status,created_at,updated_at) VALUES ('{C1}','{T1}','hosting','Hosting',true,'active','2026-09-25T00:00:00Z','2026-09-25T00:00:00Z')"
        ),
        format!(
            "INSERT INTO bss.products_sku (id,tenant_id,code,name,type,category_id,description,sellable,lifecycle,fence_prior_lifecycle,fenced_at,fence_op_id,revision,published_version,gl_code,tax_category,invoice_line_template,billing_timing,usage_type_ref,unit,type_change_pending,pending_unit_id,approved_by_unit_id,created_by,created_at,updated_at) VALUES ('{S1}','{T1}','STOR','Storage','usage','{C1}','Block storage',false,'published','published','2026-09-25T01:00:00Z',gen_random_uuid(),4,2,'4010','T1','{{name}}','arrears','storage','GB',true,gen_random_uuid(),gen_random_uuid(),gen_random_uuid(),'2026-09-25T00:00:00Z','2026-09-25T02:00:00Z')"
        ),
        format!(
            "INSERT INTO bss.products_sku (id,tenant_id,code,name,type,category_id,lifecycle,created_by,created_at,updated_at) VALUES ('{S2}','{T1}','SEAT','Seat','recurring','{C1}','draft',gen_random_uuid(),'2026-09-25T00:00:00Z','2026-09-25T00:00:00Z')"
        ),
        format!(
            "INSERT INTO bss.products_sku_version (sku_id,tenant_id,published_version,effective_from,content,created_at) VALUES ('{S1}','{T1}',1,'2026-09-25','{{\"code\":\"STOR\"}}','2026-09-25T00:00:00Z')"
        ),
        format!(
            "INSERT INTO bss.products_sku_version (sku_id,tenant_id,published_version,effective_from,content,created_at) VALUES ('{S1}','{T1}',2,'2026-10-01','{{\"code\":\"STOR\",\"gl_code\":\"4010\"}}','2026-09-25T02:00:00Z')"
        ),
        format!(
            "INSERT INTO bss.products_sku_reference (id,tenant_id,sku_id,owner_gear,ref_kind,ref_id,state,reserved_by,reserved_at) VALUES (gen_random_uuid(),'{T1}','{S1}','pricing','price_book_entry','00000000-0000-0000-0000-0000000000e1','reserved',gen_random_uuid(),'2026-09-25T03:00:00Z')"
        ),
        format!(
            "INSERT INTO bss.products_sku_reference (id,tenant_id,sku_id,owner_gear,ref_kind,ref_id,state,reserved_by,reserved_at,confirmed_at) VALUES (gen_random_uuid(),'{T1}','{S1}','pricing','plan_item',gen_random_uuid(),'confirmed',gen_random_uuid(),'2026-09-25T03:00:00Z','2026-09-25T03:01:00Z')"
        ),
        format!(
            "INSERT INTO bss.products_sku_reference (id,tenant_id,sku_id,owner_gear,ref_kind,ref_id,state,reserved_by,reserved_at,released_at,released_by,release_reason,forced) VALUES (gen_random_uuid(),'{T1}','{S1}','pricing','price_book_entry','00000000-0000-0000-0000-0000000000e1','released',gen_random_uuid(),'2026-09-25T02:00:00Z','2026-09-25T02:30:00Z',gen_random_uuid(),'abandoned',true)"
        ),
    ]
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn dropping_the_not_null_keeps_every_row_and_changes_only_the_category_nullability() {
    let pg = Pg::empty().await;
    let before_run = migrate(&pg, Some(MIGRATION)).await.unwrap();
    assert!(!before_run.applied_names.iter().any(|n| n == MIGRATION));
    let seed = seed();
    exec(&pg, &seed.iter().map(String::as_str).collect::<Vec<_>>()).await;
    let structure_before = structure(&pg).await;
    let rows_before = rows(&pg).await;
    assert_eq!(rows_before.len(), 7, "{rows_before:#?}");

    let result = migrate(&pg, None).await.unwrap();

    assert_eq!(result.applied_names, [MIGRATION], "only 000007 was pending");
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
        "P-D-196 Postgres proof: {} structural facts before, {} after, {} rows\nremoved:\n  {}\nadded:\n  {}",
        structure_before.len(),
        structure_after.len(),
        rows_before.len(),
        show(&removed),
        show(&added)
    );
    assert_eq!(
        removed,
        ["column products_sku.category_id uuid nullable=NO default=(none) position=6"],
        "added: {added:#?}"
    );
    assert_eq!(
        added,
        ["column products_sku.category_id uuid nullable=YES default=(none) position=6"]
    );
    assert_eq!(rows(&pg).await, rows_before, "every row survives");

    exec(&pg, &[&format!("INSERT INTO bss.products_sku (id,tenant_id,code,name,type,category_id,lifecycle,created_by,created_at,updated_at) VALUES (gen_random_uuid(),'{T1}','LOOSE','Loose','recurring',NULL,'draft',gen_random_uuid(),now(),now())")]).await;
    for (sql, refusal) in [
        (
            format!(
                "INSERT INTO bss.products_sku (id,tenant_id,code,name,type,category_id,lifecycle,created_by,created_at,updated_at) VALUES (gen_random_uuid(),'{T1}','GONE','Gone','recurring',gen_random_uuid(),'draft',gen_random_uuid(),now(),now())"
            ),
            "products_sku_category_id_fkey",
        ),
        (
            "UPDATE bss.products_sku_version SET content = '[]'".to_owned(),
            "products_sku_version is append-only",
        ),
        (
            format!("DELETE FROM bss.products_sku WHERE id = '{S1}'"),
            "violates foreign key constraint",
        ),
    ] {
        let error = refused(&pg, &sql).await;
        assert!(error.contains(refusal), "{sql}\n{error}");
    }
}
