//! D-427 on Postgres: the same forward migration as `entry_model_migration.rs`, tables in schema
//! `bss`. A database is migrated by the gear's whole list without 000013 through the toolkit
//! runner, seeded with entries, prices of several states, a pending unit, a plan with items and
//! reference ops whose stored create input predates 000013, then migrated again: 000013 applies
//! alone. The proof of the structure is the Postgres schema dump (columns, named constraints,
//! indexes), compared before and after and with a fresh chain.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod entry_support;
mod pg_support;
mod schema_dump;

use bss_pricing::infra::reference_ticker::system_actor;
use bss_pricing::infra::reference_work::{self, Caller, WallClock};
use bss_pricing::infra::storage::entity::reference_op;
use bss_pricing::infra::storage::repo::{idempotency_repo as idem, reference_op_repo};
use bss_pricing::module::BssPricingGear;
use entry_support::{Script, app_for, request, state_on, user_of};
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::DBProvider;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use toolkit_db::secure::AccessScope;
use uuid::Uuid;

const MIGRATION: &str = "m20260926_000013_model_on_the_entry";

const TENANT: Uuid = Uuid::from_u128(0x7e0a_0000_0000_0000_0000_0000_0000_0002);
const AUTHOR: Uuid = Uuid::from_u128(0xa0);
const BOOK: Uuid = Uuid::from_u128(0xb0);
const STORAGE: Uuid = Uuid::from_u128(0xe1);
const SEATS: Uuid = Uuid::from_u128(0xe2);
const SETUP: Uuid = Uuid::from_u128(0xe3);
const CALLS: Uuid = Uuid::from_u128(0xe4);
const SUPPORT: Uuid = Uuid::from_u128(0xe5);
const LATE: Uuid = Uuid::from_u128(0xe6);
const UNIT: Uuid = Uuid::from_u128(0x401);
const PLAN: Uuid = Uuid::from_u128(0x501);
const REVISION: Uuid = Uuid::from_u128(0x601);
const OP_DONE: Uuid = Uuid::from_u128(0x801);
const OP_CREATE: Uuid = Uuid::from_u128(0x802);
const OP_REREREVE: Uuid = Uuid::from_u128(0x803);
const KEY: &str = "k-before";
const TWIN: Uuid = Uuid::from_u128(0xe7);
const OP_TWIN: Uuid = Uuid::from_u128(0x804);
const TWIN_KEY: &str = "k-twin";

fn sku(entry: Uuid) -> Uuid {
    Uuid::from_u128(entry.as_u128() | 0x5c00)
}
fn u(id: Uuid) -> String {
    format!("'{id}'::uuid")
}
fn old_outcome(book: Uuid, sku: Uuid) -> String {
    json!({
        "target": {"price_book_entry": {"book_id": book, "input": {
            "sku_id": sku, "period": null, "dimension_key": null, "invoice_line_override": null
        }}},
        "correlation": Uuid::from_u128(0xc0),
        "refusal": null,
        "receipt": null,
    })
    .to_string()
}

async fn migrate(pg: &Pg, without: Option<&str>) -> Result<MigrationResult, MigrationError> {
    let chain = BssPricingGear::default()
        .migrations()
        .into_iter()
        .filter(|m| {
            Some(m.name()) != without
                && m.name() != "m20260930_000018_usage_rating_policy"
                && !m.name().contains("000021")
                && !m.name().contains("000022")
                && !m.name().contains("000023")
        })
        .collect();
    run_migrations_for_testing(&pg.db().await, chain).await
}

async fn exec(pg: &Pg, statements: &[String]) {
    let raw = pg.raw().await;
    for sql in statements {
        raw.execute_raw(Statement::from_string(DbBackend::Postgres, sql.clone()))
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
}

async fn refused(pg: &Pg, sql: &str) -> String {
    pg.raw()
        .await
        .execute_raw(Statement::from_string(DbBackend::Postgres, sql.to_owned()))
        .await
        .expect_err(sql)
        .to_string()
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

/// Every row of every `bss.pricing_*` table as JSON without `model`, sorted.
async fn rows_without_model(pg: &Pg) -> Vec<String> {
    let tables = strings(
        pg,
        r"SELECT tablename::text AS v FROM pg_tables WHERE schemaname = 'bss'
          AND tablename LIKE 'pricing\_%' ORDER BY 1",
    )
    .await;
    let mut rows = Vec::new();
    for table in tables {
        rows.extend(
            strings(
                pg,
                &format!(
                    "SELECT '{table} ' || (to_jsonb(t) - 'model')::text AS v FROM bss.{table} t"
                ),
            )
            .await,
        );
    }
    rows.sort();
    rows
}

async fn models(pg: &Pg) -> Vec<(Uuid, String)> {
    let mut found: Vec<(Uuid, String)> = strings(
        pg,
        "SELECT id::text || ' ' || model AS v FROM bss.pricing_price_book_entry",
    )
    .await
    .iter()
    .map(|line| {
        let (id, model) = line.split_once(' ').unwrap();
        (Uuid::parse_str(id).unwrap(), model.to_owned())
    })
    .collect();
    found.sort();
    found
}

async fn history(pg: &Pg) -> Vec<String> {
    let ledgers = strings(
        pg,
        r"SELECT schemaname || '.' || quote_ident(tablename) AS v FROM pg_tables
          WHERE tablename LIKE 'toolkit\_migrations%'",
    )
    .await;
    assert_eq!(ledgers.len(), 1, "one history table: {ledgers:?}");
    let mut names = strings(pg, &format!("SELECT version AS v FROM {}", ledgers[0])).await;
    names.sort();
    names
}

async fn seed_through_repositories(pg: &Pg) {
    let provider = DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let conn = provider.conn().unwrap();
    let scope = AccessScope::for_tenant(TENANT);
    let now = time::OffsetDateTime::now_utc();
    let at = time::Date::from_calendar_date(2026, time::Month::September, 1)
        .unwrap()
        .with_hms(9, 0, 0)
        .unwrap()
        .assume_utc();
    // The book's table gained the archive mark later (000023, D-522), so the book is written in
    // the shape this chain holds: as the repository writes it, without those columns.
    exec(
        pg,
        &[format!(
            "INSERT INTO bss.pricing_price_book (id, tenant_id, code, name, currency, valid_from, \
             valid_until, description, version, created_at, updated_at) VALUES ('{BOOK}', \
             '{TENANT}', 'standard', 'Standard', 'EUR', NULL, NULL, NULL, 1, \
             '2026-09-01T09:00:00Z', '2026-09-01T09:00:00Z')"
        )],
    )
    .await;
    let op = |op_id, kind: &str, entry, reservation, key: Option<&str>, state: &str| {
        reference_op::Model {
            op_id,
            tenant_id: TENANT,
            kind: kind.into(),
            ref_kind: "price_book_entry".into(),
            ref_id: entry,
            sku_id: sku(entry),
            reservation_id: reservation,
            idempotency_key: key.map(str::to_owned),
            state: state.into(),
            outcome: Some(old_outcome(BOOK, sku(entry))),
            attempts: 0,
            next_attempt_at: at,
            last_error: None,
            created_by: AUTHOR,
            created_at: at,
            updated_at: at,
        }
    };
    for m in [
        op(
            OP_DONE,
            "create",
            STORAGE,
            Some(Uuid::from_u128(0x9e1)),
            None,
            "done",
        ),
        op(
            OP_CREATE,
            "create",
            LATE,
            Some(Uuid::from_u128(0x9e6)),
            Some(KEY),
            "reserving",
        ),
        op(OP_REREREVE, "rereserve", STORAGE, None, None, "reserving"),
    ] {
        reference_op_repo::insert(&conn, &scope, m).await.unwrap();
    }
    let endpoint = format!("/bss-pricing/v1/price-books/{BOOK}/entries");
    idem::claim_idempotency_key(
        &conn,
        &scope,
        TENANT,
        &endpoint,
        KEY,
        &[0],
        now,
        now + time::Duration::hours(24),
    )
    .await
    .unwrap();
    idem::bind_op(&conn, &scope, TENANT, &endpoint, KEY, OP_CREATE)
        .await
        .unwrap();
}

/// A create stored before 000013 (no `model`) for STORAGE's key, still reserving with its receipt
/// and its bound Idempotency-Key.
async fn seed_a_create_for_a_taken_key(pg: &Pg) {
    let provider = DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let conn = provider.conn().unwrap();
    let scope = AccessScope::for_tenant(TENANT);
    let now = time::OffsetDateTime::now_utc();
    reference_op_repo::insert(
        &conn,
        &scope,
        reference_op::Model {
            op_id: OP_TWIN,
            tenant_id: TENANT,
            kind: "create".into(),
            ref_kind: "price_book_entry".into(),
            ref_id: TWIN,
            sku_id: sku(STORAGE),
            reservation_id: Some(Uuid::from_u128(0x9e7)),
            idempotency_key: Some(TWIN_KEY.into()),
            state: "reserving".into(),
            outcome: Some(old_outcome(BOOK, sku(STORAGE))),
            attempts: 0,
            next_attempt_at: now,
            last_error: None,
            created_by: AUTHOR,
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();
    let endpoint = format!("/bss-pricing/v1/price-books/{BOOK}/entries");
    idem::claim_idempotency_key(
        &conn,
        &scope,
        TENANT,
        &endpoint,
        TWIN_KEY,
        &[1],
        now,
        now + time::Duration::hours(24),
    )
    .await
    .unwrap();
    idem::bind_op(&conn, &scope, TENANT, &endpoint, TWIN_KEY, OP_TWIN)
        .await
        .unwrap();
}

fn entry_row(id: Uuid, kind: &str, period: Option<&str>) -> String {
    let period = period.map_or_else(|| "NULL".to_owned(), |p| format!("'{p}'"));
    format!(
        "INSERT INTO bss.pricing_price_book_entry (id,tenant_id,book_id,sku_id,charge_kind,period,dimension_key,invoice_line_override,reservation_id,reference_state,version,created_at,updated_at) \
         VALUES ({},{},{},{},'{kind}',{period},NULL,NULL,{},'confirmed',1,now(),now())",
        u(id),
        u(TENANT),
        u(BOOK),
        u(sku(id)),
        u(Uuid::from_u128(id.as_u128() | 0x9000)),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "one stored row, column for column"
)]
fn price_row(
    n: u128,
    entry: Uuid,
    version_no: i32,
    model: &str,
    money: &Value,
    from: &str,
    state: &str,
    unit: Option<Uuid>,
) -> String {
    let unit = unit.map_or_else(|| "NULL".to_owned(), u);
    let approved_at = if state == "approved" { "now()" } else { "NULL" };
    format!(
        "INSERT INTO bss.pricing_price (id,tenant_id,price_book_entry_id,version_no,dim_value,model,price_json,min_fee,eligibility,effective_from,effective_to,keep_for_bound,closed_explicitly,temporary_until,paired_price_id,return_of_price_id,state,pending_unit_id,approved_by_unit_id,note,created_by,approved_at,version,created_at,updated_at) \
         VALUES ({},{},{},{version_no},NULL,'{model}','{money}'::jsonb,NULL,'all','{from}',NULL,false,false,NULL,NULL,NULL,'{state}',{unit},NULL,'seed',{},{approved_at},1,now(),now())",
        u(Uuid::from_u128(n)),
        u(TENANT),
        u(entry),
        u(AUTHOR),
    )
}

fn graduated() -> Value {
    json!({"tiers":[{"up_to":"1000","rate":"0.010"},{"up_to":null,"rate":"0.008"}]})
}

fn item_row(n: u128, sku: Uuid, entry: Option<Uuid>, treatment: &str, qty: Option<&str>) -> String {
    let entry = entry.map_or_else(|| "NULL".to_owned(), u);
    let qty = qty.map_or_else(|| "NULL".to_owned(), |q| format!("'{q}'"));
    format!(
        "INSERT INTO bss.pricing_plan_item (id,tenant_id,revision_id,sku_id,price_book_entry_id,treatment,included_qty,qty_min,reservation_id,reference_state,version,created_by,created_at,updated_at) \
         VALUES ({},{},{},{},{entry},'{treatment}',{qty},NULL,{},'confirmed',1,{},now(),now())",
        u(Uuid::from_u128(n)),
        u(TENANT),
        u(REVISION),
        u(sku),
        u(Uuid::from_u128(n | 0x9000)),
        u(AUTHOR)
    )
}

fn seed_rows() -> Vec<String> {
    vec![
        entry_row(STORAGE, "usage", None),
        entry_row(SEATS, "recurring", Some("month")),
        entry_row(SETUP, "one_time", None),
        entry_row(CALLS, "usage", None),
        entry_row(SUPPORT, "recurring", Some("year")),
        format!(
            "INSERT INTO bss.pricing_approval_unit (id,tenant_id,kind,ref_type,ref_id,state,common_effective_date,quorum_required,generation,submitted_by,submitted_at,decided_at,decided_note,snapshot,snapshot_hash,version) \
             VALUES ({},{},'prices','price_book',{},'pending',NULL,1,1,{},now(),NULL,NULL,'{{}}'::jsonb,'seed',1)",
            u(UNIT),
            u(TENANT),
            u(BOOK),
            u(AUTHOR)
        ),
        price_row(
            0x11,
            STORAGE,
            1,
            "graduated",
            &graduated(),
            "2026-01-01",
            "approved",
            None,
        ),
        price_row(
            0x12,
            STORAGE,
            2,
            "graduated",
            &graduated(),
            "2031-01-01",
            "draft",
            None,
        ),
        price_row(
            0x13,
            STORAGE,
            3,
            "graduated",
            &graduated(),
            "2031-02-01",
            "rejected",
            None,
        ),
        price_row(
            0x21,
            SEATS,
            1,
            "flat",
            &json!({"amount":"10.00"}),
            "2026-01-01",
            "approved",
            None,
        ),
        price_row(
            0x22,
            SEATS,
            2,
            "flat",
            &json!({"amount":"12.00"}),
            "2031-01-01",
            "pending",
            Some(UNIT),
        ),
        price_row(
            0x51,
            SUPPORT,
            1,
            "per_unit",
            &json!({"rate":"2.00"}),
            "2026-01-01",
            "approved",
            None,
        ),
        format!(
            "INSERT INTO bss.pricing_plan (id,tenant_id,code,name,published_rev,version,created_by,created_at,updated_at) \
             VALUES ({},{},'pro','Pro',NULL,1,{},now(),now())",
            u(PLAN),
            u(TENANT),
            u(AUTHOR)
        ),
        format!(
            "INSERT INTO bss.pricing_plan_revision (id,tenant_id,plan_id,rev_no,book_id,state,available_from,pending_unit_id,approved_by_unit_id,published_at,version,created_by,created_at,updated_at) \
             VALUES ({},{},{},1,{},'draft',NULL,NULL,NULL,NULL,1,{},now(),now())",
            u(REVISION),
            u(TENANT),
            u(PLAN),
            u(BOOK),
            u(AUTHOR)
        ),
        item_row(0x701, sku(STORAGE), Some(STORAGE), "paid", None),
        item_row(0x702, sku(SEATS), Some(SEATS), "paid", None),
        item_row(0x703, Uuid::from_u128(0x5c99), None, "included", Some("5")),
    ]
}

async fn seeded() -> Pg {
    let pg = Pg::empty().await;
    let before = migrate(&pg, Some(MIGRATION)).await.unwrap();
    assert!(
        !before.applied_names.iter().any(|n| n == MIGRATION),
        "the chain before this run"
    );
    seed_through_repositories(&pg).await;
    exec(&pg, &seed_rows()).await;
    pg
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_the_forward_migration_moves_the_model_to_the_entry_and_keeps_every_row() {
    let pg = seeded().await;
    let dump_before = dump(&pg).await;
    let rows_before = rows_without_model(&pg).await;

    let result = migrate(&pg, None).await.unwrap();

    assert_eq!(result.applied_names, [MIGRATION], "only 000013 was pending");
    assert_eq!(
        models(&pg).await,
        vec![
            (STORAGE, "graduated".to_owned()),
            (SEATS, "flat".to_owned()),
            (SETUP, "flat".to_owned()),
            (CALLS, "per_unit".to_owned()),
            (SUPPORT, "per_unit".to_owned()),
        ]
    );
    assert_eq!(
        rows_without_model(&pg).await,
        rows_before,
        "every row survives"
    );
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
        "D-427 Postgres dump diff\nremoved:\n  {}\nadded:\n  {}",
        show(&removed),
        show(&added)
    );
    assert_eq!(removed.len(), 3, "removed: {removed:#?}");
    assert!(removed[0].starts_with("COLUMN bss.pricing_price model text NOT NULL"));
    assert!(removed[1].starts_with(
        "CONSTRAINT bss.pricing_price pricing_price_model_check CHECK ((model = ANY (ARRAY['flat'::text, 'per_unit'::text, 'graduated'::text, 'volume'::text, 'package'::text])))"
    ));
    assert!(removed[2].starts_with("INDEX bss pricing_price_book_entry_key "));
    assert!(
        removed[2].ends_with("COALESCE(period, ''::text))"),
        "{}",
        removed[2]
    );
    assert_eq!(added.len(), 3, "added: {added:#?}");
    assert_eq!(
        added[0], "COLUMN bss.pricing_price_book_entry model text NOT NULL DEFAULT -",
        "the Postgres column keeps no default"
    );
    assert_eq!(
        added[1],
        "CONSTRAINT bss.pricing_price_book_entry pricing_price_book_entry_model_check CHECK ((model = ANY (ARRAY['flat'::text, 'per_unit'::text, 'graduated'::text, 'volume'::text, 'package'::text])))"
    );
    assert!(added[2].starts_with("INDEX bss pricing_price_book_entry_key "));
    assert!(
        added[2].ends_with("COALESCE(period, ''::text), model)"),
        "{}",
        added[2]
    );
    let fresh = Pg::empty().await;
    migrate(&fresh, None).await.unwrap();
    assert_eq!(dump_after, dump(&fresh).await, "upgraded and fresh agree");
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_an_entry_op_stored_before_the_migration_resumes_after_it() {
    let pg = seeded().await;
    migrate(&pg, None).await.unwrap();
    // Current application entities require the complete schema after the historical migration assertions.
    run_migrations_for_testing(&pg.db().await, BssPricingGear::default().migrations())
        .await
        .unwrap();
    let state = state_on(DBProvider::new(pg.db().await), Arc::new(Script::default())).await;
    let system = system_actor(TENANT).unwrap();

    let receipt = reference_work::drive(
        &state,
        &system,
        OP_CREATE,
        Arc::new(WallClock),
        Caller::Ticker,
    )
    .await
    .unwrap()
    .expect("the create finished with its receipt");
    let body: Value = serde_json::from_str(&receipt.body).unwrap();
    assert_eq!(receipt.status, 201, "{body}");
    assert_eq!(body["model"], "per_unit");
    assert!(models(&pg).await.contains(&(LATE, "per_unit".to_owned())));
    assert_eq!(
        strings(
            &pg,
            &format!(
                "SELECT state || ' ' || response_status AS v FROM bss.pricing_idempotency WHERE client_key = '{KEY}'"
            )
        )
        .await,
        ["answered 201"]
    );
    reference_work::drive(
        &state,
        &system,
        OP_REREREVE,
        Arc::new(WallClock),
        Caller::Ticker,
    )
    .await
    .unwrap();
    assert!(
        models(&pg)
            .await
            .contains(&(STORAGE, "graduated".to_owned()))
    );
    assert_eq!(
        strings(
            &pg,
            &format!(
                "SELECT state AS v FROM bss.pricing_reference_op WHERE op_id = {}",
                u(OP_REREREVE)
            )
        )
        .await,
        ["done"]
    );
    // The export reads every backfilled model back through the door.
    let app = app_for(state, TENANT);
    let (status, export, _) = request(
        &app,
        &user_of(TENANT),
        "GET",
        &format!("/price-books/{BOOK}/export"),
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "{export}");
    for e in export["entries"].as_array().unwrap() {
        for p in e["prices"].as_array().unwrap() {
            assert_eq!(p["model"], e["entry"]["model"], "{p}");
        }
    }
}

/// A create stored before 000013 for a key STORAGE holds (backfilled `graduated`, not the usage
/// default) takes STORAGE's model and ends `ENTRY_KEY_TAKEN`; no second entry is written.
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_an_in_flight_create_for_a_taken_key_meets_it_after_the_model_moved() {
    let pg = seeded().await;
    seed_a_create_for_a_taken_key(&pg).await;
    migrate(&pg, None).await.unwrap();
    assert!(
        models(&pg)
            .await
            .contains(&(STORAGE, "graduated".to_owned()))
    );
    let script = Arc::new(Script::default());
    // Current application entities require the complete schema after the historical migration assertions.
    run_migrations_for_testing(&pg.db().await, BssPricingGear::default().migrations())
        .await
        .unwrap();
    let state = state_on(DBProvider::new(pg.db().await), script.clone()).await;
    let system = system_actor(TENANT).unwrap();

    let receipt = reference_work::drive(
        &state,
        &system,
        OP_TWIN,
        Arc::new(WallClock),
        Caller::Ticker,
    )
    .await
    .unwrap()
    .expect("the create finished with its answer");

    assert_eq!(receipt.status, 409, "{}", receipt.body);
    assert!(receipt.body.contains("ENTRY_KEY_TAKEN"), "{}", receipt.body);
    assert_eq!(
        strings(
            &pg,
            &format!(
                "SELECT id::text AS v FROM bss.pricing_price_book_entry WHERE sku_id = {}",
                u(sku(STORAGE))
            )
        )
        .await,
        [STORAGE.to_string()],
        "no second entry for the key"
    );
    assert_eq!(
        strings(
            &pg,
            &format!(
                "SELECT state AS v FROM bss.pricing_reference_op WHERE op_id = {}",
                u(OP_TWIN)
            )
        )
        .await,
        ["done"]
    );
    assert_eq!(
        strings(
            &pg,
            &format!(
                "SELECT state || ' ' || response_status AS v FROM bss.pricing_idempotency WHERE client_key = '{TWIN_KEY}'"
            )
        )
        .await,
        ["answered 409"]
    );
    assert!(Script::count(&script.releases) >= 1);
}

/// The statement of the one session of this database that waits for a lock.
async fn waiting_statement(pg: &Pg) -> String {
    for _ in 0..500 {
        let waiting = strings(
            pg,
            "SELECT query AS v FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .await;
        if let [statement] = waiting.as_slice() {
            return statement.clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the migration never waited for the open writer");
}

/// On Postgres the two-model check and the backfill are separate statements of a READ COMMITTED
/// transaction: a price in a second model committed between them would be backfilled away by
/// `min()` instead of refused. So 000013 first locks both tables ACCESS EXCLUSIVE. A writer of the
/// old chain still open when it starts (a price of STORAGE, graduated, in `per_unit`) holds the
/// migration at that lock, its first statement; once committed, that price is judged by the check,
/// and the migration fails naming STORAGE and records nothing.
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_the_migration_locks_both_tables_before_it_judges_the_models() {
    let pg = seeded().await;
    let history_before = history(&pg).await;
    let writer = pg.raw().await;
    let open = writer.begin().await.unwrap();
    open.execute_raw(Statement::from_string(
        DbBackend::Postgres,
        price_row(
            0x19,
            STORAGE,
            9,
            "per_unit",
            &json!({"rate":"1.00"}),
            "2032-01-01",
            "rejected",
            None,
        ),
    ))
    .await
    .unwrap();

    let commit_once_waited = async {
        let waiting = waiting_statement(&pg).await;
        open.commit().await.unwrap();
        waiting
    };
    let (result, waiting) = tokio::join!(migrate(&pg, None), commit_once_waited);

    let error = result
        .expect_err("the committed second model is refused, not backfilled away")
        .to_string();
    assert!(
        error.contains(MIGRATION) && error.contains(&STORAGE.to_string()),
        "names STORAGE: {error}"
    );
    assert_eq!(
        waiting,
        "LOCK TABLE bss.pricing_price_book_entry, bss.pricing_price IN ACCESS EXCLUSIVE MODE",
        "the migration waits at its lock, before any other statement"
    );
    assert_eq!(history(&pg).await, history_before, "000013 is not recorded");
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_two_models_on_one_entry_fail_the_migration_and_change_nothing() {
    let pg = seeded().await;
    exec(
        &pg,
        &[
            price_row(
                0x31,
                CALLS,
                1,
                "per_unit",
                &json!({"rate":"1.00"}),
                "2026-01-01",
                "approved",
                None,
            ),
            price_row(
                0x32,
                CALLS,
                2,
                "graduated",
                &graduated(),
                "2031-01-01",
                "rejected",
                None,
            ),
            price_row(
                0x33,
                SUPPORT,
                2,
                "flat",
                &json!({"amount":"3.00"}),
                "2031-01-01",
                "draft",
                None,
            ),
        ],
    )
    .await;
    let dump_before = dump(&pg).await;
    let history_before = history(&pg).await;

    let error = migrate(&pg, None).await.unwrap_err().to_string();

    assert!(error.contains(MIGRATION), "{error}");
    assert!(
        error.contains(&CALLS.to_string()) && error.contains(&SUPPORT.to_string()),
        "{error}"
    );
    assert!(!error.contains(&STORAGE.to_string()), "{error}");
    assert_eq!(dump(&pg).await, dump_before);
    assert_eq!(history(&pg).await, history_before);
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_after_the_migration_the_entry_key_takes_the_model() {
    let pg = seeded().await;
    migrate(&pg, None).await.unwrap();
    let insert = |id: u128, model: &str| {
        format!(
            "INSERT INTO bss.pricing_price_book_entry (id,tenant_id,book_id,sku_id,charge_kind,period,dimension_key,invoice_line_override,reservation_id,reference_state,version,created_at,updated_at,model) \
             VALUES ({},{},{},{},'usage',NULL,NULL,NULL,{},'confirmed',1,now(),now(),{model})",
            u(Uuid::from_u128(id)),
            u(TENANT),
            u(BOOK),
            u(sku(CALLS)),
            u(Uuid::from_u128(id | 0x9000)),
        )
    };
    exec(&pg, &[insert(0xf1, "'graduated'")]).await;
    for (sql, refusal) in [
        (insert(0xf2, "'per_unit'"), "pricing_price_book_entry_key"),
        (
            insert(0xf3, "'stair'"),
            "pricing_price_book_entry_model_check",
        ),
        (insert(0xf4, "NULL"), "null value in column \"model\""),
        (
            price_row(
                0x99,
                CALLS,
                9,
                "per_unit",
                &json!({"rate":"1.00"}),
                "2031-01-01",
                "draft",
                None,
            ),
            "column \"model\" of relation \"pricing_price\" does not exist",
        ),
    ] {
        let error = refused(&pg, &sql).await;
        assert!(error.contains(refusal), "{sql}\n{error}");
    }
}
