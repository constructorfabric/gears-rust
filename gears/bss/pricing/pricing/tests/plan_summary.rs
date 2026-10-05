//! D-484, D-485: the stored plan summary and the plans list query (run 9.8b).
//! Red first: the chain names `000020`, the list serves the axes and a page, the counts
//! route answers, and a source scan requires every plan write to refresh the summary.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
use bss_pricing::domain::plan::{self, StoredRevision};
use bss_pricing::infra::plan_summary::{RevisionFact, change, selling, summarize};
use bss_pricing::infra::reference_ticker::Ticker;
use bss_pricing::infra::reference_work::Clock;
use bss_pricing::infra::storage::repo::{book_repo, plan_repo, plan_revision_repo};
use bss_pricing::infra::storage::{entity::plan as plan_e, migrations::Migrator};
use bss_products_sdk::models::SkuType;
use plan_support::entry_support::{enforcer_for, production};
use plan_support::{
    Catalog, Fixture, book, entry_support, id_of, item, plan, policy_entry, scope, setup,
};
use sea_orm::{
    ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, EntityTrait, Statement,
};
use sea_orm_migration::{MigratorTrait, SchemaManager};
use serde_json::{Value, json};
use std::sync::Arc;
use time::{Date, Duration, OffsetDateTime};
use toolkit_security::SecurityContext;
use uuid::Uuid;

async fn get(f: &Fixture, path: &str) -> (u16, Value) {
    let (s, b, _) = f.call("GET", path, json!({}), None, None).await;
    (s, b)
}

#[test]
fn the_chain_names_the_plan_summary_migration() {
    let names: Vec<_> = Migrator::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    assert!(
        names.iter().any(|n| n == "m20261002_000020_plan_summary"),
        "pricing 000020 is missing: {names:?}"
    );
    assert_eq!(
        names.last().map(String::as_str),
        Some("m20261003_000023_book_archive")
    );
}

#[tokio::test]
async fn the_plans_list_serves_the_axes_the_page_and_the_book() {
    let (f, _) = setup().await;
    let eur = book(&f, "eur").await;
    let (created, _) = plan(&f, "alpha", eur).await;
    let (status, listed) = get(&f, "/plans").await;
    assert_eq!(status, 200, "{listed}");
    assert!(listed["page_info"]["limit"].is_number(), "{listed}");
    assert_eq!(listed["page_info"]["limit"], 500, "{listed}");
    let row = &listed["items"][0];
    assert_eq!(row["id"], created["id"]);
    assert_eq!(row["selling"], false, "{row}");
    assert_eq!(row["change"], "draft", "{row}");
    assert!(row["last_activity_at"].is_string(), "{row}");
    assert_eq!(row["current"]["book"]["currency"], "EUR", "{row}");
    assert_eq!(row["current"]["book"]["code"], "eur", "{row}");
    let (status, one) = get(&f, &format!("/plans/{}", id_of(&created["id"]))).await;
    assert_eq!(status, 200, "{one}");
    assert_eq!(
        one["id"], created["id"],
        "GET /plans/{{id}} is the plan, not the counts"
    );
    assert!(one.get("by_selling").is_none(), "{one}");
}

#[tokio::test]
async fn the_plans_counts_are_one_grouped_read_and_name_every_bucket() {
    let (f, _) = setup().await;
    let eur = book(&f, "eur").await;
    plan(&f, "alpha", eur).await;
    let (status, counts) = get(&f, "/plans/counts").await;
    assert_eq!(status, 200, "{counts}");
    let t = counts["by_selling"]["true"].as_u64().unwrap();
    let f_ = counts["by_selling"]["false"].as_u64().unwrap();
    assert_eq!(t + f_, counts["total"].as_u64().unwrap(), "{counts}");
    assert_eq!(counts["by_change"]["draft"], 1, "{counts}");
    for key in ["none", "draft", "pending", "scheduled"] {
        assert!(counts["by_change"][key].is_number(), "{key}: {counts}");
    }
}

#[tokio::test]
async fn a_plain_selling_key_is_a_list_parameter() {
    let (f, _) = setup().await;
    let (status, body) = get(&f, "/plans?selling=false").await;
    assert_eq!(status, 200, "{body}");
    assert!(body["items"].is_array(), "{body}");
}

#[test]
fn every_plan_write_refreshes_the_summary_or_is_allowed() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/infra/storage/repo");
    let mut misses = Vec::new();
    for file in ["plan_repo.rs", "plan_revision_repo.rs"] {
        let source = std::fs::read_to_string(root.join(file)).unwrap();
        for (name, body) in writers(&source) {
            let calls = body.contains("plan_summary::refresh");
            let allowed = matches!(name.as_str(), "advance_published" | "delete_unpublished");
            if !calls && !allowed {
                misses.push(format!("{file}::{name}"));
            }
            if calls && allowed {
                misses.push(format!("{file}::{name} is allowed and also refreshes"));
            }
        }
    }
    assert!(
        misses.is_empty(),
        "a plan write must refresh the summary or be on the allow-list: {misses:?}"
    );
    // Positive control: the matcher sees a write, and only a write.
    let sample = "pub async fn poke() { e::Entity::update_many().exec(runner).await?; }\n\
                  pub async fn read() { e::Entity::find().one(runner).await?; }\n\
                  async fn patched() { row.update(runner).await?; }\n\
                  async fn saved() { row.save(runner).await?; }\n\
                  async fn removed() { row.delete(runner).await?; }\n\
                  pub async fn kept() { e::Entity::insert(active).exec(runner).await?; plan_summary::refresh(runner).await?; }";
    let found: Vec<_> = writers(sample);
    assert_eq!(
        found
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["poke", "patched", "saved", "removed", "kept"],
        "{found:?}"
    );
    assert!(!found[0].1.contains("plan_summary::refresh"));
    assert!(found[4].1.contains("plan_summary::refresh"));
}

/// `pub async fn` bodies that insert, update or delete.
fn writers(source: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find("async fn ") {
        rest = &rest[at + "async fn ".len()..];
        let name = rest.split(['(', '<']).next().unwrap().trim().to_owned();
        let body_at = rest.find('{').unwrap();
        let body = brace_body(&rest[body_at..]);
        let writes = [
            "update_many",
            "delete_many",
            "::insert(",
            ".insert(",
            ".update(",
            ".save(",
            ".delete(",
        ]
        .iter()
        .any(|needle| body.contains(needle));
        if writes {
            out.push((name, body.to_owned()));
        }
        rest = &rest[body_at + body.len()..];
    }
    out
}

fn brace_body(source: &str) -> &str {
    let mut depth = 0_i32;
    for (i, ch) in source.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[..=i];
                }
            }
            _ => {}
        }
    }
    source
}

#[tokio::test]
async fn the_list_reads_five_statements_for_10_and_for_100_plans() {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let catalog = Arc::new(Catalog::default());
    let f = Fixture::on(db, tenant, dsn, catalog).await;
    let eur = book(&f, "eur").await;
    for i in 0..10 {
        plan(&f, &format!("p{i:03}"), eur).await;
    }
    let ten = statements(&f, &recorder).await;
    for i in 10..100 {
        plan(&f, &format!("p{i:03}"), eur).await;
    }
    let hundred = statements(&f, &recorder).await;
    assert_eq!(ten.len(), 5, "{ten:#?}");
    assert_eq!(ten, hundred, "the same statements, whatever the page");
    let _ = id_of;
}

fn encode_query(raw: &str) -> String {
    let mut out = String::new();
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                out.push(char::from(b));
            }
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(char::from(HEX[usize::from(b >> 4)]));
                out.push(char::from(HEX[usize::from(b & 0x0f)]));
            }
        }
    }
    out
}

fn blob(id: Uuid) -> String {
    format!("X'{}'", id.simple())
}

async fn sqlite(db: &DatabaseConnection, sql: String) {
    db.execute_raw(Statement::from_string(DatabaseBackend::Sqlite, sql))
        .await
        .unwrap();
}

fn book_sql(tenant: Uuid, id: Uuid, code: &str, currency: &str) -> String {
    format!(
        "INSERT INTO pricing_price_book (id,tenant_id,code,name,currency,version,created_at,updated_at) \
         VALUES ({},{},'{code}','{code}','{currency}',1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        blob(id),
        blob(tenant)
    )
}

fn plan_sql(tenant: Uuid, author: Uuid, id: Uuid, code: &str, updated: &str) -> String {
    format!(
        "INSERT INTO pricing_plan (id,tenant_id,code,name,published_rev,version,created_by,created_at,updated_at) \
         VALUES ({},{},'{code}','{code}',NULL,1,{},'2026-01-01T00:00:00Z','{updated}')",
        blob(id),
        blob(tenant),
        blob(author)
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "one SQL row's columns, named at the call"
)]
fn revision_sql(
    tenant: Uuid,
    author: Uuid,
    id: Uuid,
    plan: Uuid,
    book: Uuid,
    no: i32,
    state: &str,
    from: &str,
    updated: &str,
) -> String {
    let from_sql = if from.is_empty() {
        "NULL".to_owned()
    } else {
        format!("'{from}'")
    };
    format!(
        "INSERT INTO pricing_plan_revision (id,tenant_id,plan_id,rev_no,book_id,state,available_from,version,created_by,created_at,updated_at) \
         VALUES ({},{},{},{no},{},'{state}',{from_sql},1,{},'2026-01-01T00:00:00Z','{updated}')",
        blob(id),
        blob(tenant),
        blob(plan),
        blob(book),
        blob(author)
    )
}

struct ShapeIds {
    none: Uuid,
    eur: Uuid,
    usd: Uuid,
}

/// Seeds the six shapes the backfill must fill: no revisions, draft only, draft beside a
/// published revision, a future schedule, a due schedule, and superseded history.
async fn seed_shapes(db: &DatabaseConnection) -> ShapeIds {
    let tenant = Uuid::from_u128(0x10);
    let author = Uuid::from_u128(0x11);
    let eur = Uuid::from_u128(0x20);
    let usd = Uuid::from_u128(0x21);
    sqlite(db, book_sql(tenant, eur, "eur", "EUR")).await;
    sqlite(db, book_sql(tenant, usd, "usd", "USD")).await;
    let none = Uuid::from_u128(0x30);
    let draft_only = Uuid::from_u128(0x31);
    let beside = Uuid::from_u128(0x32);
    let future = Uuid::from_u128(0x33);
    let due = Uuid::from_u128(0x34);
    let history = Uuid::from_u128(0x35);
    for (id, code, updated) in [
        (none, "NONE", "2026-01-02T00:00:00Z"),
        (draft_only, "DRAFT", "2026-01-03T00:00:00Z"),
        (beside, "BESIDE", "2026-01-04T00:00:00Z"),
        (future, "FUTURE", "2026-01-05T00:00:00Z"),
        (due, "DUE", "2026-01-06T00:00:00Z"),
        (history, "HISTORY", "2026-01-07T00:00:00Z"),
    ] {
        sqlite(db, plan_sql(tenant, author, id, code, updated)).await;
    }
    let rows = [
        (
            0x41,
            draft_only,
            eur,
            1,
            "draft",
            "",
            "2026-02-01T00:00:00Z",
        ),
        (
            0x42,
            beside,
            eur,
            1,
            "published",
            "",
            "2026-02-02T00:00:00Z",
        ),
        (0x43, beside, usd, 2, "draft", "", "2026-03-01T00:00:00Z"),
        (
            0x44,
            future,
            eur,
            1,
            "published",
            "",
            "2026-02-03T00:00:00Z",
        ),
        (
            0x45,
            future,
            usd,
            2,
            "scheduled",
            "2026-12-01",
            "2026-03-02T00:00:00Z",
        ),
        (0x46, due, eur, 1, "published", "", "2026-02-04T00:00:00Z"),
        (
            0x47,
            due,
            usd,
            2,
            "scheduled",
            "2020-01-01",
            "2026-03-03T00:00:00Z",
        ),
        (
            0x48,
            history,
            eur,
            1,
            "superseded",
            "",
            "2026-04-01T00:00:00Z",
        ),
        (
            0x49,
            history,
            usd,
            2,
            "published",
            "",
            "2026-02-05T00:00:00Z",
        ),
    ];
    for (id, plan, book, no, state, from, updated) in rows {
        sqlite(
            db,
            revision_sql(
                tenant,
                author,
                Uuid::from_u128(id),
                plan,
                book,
                no,
                state,
                from,
                updated,
            ),
        )
        .await;
    }
    ShapeIds { none, eur, usd }
}

#[allow(
    clippy::disallowed_methods,
    reason = "the backfill proof reads every plan after the migration, which has no caller scope"
)]
#[allow(
    clippy::cognitive_complexity,
    reason = "each seeded shape is one straight block of column asserts"
)]
async fn filled_shapes(db: &DatabaseConnection, ids: &ShapeIds) {
    let rows = plan_e::Entity::find().all(db).await.unwrap();
    let row = |code: &str| rows.iter().find(|p| p.code == code).unwrap();
    let empty = row("NONE");
    assert!(empty.work_revision_id.is_none());
    assert!(empty.published_revision_id.is_none());
    assert!(empty.current_book_id.is_none());
    assert_eq!(empty.last_activity_at, empty.updated_at);
    let only = row("DRAFT");
    assert_eq!(only.work_state.as_deref(), Some("draft"));
    assert_eq!(only.work_revision_id, Some(Uuid::from_u128(0x41)));
    assert_eq!(only.current_book_id, Some(ids.eur));
    assert_eq!(only.current_currency.as_deref(), Some("EUR"));
    let mixed = row("BESIDE");
    assert_eq!(mixed.work_revision_id, Some(Uuid::from_u128(0x43)));
    assert_eq!(mixed.published_revision_id, Some(Uuid::from_u128(0x42)));
    assert_eq!(mixed.current_book_id, Some(ids.usd));
    assert_eq!(mixed.current_currency.as_deref(), Some("USD"));
    let waiting = row("FUTURE");
    assert_eq!(waiting.scheduled_revision_id, Some(Uuid::from_u128(0x45)));
    assert_eq!(
        waiting.scheduled_from.map(|d| d.to_string()).as_deref(),
        Some("2026-12-01")
    );
    assert_eq!(waiting.published_revision_id, Some(Uuid::from_u128(0x44)));
    assert_eq!(waiting.current_book_id, Some(ids.usd));
    let arrived = row("DUE");
    assert_eq!(
        arrived.scheduled_from.map(|d| d.to_string()).as_deref(),
        Some("2020-01-01")
    );
    assert_eq!(arrived.published_revision_id, Some(Uuid::from_u128(0x46)));
    assert!(arrived.work_revision_id.is_none());
    let past = row("HISTORY");
    assert!(past.work_revision_id.is_none());
    assert!(past.scheduled_revision_id.is_none());
    assert_eq!(past.published_revision_id, Some(Uuid::from_u128(0x49)));
    assert_eq!(past.current_book_id, Some(ids.usd));
    assert!(past.last_activity_at > past.updated_at);
}

/// D-484: both backends' backfill is the sqlite half here. The shapes are seeded on the chain
/// before `000020`, then the migration fills the summary by id.
#[tokio::test]
async fn the_backfill_fills_every_seeded_shape_and_the_checks_pair_the_columns() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    let chain = Migrator::migrations();
    let at = chain
        .iter()
        .position(|m| m.name() == "m20261002_000020_plan_summary")
        .unwrap();
    for prior in &chain[..at] {
        prior.up(&manager).await.unwrap();
    }
    let ids = seed_shapes(&db).await;
    chain[at].up(&manager).await.unwrap();
    chain[at].up(&manager).await.unwrap();
    filled_shapes(&db, &ids).await;
    let refused = db
        .execute_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "UPDATE pricing_plan SET work_state = 'draft' WHERE id = {}",
                blob(ids.none)
            ),
        ))
        .await;
    assert!(refused.is_err(), "a state without its revision");
    let down = chain[at].down(&manager).await.unwrap_err().to_string();
    assert!(down.contains("m20261002_000020_plan_summary"), "{down}");
    assert!(down.contains("irreversible"), "{down}");
}

#[tokio::test]
async fn the_list_filters_orders_and_rejects_a_cursor_under_another_narrowing() {
    let (f, _) = setup().await;
    let eur = book(&f, "eur").await;
    let usd = book(&f, "usd").await;
    plan(&f, "alpha", eur).await;
    plan(&f, "beta", usd).await;
    let (status, by_name) = get(&f, "/plans?$orderby=name%20desc&limit=1").await;
    assert_eq!(status, 200, "{by_name}");
    assert_eq!(by_name["items"].as_array().unwrap().len(), 1, "{by_name}");
    assert_eq!(by_name["page_info"]["limit"], 1, "{by_name}");
    let cursor = by_name["page_info"]["next_cursor"].as_str().unwrap();
    let cursor = encode_query(cursor);
    let (status, second) = get(&f, &format!("/plans?cursor={cursor}")).await;
    assert_eq!(status, 200, "{second}");
    assert_ne!(second["items"][0]["id"], by_name["items"][0]["id"]);
    let (status, mismatch) = get(&f, &format!("/plans?cursor={cursor}&q=nope")).await;
    assert_eq!(status, 400, "{mismatch}");
    assert!(
        mismatch.to_string().contains("FILTER_MISMATCH"),
        "{mismatch}"
    );
    let (status, selling) = get(&f, "/plans?selling=true").await;
    assert_eq!(status, 200, "{selling}");
    assert_eq!(selling["items"].as_array().unwrap().len(), 0, "{selling}");
    let (status, change) = get(&f, "/plans?change=draft,none").await;
    assert_eq!(status, 200, "{change}");
    assert_eq!(change["items"].as_array().unwrap().len(), 2, "{change}");
    let (status, bad) = get(&f, "/plans?change=retired").await;
    assert_eq!(status, 400, "{bad}");
    let (status, unordered) = get(&f, "/plans?$orderby=book_id").await;
    assert_eq!(status, 400, "{unordered}");
    let (status, clamped) = get(&f, "/plans?$top=1000").await;
    assert_eq!(status, 200, "{clamped}");
    assert_eq!(clamped["page_info"]["limit"], 500, "{clamped}");
}

async fn statements(
    f: &Fixture,
    recorder: &toolkit_db::test_support::QueryRecorder,
) -> Vec<String> {
    recorder.clear();
    let (status, body) = get(f, "/plans").await;
    assert_eq!(status, 200, "{body}");
    recorder
        .events()
        .into_iter()
        .filter(|q| {
            q.table
                .as_deref()
                .is_some_and(|t| t.starts_with("pricing_"))
        })
        .map(|q| q.sql)
        .collect()
}

struct FixedClock(OffsetDateTime);
impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

fn noon(day: Date) -> Arc<dyn Clock> {
    Arc::new(FixedClock(
        day.midnight().assume_utc() + Duration::hours(12),
    ))
}

fn wall_today() -> Date {
    OffsetDateTime::now_utc().date()
}

async fn opened(
    f: &Fixture,
    day: Date,
) -> (
    axum::Router,
    Arc<bss_pricing::api::rest::authoring::AuthoringState>,
) {
    let db = toolkit_db::connect_db(
        &f.dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..toolkit_db::ConnectOpts::default()
        },
    )
    .await
    .unwrap();
    let state = Arc::new(
        bss_pricing::api::rest::authoring::AuthoringState::new(
            toolkit_db::DBProvider::new(db),
            f.state.hub.clone(),
        )
        .await
        .unwrap()
        .with_clock(noon(day)),
    );
    let app =
        production(state.clone()).layer(axum::Extension(enforcer_for(f.ctx.subject_tenant_id())));
    (app, state)
}

async fn http(
    app: &axum::Router,
    ctx: &SecurityContext,
    method: &str,
    path: &str,
    body: Value,
    key: Option<&str>,
) -> (u16, Value) {
    let (status, body, _) = entry_support::request(app, ctx, method, path, body, None, key).await;
    (status, body)
}

async fn read_json(app: &axum::Router, ctx: &SecurityContext, path: &str) -> Value {
    let (status, body) = http(app, ctx, "GET", path, json!({}), None).await;
    assert_eq!(status, 200, "{path}: {body}");
    body
}

/// The stored summary, the SQL axes and `domain::plan` agree for this plan on `today`.
#[allow(
    clippy::cognitive_complexity,
    reason = "one plan's stored summary, domain axes and list answer, checked in that order"
)]
async fn agree(f: &Fixture, app: &axum::Router, plan_id: Uuid, today: Date, only: bool) {
    let conn = f.db.conn().unwrap();
    let tenant = f.ctx.subject_tenant_id();
    let children = scope(f);
    let stored = plan_repo::find(&conn, &children, tenant, plan_id)
        .await
        .unwrap()
        .unwrap();
    let revisions = plan_revision_repo::for_plan(&conn, &children, tenant, plan_id)
        .await
        .unwrap();
    let mut facts = Vec::new();
    let mut stored_revs = Vec::new();
    for revision in &revisions {
        let book = book_repo::find(&conn, &children, tenant, revision.book_id)
            .await
            .unwrap()
            .unwrap();
        facts.push(RevisionFact {
            id: revision.id,
            state: revision.state.clone(),
            available_from: revision.available_from,
            updated_at: revision.updated_at,
            book_id: revision.book_id,
            currency: Some(book.currency),
        });
        stored_revs.push(StoredRevision {
            id: revision.id,
            plan_id: revision.plan_id,
            rev_no: revision.rev_no,
            state: revision.state.parse().unwrap(),
            available_from: revision.available_from,
            published_at: revision.published_at,
        });
    }
    let summary = summarize(stored.updated_at, &facts).unwrap();
    assert_eq!(stored.work_revision_id, summary.work_revision_id);
    assert_eq!(stored.work_state, summary.work_state);
    assert_eq!(stored.scheduled_revision_id, summary.scheduled_revision_id);
    assert_eq!(stored.scheduled_from, summary.scheduled_from);
    assert_eq!(stored.published_revision_id, summary.published_revision_id);
    assert_eq!(stored.current_book_id, summary.current_book_id);
    assert_eq!(stored.current_currency, summary.current_currency);
    assert_eq!(stored.last_activity_at, summary.last_activity_at);
    let effective = plan::effective(&stored_revs, today);
    let current = plan::current(&effective);
    let want_selling = selling(&summary, today);
    let want_change = change(&summary, today);
    assert_eq!(want_selling, plan::in_effect(&effective).is_some());
    let from_current = match current.map(|row| row.state) {
        Some(plan::RevisionState::Draft) => "draft",
        Some(plan::RevisionState::Pending) => "pending",
        Some(plan::RevisionState::Scheduled) => "scheduled",
        _ => "none",
    };
    assert_eq!(want_change.as_str(), from_current);
    let listed = read_json(app, &f.ctx, "/plans").await;
    let row = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == plan_id.to_string())
        .unwrap_or_else(|| panic!("plan {plan_id} missing: {listed}"));
    assert_eq!(row["selling"], want_selling, "{row}");
    assert_eq!(row["change"], want_change.as_str(), "{row}");
    let kept = read_json(
        app,
        &f.ctx,
        &format!(
            "/plans?selling={want_selling}&change={}",
            want_change.as_str()
        ),
    )
    .await;
    assert!(
        kept["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == plan_id.to_string()),
        "the SQL axes keep it: {kept}"
    );
    let other = !want_selling;
    let dropped = read_json(app, &f.ctx, &format!("/plans?selling={other}")).await;
    assert!(
        dropped["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["id"] != plan_id.to_string()),
        "the other selling axis drops it: {dropped}"
    );
    if only {
        let counts = read_json(app, &f.ctx, "/plans/counts").await;
        let flag = if want_selling { "true" } else { "false" };
        assert_eq!(counts["total"], 1, "{counts}");
        assert_eq!(counts["by_selling"][flag], 1, "{counts}");
        assert_eq!(counts["by_change"][want_change.as_str()], 1, "{counts}");
    }
}

async fn quorum(f: &Fixture, n: u32) {
    let (_, _, tag) = f
        .call("GET", "/approval-policy", json!({}), None, None)
        .await;
    let (status, body, _) = f
        .call(
            "PUT",
            "/approval-policy",
            json!({"kind": "plan_revision", "quorum": n}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(status, 200, "{body}");
}

async fn submit(f: &Fixture, revision: Uuid, key: &str) -> Value {
    let (status, body, _) = f
        .call(
            "POST",
            &format!("/plan-revisions/{revision}/submit"),
            json!({}),
            None,
            Some(key),
        )
        .await;
    assert_eq!(status, 201, "{body}");
    body
}

async fn vote(
    f: &Fixture,
    who: &SecurityContext,
    unit: &str,
    action: &str,
    body: Value,
    key: &str,
) {
    let (status, body, _) = f
        .call_as(
            who,
            "POST",
            &format!("/approval-units/{unit}/{action}"),
            body,
            None,
            Some(key),
        )
        .await;
    assert_eq!(status, 200, "{action}: {body}");
}

async fn patch_book(f: &Fixture, revision: Uuid, book: Uuid) {
    let path = format!("/plan-revisions/{revision}");
    let (_, _, tag) = f.call("GET", &path, json!({}), None, None).await;
    let (status, body, _) = f
        .call("PATCH", &path, json!({"book_id": book}), Some(&tag), None)
        .await;
    assert_eq!(status, 200, "{body}");
}

async fn sale_on(f: &Fixture, revision: Uuid, day: Date) {
    let path = format!("/plan-revisions/{revision}");
    let (_, _, tag) = f.call("GET", &path, json!({}), None, None).await;
    let (status, body, _) = f
        .call(
            "PATCH",
            &path,
            json!({"available_from": day.to_string()}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(status, 200, "{body}");
}

async fn copy_of(f: &Fixture, plan_id: Uuid, key: &str) -> Uuid {
    let (status, body, _) = f
        .call(
            "POST",
            &format!("/plans/{plan_id}/revisions"),
            json!({}),
            None,
            Some(key),
        )
        .await;
    assert_eq!(status, 201, "{body}");
    id_of(&body["id"])
}

async fn priced(f: &Fixture, entry: Uuid) {
    use bss_pricing::infra::storage::repo::{price_book_entry_repo, price_repo};
    let tenant = f.ctx.subject_tenant_id();
    let conn = f.db.conn().unwrap();
    let row = price_book_entry_repo::find(&conn, &scope(f), tenant, entry)
        .await
        .unwrap()
        .unwrap();
    let mut price = entry_support::price(&row);
    price.state = "approved".into();
    price.effective_from = Date::from_calendar_date(2020, time::Month::January, 1).unwrap();
    price_repo::insert(&conn, &scope(f), price).await.unwrap();
}

/// D-484: one plan through every write, then a day with no write. Unschedule runs while the date
/// is still ahead: a due revision cannot be unscheduled (D-448).
#[tokio::test]
async fn the_summary_matches_the_domain_through_the_lifecycle_and_across_midnight() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let usd = book(&f, "usd").await;
    let (created, rev) = plan(&f, "alpha", eur).await;
    let plan_id = id_of(&created["id"]);
    let today = wall_today();
    agree(&f, &f.app, plan_id, today, true).await;
    patch_book(&f, rev, usd).await;
    agree(&f, &f.app, plan_id, today, true).await;
    let sku = catalog.sku(SkuType::Usage);
    let entry = policy_entry(&f, usd, sku, "usage", None).await;
    priced(&f, entry).await;
    item(&f, rev, sku, Some(entry), "paid").await;
    agree(&f, &f.app, plan_id, today, true).await;
    quorum(&f, 1).await;
    let receipt = submit(&f, rev, "submit-1").await;
    agree(&f, &f.app, plan_id, today, true).await;
    let unit = receipt["unit"]["id"].as_str().unwrap().to_owned();
    vote(
        &f,
        &f.user(),
        &unit,
        "reject",
        json!({"generation": 1, "note": "no"}),
        "reject-1",
    )
    .await;
    agree(&f, &f.app, plan_id, today, true).await;
    let receipt = submit(&f, rev, "submit-2").await;
    agree(&f, &f.app, plan_id, today, true).await;
    let unit = receipt["unit"]["id"].as_str().unwrap().to_owned();
    vote(&f, &f.ctx, &unit, "withdraw", json!({}), "withdraw-1").await;
    agree(&f, &f.app, plan_id, today, true).await;
    let receipt = submit(&f, rev, "submit-3").await;
    let unit = receipt["unit"]["id"].as_str().unwrap().to_owned();
    vote(
        &f,
        &f.user(),
        &unit,
        "approve",
        json!({"generation": 1}),
        "approve-1",
    )
    .await;
    agree(&f, &f.app, plan_id, today, true).await;
    quorum(&f, 0).await;
    let rev2 = copy_of(&f, plan_id, "copy-1").await;
    let ahead = today + Duration::days(30);
    sale_on(&f, rev2, ahead).await;
    submit(&f, rev2, "schedule-1").await;
    agree(&f, &f.app, plan_id, today, true).await;
    let (status, body) = http(
        &f.app,
        &f.ctx,
        "POST",
        &format!("/plan-revisions/{rev2}/unschedule"),
        json!({}),
        Some("unschedule-1"),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    agree(&f, &f.app, plan_id, today, true).await;
    sale_on(&f, rev2, ahead).await;
    submit(&f, rev2, "schedule-2").await;
    agree(&f, &f.app, plan_id, today, true).await;
    let (late, state) = opened(&f, ahead).await;
    agree(&f, &late, plan_id, ahead, true).await;
    let conn = f.db.conn().unwrap();
    let waiting = plan_repo::find(&conn, &scope(&f), f.ctx.subject_tenant_id(), plan_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        waiting.scheduled_revision_id.is_some(),
        "no write moved the row"
    );
    Ticker::new(state, noon(ahead), 10, 100)
        .switch_every(1)
        .tick()
        .await
        .unwrap();
    agree(&f, &f.app, plan_id, today, true).await;
    let rev3 = copy_of(&f, plan_id, "copy-2").await;
    agree(&f, &f.app, plan_id, today, true).await;
    let (status, body) = http(
        &f.app,
        &f.ctx,
        "DELETE",
        &format!("/plan-revisions/{rev3}"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(status, 204, "{body}");
    agree(&f, &f.app, plan_id, today, true).await;
    let (status, cloned) = http(
        &f.app,
        &f.ctx,
        "POST",
        &format!("/plans/{plan_id}/clone"),
        json!({"code": "ALPHA-2", "name": "Alpha 2"}),
        Some("clone-1"),
    )
    .await;
    assert_eq!(status, 201, "{cloned}");
    agree(&f, &f.app, plan_id, today, false).await;
    agree(&f, &f.app, id_of(&cloned["id"]), today, false).await;
}

#[tokio::test]
async fn the_list_filter_names_code_name_book_currency_activity_and_sku() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let (created, rev) = plan(&f, "alpha", eur).await;
    let plan_id = id_of(&created["id"]);
    let sku = catalog.sku(SkuType::Usage);
    let other = catalog.sku(SkuType::Usage);
    let entry = policy_entry(&f, eur, sku, "usage", None).await;
    item(&f, rev, sku, Some(entry), "paid").await;
    for query in [
        "$filter=code%20eq%20%27ALPHA%27".to_owned(),
        "$filter=name%20eq%20%27Plan%20alpha%27".to_owned(),
        format!("$filter=book_id%20eq%20{eur}"),
        "$filter=currency%20eq%20%27EUR%27".to_owned(),
        "$filter=last_activity_at%20ge%202026-01-01T00:00:00Z".to_owned(),
        "q=alp".to_owned(),
        format!("sku_id={sku}"),
    ] {
        let body = read_json(&f.app, &f.ctx, &format!("/plans?{query}")).await;
        assert_eq!(
            body["items"][0]["id"],
            plan_id.to_string(),
            "{query}: {body}"
        );
    }
    let quiet = read_json(
        &f.app,
        &f.ctx,
        "/plans?$filter=last_activity_at%20lt%202020-01-01T00:00:00Z",
    )
    .await;
    assert_eq!(quiet["items"].as_array().unwrap().len(), 0, "{quiet}");
    let missed = read_json(&f.app, &f.ctx, &format!("/plans?sku_id={other}")).await;
    assert_eq!(missed["items"].as_array().unwrap().len(), 0, "{missed}");
    let ordered = read_json(&f.app, &f.ctx, "/plans?$orderby=last_activity_at%20desc").await;
    assert_eq!(ordered["items"][0]["id"], plan_id.to_string(), "{ordered}");
}

fn door_writes(recorder: &toolkit_db::test_support::QueryRecorder) -> Vec<String> {
    recorder
        .events()
        .into_iter()
        .filter_map(|query| {
            let verb = query.sql.split_whitespace().next()?.to_ascii_uppercase();
            if !matches!(verb.as_str(), "INSERT" | "UPDATE" | "DELETE") {
                return None;
            }
            let table = query.table.as_deref()?;
            if table != "pricing_plan" && table != "pricing_plan_revision" {
                return None;
            }
            let summary =
                verb == "UPDATE" && query.sql.to_ascii_lowercase().contains("last_activity_at");
            Some(if summary {
                "refresh".to_owned()
            } else {
                format!("{verb} {table}")
            })
        })
        .collect()
}

/// N2: each door's plan and revision writes, counted from the call, not a flat +2.
#[tokio::test]
async fn the_write_doors_pin_their_own_statements() {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let catalog = Arc::new(Catalog::default());
    let f = Fixture::on(db, tenant, dsn, catalog.clone()).await;
    let eur = book(&f, "eur").await;
    recorder.clear();
    let (created, rev1) = plan(&f, "alpha", eur).await;
    let create = door_writes(&recorder);
    let plan_id = id_of(&created["id"]);
    let sku = catalog.sku(SkuType::Usage);
    let entry = policy_entry(&f, eur, sku, "usage", None).await;
    priced(&f, entry).await;
    item(&f, rev1, sku, Some(entry), "paid").await;
    quorum(&f, 0).await;
    recorder.clear();
    submit(&f, rev1, "apply-none").await;
    let apply_none = door_writes(&recorder);
    let rev2 = copy_of(&f, plan_id, "copy-pin").await;
    sale_on(&f, rev2, wall_today() + Duration::days(30)).await;
    recorder.clear();
    submit(&f, rev2, "schedule-pin").await;
    let schedule_only = door_writes(&recorder);
    let conn = f.db.conn().unwrap();
    recorder.clear();
    let missed = plan_revision_repo::switch_due(
        &conn,
        &scope(&f),
        f.ctx.subject_tenant_id(),
        plan_id,
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    assert!(missed.is_none());
    let noop = door_writes(&recorder);
    let noop_sql: Vec<_> = recorder
        .events()
        .into_iter()
        .map(|query| query.sql)
        .collect();
    assert!(
        noop_sql
            .iter()
            .all(|sql| sql.trim_start().to_ascii_uppercase().starts_with("SELECT")),
        "a miss writes nothing: {noop_sql:?}"
    );
    recorder.clear();
    let switched = plan_revision_repo::switch_due(
        &conn,
        &scope(&f),
        f.ctx.subject_tenant_id(),
        plan_id,
        OffsetDateTime::now_utc() + Duration::days(30),
    )
    .await
    .unwrap();
    assert!(switched.is_some());
    let due = door_writes(&recorder);
    let rev3 = copy_of(&f, plan_id, "copy-pin-2").await;
    let path = format!("/plan-revisions/{rev3}");
    let (_, _, tag) = f.call("GET", &path, json!({}), None, None).await;
    let (status, body, _) = f
        .call(
            "PATCH",
            &path,
            json!({"available_from": null}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    recorder.clear();
    submit(&f, rev3, "apply-pred").await;
    let apply_pred = door_writes(&recorder);
    let pin = |rows: &[&str]| -> Vec<String> { rows.iter().map(|row| (*row).to_owned()).collect() };
    assert_eq!(
        create,
        pin(&[
            "INSERT pricing_plan",
            "refresh",
            "INSERT pricing_plan_revision",
            "refresh",
        ])
    );
    assert_eq!(
        apply_none,
        pin(&[
            "UPDATE pricing_plan_revision",
            "refresh",
            "UPDATE pricing_plan_revision",
            "refresh",
            "UPDATE pricing_plan",
            "refresh",
        ])
    );
    assert_eq!(
        schedule_only,
        pin(&[
            "UPDATE pricing_plan_revision",
            "refresh",
            "UPDATE pricing_plan_revision",
            "refresh",
        ])
    );
    assert_eq!(noop, pin(&[]));
    assert_eq!(noop_sql.len(), 1, "the miss is one read: {noop_sql:?}");
    assert_eq!(
        due,
        pin(&[
            "UPDATE pricing_plan_revision",
            "UPDATE pricing_plan_revision",
            "UPDATE pricing_plan",
            "refresh",
        ])
    );
    assert_eq!(
        apply_pred,
        pin(&[
            "UPDATE pricing_plan_revision",
            "refresh",
            "UPDATE pricing_plan_revision",
            "refresh",
            "UPDATE pricing_plan_revision",
            "refresh",
            "UPDATE pricing_plan",
            "refresh",
        ]),
        "{apply_pred:?}"
    );
}
