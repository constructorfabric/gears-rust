//! P-D-213 on Postgres: `m20260927_000008_audit_lifecycle_move` adds `from_lifecycle` and
//! `to_lifecycle` to `bss.products_audit_log` and redefines the append-only function. The twin of
//! `audit_lifecycle_migration.rs`, with the same shape: the gear's whole list without 000008, audit
//! rows seeded (one sealed), the structure captured, then the whole list again through the real
//! runner, which applies 000008 alone. The structure is `information_schema.columns`,
//! `pg_constraint`, `pg_trigger` (internal ones included) and `pg_indexes` of the table, and
//! `pg_get_functiondef` of `bss.products_audit_log_append_only()`. Exactly these facts may differ:
//! the two new columns, their two named CHECKs, and the function (the two columns added to the
//! seal's unchanged-list). Every row survives and reads null; the guard still refuses every UPDATE
//! that is not the seal and every DELETE, and the seal still passes.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod pg_support;

use bss_products::gear::BssProductsGear;
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};

const MIGRATION: &str = "m20260927_000008_audit_lifecycle_move";
/// What the guard answers every refused UPDATE and DELETE.
const REFUSED: &str = "products_audit_log is append-only";

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

async fn exec(pg: &Pg, sql: &str) -> Result<(), String> {
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

/// The structure of the table and its guard function, one fact per line, sorted.
async fn structure(pg: &Pg) -> Vec<String> {
    let mut facts = Vec::new();
    for sql in [
        "SELECT 'column ' || column_name || ' ' || data_type || ' nullable=' || is_nullable || \
         ' default=' || coalesce(column_default, '(none)') || ' position=' || ordinal_position AS v \
         FROM information_schema.columns WHERE table_schema = 'bss' AND table_name = 'products_audit_log'",
        "SELECT 'constraint ' || con.conname || ' ' || con.contype::text || ' ' || \
         pg_get_constraintdef(con.oid) AS v FROM pg_constraint con \
         JOIN pg_class c ON c.oid = con.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = 'bss' AND c.relname = 'products_audit_log'",
        "SELECT 'trigger ' || t.tgname || ' internal=' || t.tgisinternal::text || ' ' || \
         pg_get_triggerdef(t.oid) AS v FROM pg_trigger t \
         JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = 'bss' AND c.relname = 'products_audit_log'",
        "SELECT 'index ' || indexname || ' ' || indexdef AS v FROM pg_indexes \
         WHERE schemaname = 'bss' AND tablename = 'products_audit_log'",
        "SELECT 'function ' || pg_get_functiondef('bss.products_audit_log_append_only()'::regprocedure) AS v",
    ] {
        facts.extend(strings(pg, sql).await);
    }
    facts.sort();
    facts
}

const T1: &str = "00000000-0000-0000-0000-0000000000a1";
const S1: &str = "00000000-0000-0000-0000-000000000051";
const A: [&str; 3] = [
    "00000000-0000-0000-0000-00000000aa01",
    "00000000-0000-0000-0000-00000000aa02",
    "00000000-0000-0000-0000-00000000aa03",
];

fn seed() -> Vec<String> {
    vec![
        format!(
            "INSERT INTO bss.products_audit_log (audit_id,tenant_id,actor_ref,action,subject_kind,subject_id,subject_revision,written_at,seal_state) VALUES ('{}','{T1}',gen_random_uuid(),'sku.create','sku','{S1}',1,'2026-09-25T00:00:00Z','unsealed')",
            A[0]
        ),
        format!(
            "INSERT INTO bss.products_audit_log (audit_id,tenant_id,actor_ref,action,subject_kind,subject_id,reason,written_at,seal_state) VALUES ('{}','{T1}',gen_random_uuid(),'approval.rejected','approval_unit',gen_random_uuid(),'no','2026-09-25T01:00:00Z','unsealed')",
            A[1]
        ),
        format!(
            "INSERT INTO bss.products_audit_log (audit_id,tenant_id,actor_ref,action,subject_kind,subject_id,written_at,seal_state) VALUES ('{}','{T1}',gen_random_uuid(),'sku.unfence','sku','{S1}','2026-09-25T02:00:00Z','unsealed')",
            A[2]
        ),
        format!(
            "UPDATE bss.products_audit_log SET seal_state = 'sealed', chain_id = gen_random_uuid(), seq = 0, row_hash = '\\x01'::bytea WHERE audit_id = '{}'",
            A[2]
        ),
    ]
}

/// The rows' record columns as `000004` has them, in key order.
const RECORD: &str = "audit_id, tenant_id, actor_ref, action, subject_kind, subject_id, \
     subject_revision, error_code, attempted_key, reason, correlation_id, written_at, session_id, \
     ceremony_ref, seal_state, chain_id, seq, prev_hash, row_hash";

async fn rows(pg: &Pg) -> Vec<String> {
    strings(
        pg,
        &format!(
            "SELECT row_to_json(t)::text AS v FROM (SELECT {RECORD} FROM bss.products_audit_log \
             ORDER BY audit_id) t"
        ),
    )
    .await
}

const SEAL_TAIL: &str =
    "AND NEW.ceremony_ref IS NOT DISTINCT FROM OLD.ceremony_ref\n          THEN";
const SEAL_TAIL_AFTER: &str = "AND NEW.ceremony_ref IS NOT DISTINCT FROM OLD.ceremony_ref\n             AND NEW.from_lifecycle IS NOT DISTINCT FROM OLD.from_lifecycle\n             AND NEW.to_lifecycle IS NOT DISTINCT FROM OLD.to_lifecycle\n          THEN";

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_two_columns_arrive_empty_and_the_append_only_function_keeps_them() {
    let pg = Pg::empty().await;
    let before_run = migrate(&pg, Some(MIGRATION)).await.unwrap();
    assert!(!before_run.applied_names.iter().any(|n| n == MIGRATION));
    for sql in seed() {
        exec(&pg, &sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    let structure_before = structure(&pg).await;
    let rows_before = rows(&pg).await;
    assert_eq!(rows_before.len(), 3, "{rows_before:#?}");

    let result = migrate(&pg, None).await.unwrap();

    assert_eq!(result.applied_names, [MIGRATION], "only 000008 was pending");
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
        "P-D-213 Postgres proof: {} structural facts before, {} after, {} rows\nremoved:\n  {}\nadded:\n  {}",
        structure_before.len(),
        structure_after.len(),
        rows_before.len(),
        show(&removed),
        show(&added)
    );
    assert_eq!(removed.len(), 1, "removed: {removed:#?}\nadded: {added:#?}");
    assert_eq!(added.len(), 5, "removed: {removed:#?}\nadded: {added:#?}");
    let function_before = removed[0];
    assert!(
        function_before.starts_with("function "),
        "{function_before}"
    );
    assert_eq!(
        function_before.matches(SEAL_TAIL).count(),
        1,
        "{function_before}"
    );
    let mut expected_added = vec![
        "column from_lifecycle text nullable=YES default=(none) position=20".to_owned(),
        "column to_lifecycle text nullable=YES default=(none) position=21".to_owned(),
        "constraint chk_products_audit_log_from_lifecycle c CHECK ((from_lifecycle = ANY (ARRAY['draft'::text, 'published'::text, 'deprecated'::text, 'retiring'::text, 'retired'::text])))".to_owned(),
        "constraint chk_products_audit_log_to_lifecycle c CHECK ((to_lifecycle = ANY (ARRAY['draft'::text, 'published'::text, 'deprecated'::text, 'retiring'::text, 'retired'::text])))".to_owned(),
        function_before.replace(SEAL_TAIL, SEAL_TAIL_AFTER),
    ];
    expected_added.sort();
    assert_eq!(
        added.iter().map(|f| (*f).clone()).collect::<Vec<_>>(),
        expected_added
    );

    assert_eq!(rows(&pg).await, rows_before, "every row survives");
    assert_eq!(
        strings(
            &pg,
            "SELECT coalesce(from_lifecycle, 'null') || ' ' || coalesce(to_lifecycle, 'null') AS v \
             FROM bss.products_audit_log ORDER BY audit_id"
        )
        .await,
        ["null null", "null null", "null null"]
    );

    for sql in [
        format!(
            "UPDATE bss.products_audit_log SET from_lifecycle = 'draft' WHERE audit_id = '{}'",
            A[0]
        ),
        format!(
            "UPDATE bss.products_audit_log SET to_lifecycle = 'draft' WHERE audit_id = '{}'",
            A[0]
        ),
        format!(
            "UPDATE bss.products_audit_log SET from_lifecycle = 'draft' WHERE audit_id = '{}'",
            A[2]
        ),
        format!(
            "UPDATE bss.products_audit_log SET seal_state = 'sealed', chain_id = gen_random_uuid(), seq = 1, row_hash = '\\x02'::bytea, from_lifecycle = 'draft' WHERE audit_id = '{}'",
            A[0]
        ),
        format!(
            "UPDATE bss.products_audit_log SET seal_state = 'sealed', chain_id = gen_random_uuid(), seq = 1, row_hash = '\\x02'::bytea, to_lifecycle = 'retired' WHERE audit_id = '{}'",
            A[0]
        ),
        format!(
            "DELETE FROM bss.products_audit_log WHERE audit_id = '{}'",
            A[1]
        ),
    ] {
        let error = exec(&pg, &sql).await.expect_err(&sql);
        assert!(error.contains(REFUSED), "{sql}\n{error}");
    }
    exec(
        &pg,
        &format!(
            "UPDATE bss.products_audit_log SET seal_state = 'sealed', chain_id = gen_random_uuid(), \
             seq = 1, row_hash = '\\x02'::bytea, prev_hash = '\\x01'::bytea WHERE audit_id = '{}'",
            A[0]
        ),
    )
    .await
    .unwrap();
    let error = exec(
        &pg,
        &format!(
            "INSERT INTO bss.products_audit_log (audit_id,tenant_id,actor_ref,action,subject_kind,subject_id,written_at,seal_state,from_lifecycle) VALUES (gen_random_uuid(),'{T1}',gen_random_uuid(),'sku.create','sku','{S1}',now(),'unsealed','gone')"
        ),
    )
    .await
    .expect_err("a value outside the five lifecycles");
    assert!(
        error.contains("chk_products_audit_log_from_lifecycle"),
        "{error}"
    );
    assert_eq!(
        strings(
            &pg,
            "SELECT seal_state AS v FROM bss.products_audit_log ORDER BY audit_id"
        )
        .await,
        ["sealed", "unsealed", "sealed"]
    );
}
