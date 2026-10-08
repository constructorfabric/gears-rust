//! D-520 and D-521 on Postgres: 000022 adds the cancel and end columns, widens `state`, narrows
//! the approved-start index to prices, and reverses; and a cancel and an end apply through the
//! prices unit on the native engine.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod entry_support;
mod pg_support;

use bss_pricing::infra::clock::Clock;
use bss_pricing::infra::commercial_terms::wire;
use bss_pricing::infra::storage::repo::{acceptance_repo, price_book_entry_repo, price_repo};
use bss_pricing::module::BssPricingGear;
use entry_support::{Script, app_for, policy_support, request, state_with_clock, user_of};
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sea_orm_migration::SchemaManager;
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::{DBProvider, DbError, secure::AccessScope};
use uuid::Uuid;

const MIGRATION: &str = "m20261003_000022_price_cancel_and_end";
const TENANT: Uuid = Uuid::from_u128(0x22);
const BOOK: Uuid = Uuid::from_u128(0x2201);
const ENTRY: Uuid = Uuid::from_u128(0x2202);
const PRICE_A: Uuid = Uuid::from_u128(0x2210);
const PRICE_B: Uuid = Uuid::from_u128(0x2211);
const PAIR_A: Uuid = Uuid::from_u128(0x2220);
const PAIR_B: Uuid = Uuid::from_u128(0x2221);
const AUTHOR: Uuid = Uuid::from_u128(0x2230);
const UNIT: Uuid = Uuid::from_u128(0x2250);
const CHANGE: Uuid = Uuid::from_u128(0x2260);

fn q(id: Uuid) -> String {
    format!("'{id}'")
}

async fn exec(pg: &Pg, sql: &str) {
    pg.raw()
        .await
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            sql.to_owned(),
        ))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn try_exec(pg: &Pg, sql: &str) -> Result<(), sea_orm::DbErr> {
    pg.raw()
        .await
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            sql.to_owned(),
        ))
        .await
        .map(|_| ())
}

async fn strings(pg: &Pg, sql: &str) -> Vec<String> {
    pg.raw()
        .await
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            sql.to_owned(),
        ))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .iter()
        .map(|row| row.try_get::<String>("", "v").unwrap())
        .collect()
}

async fn prior(pg: &Pg) {
    let chain = BssPricingGear::default()
        .migrations()
        .into_iter()
        .filter(|m| m.name() != MIGRATION)
        .collect();
    toolkit_db::migration_runner::run_migrations_for_testing(&pg.db().await, chain)
        .await
        .unwrap();
}

async fn step(pg: &Pg, down: bool) {
    let migration = BssPricingGear::default()
        .migrations()
        .into_iter()
        .find(|m| m.name() == MIGRATION)
        .expect("000022 is in the chain");
    let conn = pg.raw().await;
    let manager = SchemaManager::new(&conn);
    if down {
        migration.down(&manager).await.unwrap();
    } else {
        migration.up(&manager).await.unwrap();
    }
}

async fn index_defs(pg: &Pg) -> Vec<String> {
    strings(
        pg,
        "SELECT indexdef::text AS v FROM pg_indexes WHERE schemaname = 'bss' AND tablename = 'pricing_price' ORDER BY indexname",
    )
    .await
}

fn seed() -> Vec<String> {
    let price = |id, version, from, state| {
        format!(
            "INSERT INTO bss.pricing_price (id,tenant_id,price_book_entry_id,version_no,price_json,eligibility,effective_from,state,created_by,version,created_at,updated_at) VALUES ({},{},{},{version},'{{}}','all','{from}','{state}',{},1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            q(id),
            q(TENANT),
            q(ENTRY),
            q(AUTHOR)
        )
    };
    vec![
        format!(
            "INSERT INTO bss.pricing_price_book (id,tenant_id,code,name,currency,version,created_at,updated_at) VALUES ({},{},'eur','eur','EUR',1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            q(BOOK),
            q(TENANT)
        ),
        format!(
            "INSERT INTO bss.pricing_price_book_entry (id,tenant_id,book_id,sku_id,charge_kind,period,reservation_id,reference_state,version,created_at,updated_at,model) VALUES ({},{},{},{},'recurring','month',{},'confirmed',1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','flat')",
            q(ENTRY),
            q(TENANT),
            q(BOOK),
            q(Uuid::from_u128(0x2240)),
            q(Uuid::from_u128(0x2241))
        ),
        price(PRICE_A, 1, "2026-01-01", "approved"),
        price(PRICE_B, 2, "2026-03-01", "approved"),
        price(PAIR_A, 3, "2026-06-01", "draft"),
        price(PAIR_B, 4, "2026-07-01", "draft"),
        format!(
            "UPDATE bss.pricing_price SET paired_price_id = {} WHERE id = {}",
            q(PAIR_B),
            q(PAIR_A)
        ),
        format!(
            "UPDATE bss.pricing_price SET paired_price_id = {} WHERE id = {}",
            q(PAIR_A),
            q(PAIR_B)
        ),
    ]
}

/// An applied change keeps the start of the price it names (review RF-P item 9): `down` is
/// refused on the approved-start index, and the runner's transaction leaves the schema as it was.
async fn down_refuses_an_applied_change_on_its_prices_start(pg: &Pg, narrowed: &[String]) {
    exec(
        pg,
        &format!(
            "INSERT INTO bss.pricing_price (id,tenant_id,price_book_entry_id,version_no,price_json,eligibility,effective_from,effective_to,state,change_kind,target_price_id,created_by,version,created_at,updated_at) VALUES ({},{},{},5,'{{}}','all','2026-01-01','2026-02-01','approved','end',{},{},1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            q(CHANGE),
            q(TENANT),
            q(ENTRY),
            q(PRICE_A),
            q(AUTHOR)
        ),
    )
    .await;
    {
        use sea_orm::TransactionTrait;
        let migration = BssPricingGear::default()
            .migrations()
            .into_iter()
            .find(|m| m.name() == MIGRATION)
            .unwrap();
        let conn = pg.raw().await;
        let txn = conn.begin().await.unwrap();
        let refused = migration.down(&SchemaManager::new(&txn)).await;
        txn.rollback().await.unwrap();
        let refused = refused.expect_err("an applied change on its price's start refuses down");
        assert!(
            refused.to_string().contains("pricing_price_approved_start"),
            "the approved-start index refuses it: {refused}"
        );
    }
    assert_eq!(index_defs(pg).await, narrowed, "the schema is as it was");
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_adds_cancel_and_end_and_round_trips() {
    let pg = Pg::empty().await;
    prior(&pg).await;
    for sql in seed() {
        exec(&pg, &sql).await;
    }
    let indexes = index_defs(&pg).await;
    step(&pg, false).await;
    let narrowed: Vec<String> = indexes
        .iter()
        .map(|def| {
            if def.contains("pricing_price_approved_start") {
                def.replace(
                    "WHERE (state = 'approved'::text)",
                    "WHERE ((state = 'approved'::text) AND (change_kind = 'set'::text))",
                )
            } else {
                def.clone()
            }
        })
        .collect();
    assert_ne!(narrowed, indexes, "the approved start narrows");
    assert_eq!(index_defs(&pg).await, narrowed);
    let cols = strings(
        &pg,
        "SELECT column_name::text AS v FROM information_schema.columns WHERE table_schema = 'bss' AND table_name = 'pricing_price'",
    )
    .await;
    for name in ["change_kind", "target_price_id", "cancelled_by_unit_id"] {
        assert!(cols.iter().any(|c| c == name), "{cols:?}");
    }
    let kinds = strings(
        &pg,
        "SELECT change_kind AS v FROM bss.pricing_price ORDER BY version_no",
    )
    .await;
    assert_eq!(kinds, vec!["set", "set", "set", "set"]);
    let chain = strings(
        &pg,
        "SELECT effective_from::text AS v FROM bss.pricing_price WHERE state = 'approved' ORDER BY version_no",
    )
    .await;
    assert_eq!(chain, vec!["2026-01-01", "2026-03-01"]);
    let pair = strings(
        &pg,
        &format!(
            "SELECT paired_price_id::text AS v FROM bss.pricing_price WHERE id = {}",
            q(PAIR_A)
        ),
    )
    .await;
    assert_eq!(pair, vec![PAIR_B.to_string()]);
    // The pairings: a change names its price and a price names none; a cancelled price names the
    // unit that cancelled it, and only a cancelled price names one.
    exec(
        &pg,
        &format!(
            "INSERT INTO bss.pricing_approval_unit (id,tenant_id,kind,ref_type,ref_id,state,common_effective_date,quorum_required,generation,submitted_by,submitted_at,decided_at,decided_note,snapshot,snapshot_hash,version) \
             VALUES ({},{},'prices','price_book',{},'pending',NULL,1,1,{},now(),NULL,NULL,'{{}}'::jsonb,'seed',1)",
            q(UNIT),
            q(TENANT),
            q(BOOK),
            q(AUTHOR)
        ),
    )
    .await;
    for (sql, what) in [
        (
            format!(
                "UPDATE bss.pricing_price SET change_kind = 'cancel' WHERE id = {}",
                q(PRICE_B)
            ),
            "a change that names no price",
        ),
        (
            format!(
                "UPDATE bss.pricing_price SET target_price_id = {} WHERE id = {}",
                q(PRICE_A),
                q(PRICE_B)
            ),
            "a price that names another",
        ),
        (
            format!(
                "UPDATE bss.pricing_price SET state = 'cancelled' WHERE id = {}",
                q(PRICE_B)
            ),
            "a cancelled price that names no unit",
        ),
        (
            format!(
                "UPDATE bss.pricing_price SET cancelled_by_unit_id = {} WHERE id = {}",
                q(UNIT),
                q(PRICE_B)
            ),
            "a unit on a price that is not cancelled",
        ),
    ] {
        assert!(try_exec(&pg, &sql).await.is_err(), "{what}: {sql}");
    }
    exec(
        &pg,
        &format!(
            "UPDATE bss.pricing_price SET state = 'cancelled', cancelled_by_unit_id = {} WHERE id = {}",
            q(UNIT),
            q(PRICE_B)
        ),
    )
    .await;
    assert!(
        try_exec(
            &pg,
            &format!(
                "UPDATE bss.pricing_price SET state = 'retired' WHERE id = {}",
                q(PRICE_A)
            ),
        )
        .await
        .is_err()
    );
    assert!(
        try_exec(
            &pg,
            &format!(
                "UPDATE bss.pricing_price SET change_kind = 'move' WHERE id = {}",
                q(PRICE_A)
            ),
        )
        .await
        .is_err()
    );
    exec(
        &pg,
        &format!(
            "UPDATE bss.pricing_price SET state = 'approved', cancelled_by_unit_id = NULL WHERE id = {}",
            q(PRICE_B)
        ),
    )
    .await;
    down_refuses_an_applied_change_on_its_prices_start(&pg, &narrowed).await;
    // A draft change goes back as a plain row.
    exec(
        &pg,
        &format!(
            "UPDATE bss.pricing_price SET state = 'draft' WHERE id = {}",
            q(CHANGE)
        ),
    )
    .await;
    step(&pg, true).await;
    let restored = strings(
        &pg,
        "SELECT column_name::text AS v FROM information_schema.columns WHERE table_schema = 'bss' AND table_name = 'pricing_price'",
    )
    .await;
    for name in ["change_kind", "target_price_id", "cancelled_by_unit_id"] {
        assert!(!restored.iter().any(|c| c == name), "{restored:?}");
    }
    assert_eq!(index_defs(&pg).await, indexes, "down restores the index");
    let still = strings(
        &pg,
        "SELECT effective_from::text AS v FROM bss.pricing_price WHERE state = 'approved' ORDER BY version_no",
    )
    .await;
    assert_eq!(still, vec!["2026-01-01", "2026-03-01"]);
    let plain = strings(
        &pg,
        &format!(
            "SELECT state || ' ' || effective_from::text AS v FROM bss.pricing_price WHERE id = {}",
            q(CHANGE)
        ),
    )
    .await;
    assert_eq!(plain, ["draft 2026-01-01"]);
    assert!(
        try_exec(
            &pg,
            &format!(
                "UPDATE bss.pricing_price SET state = 'cancelled' WHERE id = {}",
                q(PRICE_B)
            ),
        )
        .await
        .is_err()
    );
    step(&pg, false).await;
    let again = strings(
        &pg,
        "SELECT column_name::text AS v FROM information_schema.columns WHERE table_schema = 'bss' AND table_name = 'pricing_price' AND column_name = 'change_kind'",
    )
    .await;
    assert_eq!(again, vec!["change_kind"]);
}

struct Fixed(time::OffsetDateTime);
impl Clock for Fixed {
    fn now(&self) -> time::OffsetDateTime {
        self.0
    }
}
fn day(s: &str) -> time::Date {
    time::Date::parse(s, &time::format_description::well_known::Iso8601::DATE).unwrap()
}

/// The chain on Postgres: A → B → C; cancelling B re-opens A onto C, and ending C keeps its
/// explicit end. A later price D that an acceptance binds is refused a cancel (`PRICE_BOUND`).
/// Each applied change keeps its price's start, which the narrowed approved-start index allows,
/// while a second price on a taken start is still refused.
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_a_cancel_and_an_end_apply_through_the_unit() {
    let pg = Pg::applied().await;
    let tenant = Uuid::new_v4();
    let db = DBProvider::<DbError>::new(pg.db().await);
    let today = time::OffsetDateTime::new_utc(day("2026-02-15"), time::Time::MIDNIGHT);
    let state = state_with_clock(
        db.clone(),
        Arc::new(Script::default()),
        Arc::new(Fixed(today)),
    )
    .await;
    let app = app_for(state, tenant);
    let author = user_of(tenant);
    let call = |method: &'static str, path: String, body: Value, key: Option<&'static str>| {
        let (app, author) = (app.clone(), author.clone());
        async move {
            let (status, body, _) = request(&app, &author, method, &path, body, None, key).await;
            (status, body)
        }
    };
    let (s, book) = call(
        "POST",
        "/price-books".into(),
        json!({"code":"standard","name":"Standard","currency":"EUR"}),
        Some("book"),
    )
    .await;
    assert_eq!(s, 201, "{book}");
    let (s, entry) = call(
        "POST",
        format!("/price-books/{}/entries", book["id"].as_str().unwrap()),
        json!({"usage_rating_policy":policy_support::input(),"sku_id":Uuid::new_v4(),"model":"per_unit"}),
        Some("entry"),
    )
    .await;
    assert_eq!(s, 201, "{entry}");
    let (_, _, tag) = request(
        &app,
        &author,
        "GET",
        "/approval-policy",
        json!({}),
        None,
        None,
    )
    .await;
    let (s, _, _) = request(
        &app,
        &author,
        "PUT",
        "/approval-policy",
        json!({"quorum":0}),
        Some(&tag),
        None,
    )
    .await;
    assert_eq!(s, 200);
    let entry: Uuid = entry["id"].as_str().unwrap().parse().unwrap();
    let scope = AccessScope::for_tenant(tenant);
    let conn = db.conn().unwrap();
    let stored = price_book_entry_repo::find(&conn, &scope, tenant, entry)
        .await
        .unwrap()
        .unwrap();
    let mut chain = Vec::new();
    for (version_no, from) in [(1, "2026-01-01"), (2, "2026-03-01"), (3, "2026-05-01")] {
        let mut price = entry_support::price(&stored);
        price.version_no = version_no;
        price.state = "approved".into();
        price.effective_from = day(from);
        chain.push(price_repo::insert(&conn, &scope, price).await.unwrap().id);
    }
    let [a, b, c] = chain[..] else {
        unreachable!("three prices")
    };
    let (s, cancel) = call(
        "POST",
        format!("/prices/{b}/cancel"),
        json!({}),
        Some("cancel"),
    )
    .await;
    assert_eq!(s, 201, "{cancel}");
    let (s, receipt) = call(
        "POST",
        format!("/prices/{}/submit", cancel["id"].as_str().unwrap()),
        json!({}),
        Some("submit-cancel"),
    )
    .await;
    assert_eq!(s, 201, "{receipt}");
    assert_eq!(receipt["applied"], true);
    let (s, end) = call(
        "POST",
        format!("/prices/{c}/end"),
        json!({"effective_to": "2026-06-01"}),
        Some("end"),
    )
    .await;
    assert_eq!(s, 201, "{end}");
    let (s, receipt) = call(
        "POST",
        format!("/prices/{}/submit", end["id"].as_str().unwrap()),
        json!({}),
        Some("submit-end"),
    )
    .await;
    assert_eq!(s, 201, "{receipt}");
    assert_eq!(receipt["applied"], true);
    let rows = price_repo::for_entry(&conn, &scope, tenant, entry)
        .await
        .unwrap();
    let row = |id: Uuid| rows.iter().find(|r| r.id == id).unwrap();
    assert_eq!(row(a).effective_to, Some(day("2026-05-01")));
    assert!(!row(a).closed_explicitly);
    assert_eq!(row(b).state, "cancelled");
    assert!(row(b).cancelled_by_unit_id.is_some());
    assert_eq!(row(c).effective_to, Some(day("2026-06-01")));
    assert!(row(c).closed_explicitly);
    let applied: Vec<_> = rows
        .iter()
        .filter(|r| r.change_kind != "set")
        .map(|r| (r.change_kind.as_str(), r.state.as_str(), r.effective_from))
        .collect();
    assert_eq!(
        applied,
        [
            ("cancel", "approved", day("2026-03-01")),
            ("end", "approved", day("2026-05-01")),
        ]
    );
    // D-520 amended: a consumer's binding refuses a cancel. The acceptance's receipt is text on
    // Postgres too, and only its bindings count.
    let mut later = entry_support::price(&stored);
    later.version_no = 8;
    later.state = "approved".into();
    later.effective_from = day("2026-07-01");
    let bound = price_repo::insert(&conn, &scope, later).await.unwrap().id;
    let mut receipt =
        wire::decode_acceptance(include_str!("commercial_receipts/acceptance-v1.json")).unwrap();
    receipt.acceptance_id = Uuid::now_v7();
    receipt.query.tenant_axes.seller_tenant_id = tenant;
    receipt.bindings[0].price.price_id = bound;
    acceptance_repo::insert(
        &conn,
        &scope,
        acceptance_repo::from_receipt(&receipt, AUTHOR).unwrap(),
    )
    .await
    .unwrap();
    let (s, refused) = call(
        "POST",
        format!("/prices/{bound}/cancel"),
        json!({}),
        Some("cancel-bound"),
    )
    .await;
    assert_eq!(s, 409, "{refused}");
    assert!(refused.to_string().contains("PRICE_BOUND"), "{refused}");
    let mut twin = entry_support::price(&stored);
    twin.version_no = 9;
    twin.state = "approved".into();
    twin.effective_from = day("2026-05-01");
    assert!(
        price_repo::insert(&conn, &scope, twin).await.is_err(),
        "one price per approved start"
    );
}
