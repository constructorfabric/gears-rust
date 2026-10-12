//! Scheduled plan revisions through the production router and the ticker (phase 8 run 8.2,
//! D-449 to D-454): an approval before the sale date schedules the revision and publishes nothing;
//! the switch happens on the date — every read derives it at once, the job persists and announces
//! it once, and the three doors that can meet it (copy, clone, unschedule) catch it up first; a
//! waiting revision can be withdrawn to a draft; `/resolve` serves it from its date; the counts
//! read the stored state.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
use bss_pricing::infra::{
    reference_ticker::Ticker,
    reference_work::Clock,
    storage::repo::{plan_repo, plan_revision_repo, price_book_entry_repo, price_repo},
};
use bss_products_sdk::models::{Lifecycle, SkuType};
use plan_support::{
    Catalog, Fixture, book, entry_support, entry_support::outbox_events, holding, id_of, item,
    items, plan, policy_entry as entry, scope, setup, text,
};
use serde_json::{Value, json};
use std::sync::Arc;
use time::{Date, Duration, OffsetDateTime};
use toolkit_security::SecurityContext;
use uuid::Uuid;

const PUBLISHED: &str = "gts.cf.core.events.event.v1~cf.bss.pricing.plan_revision_published.v1~";
const DECIDED: &str = "gts.cf.core.events.event.v1~cf.bss.pricing.approval_unit_decided.v1~";

// ------------------------------------------------------------------ fixture

struct FixedClock(OffsetDateTime);
impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}
/// A clock one hour into `day` (UTC): a job tick on that day, whatever the wall clock says.
fn on(day: Date) -> Arc<dyn Clock> {
    Arc::new(FixedClock(day.midnight().assume_utc() + Duration::hours(1)))
}
fn today() -> Date {
    OffsetDateTime::now_utc().date()
}
fn days(n: i64) -> Date {
    today() + Duration::days(n)
}
/// `published_at` of a revision that took effect on `day`: 00:00 UTC.
fn midnight(day: Date) -> String {
    format!("{day}T00:00:00Z")
}

/// A plan whose rev 1 is published at once (quorum 0, no date): one confirmed paid usage item on
/// an entry of its book, priced from 2020 with an open tail.
struct Live {
    plan: Uuid,
    book: Uuid,
    rev1: Uuid,
    sku: Uuid,
    entry: Uuid,
}
async fn approved(f: &Fixture, entry: Uuid, from: &str) {
    let tenant = f.ctx.subject_tenant_id();
    let conn = f.db.conn().unwrap();
    let e = price_book_entry_repo::find(&conn, &scope(f), tenant, entry)
        .await
        .unwrap()
        .unwrap();
    let mut p = entry_support::price(&e);
    p.state = "approved".into();
    p.effective_from =
        Date::parse(from, &time::format_description::well_known::Iso8601::DATE).unwrap();
    price_repo::insert(&conn, &scope(f), p).await.unwrap();
}
async fn policy(f: &Fixture, quorum: u32) {
    let (_, _, tag) = f
        .call("GET", "/approval-policy", json!({}), None, None)
        .await;
    let (s, b, _) = f
        .call(
            "PUT",
            "/approval-policy",
            json!({"kind":"plan_revision","quorum":quorum}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
}
async fn submit(f: &Fixture, who: &SecurityContext, revision: Uuid, key: &str) -> (u16, Value) {
    let (s, b, _) = f
        .call_as(
            who,
            "POST",
            &format!("/plan-revisions/{revision}/submit"),
            json!({}),
            None,
            Some(key),
        )
        .await;
    (s, b)
}
async fn approve(f: &Fixture, who: &SecurityContext, unit: &Value, key: &str) -> Value {
    let (s, b, _) = f
        .call_as(
            who,
            "POST",
            &format!("/approval-units/{}/approve", unit.as_str().unwrap()),
            json!({"generation":1}),
            None,
            Some(key),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    b
}
async fn live(f: &Fixture, catalog: &Catalog, code: &str) -> Live {
    let eur = book(f, code).await;
    let (created, rev1) = plan(f, code, eur).await;
    let sku = catalog.sku(SkuType::Usage);
    let priced = entry(f, eur, sku, "usage", None).await;
    approved(f, priced, "2020-01-01").await;
    item(f, rev1, sku, Some(priced), "paid").await;
    policy(f, 0).await;
    let (s, receipt) = submit(f, &f.ctx, rev1, &format!("{code}-rev1")).await;
    assert_eq!(s, 201, "{receipt}");
    assert_eq!(receipt["revision"]["state"], "published", "{receipt}");
    Live {
        plan: id_of(&created["id"]),
        book: eur,
        rev1,
        sku,
        entry: priced,
    }
}
async fn get(f: &Fixture, path: &str) -> Value {
    let (s, b, _) = f.call("GET", path, json!({}), None, None).await;
    assert_eq!(s, 200, "{path}: {b}");
    b
}
/// The copy door's new draft.
async fn copy(f: &Fixture, plan: Uuid, key: &str) -> Uuid {
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/plans/{plan}/revisions"),
            json!({}),
            None,
            Some(key),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    id_of(&b["id"])
}
/// Set a draft's sale date through its PATCH (null for "at publish").
async fn sale_date(f: &Fixture, revision: Uuid, from: Option<Date>) {
    let path = format!("/plan-revisions/{revision}");
    let (_, _, tag) = f.call("GET", &path, json!({}), None, None).await;
    let (s, b, _) = f
        .call(
            "PATCH",
            &path,
            json!({"available_from": from.map(|d| d.to_string())}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
}
/// Rev 2, the copy of the published rev 1 with sale date `from`, submitted by the fixture's
/// author under quorum 1 and approved by `approver`: `(rev 2, its unit id)`.
async fn waiting(f: &Fixture, pro: &Live, from: Date, approver: &SecurityContext) -> (Uuid, Value) {
    let rev2 = copy(f, pro.plan, "copy-rev2").await;
    sale_date(f, rev2, Some(from)).await;
    policy(f, 1).await;
    let (s, receipt) = submit(f, &f.ctx, rev2, "submit-rev2").await;
    assert_eq!(s, 201, "{receipt}");
    assert_eq!(receipt["applied"], false, "{receipt}");
    let unit = receipt["unit"]["id"].clone();
    let vote = approve(f, approver, &unit, "approve-rev2").await;
    assert_eq!(vote["outcome"], "applied", "{vote}");
    (rev2, unit)
}
/// Rev 2 with sale date `from`, locked under a fixture unit and scheduled straight through the
/// repository: a date on or before today, which the apply publishes at once, can only be seeded.
/// `(rev 2, its unit)`.
async fn seeded(f: &Fixture, pro: &Live, from: Date, key: &str) -> (Uuid, Uuid) {
    let rev2 = copy(f, pro.plan, key).await;
    sale_date(f, rev2, Some(from)).await;
    let unit = plan_support::lock(f, rev2).await;
    plan_revision_repo::schedule(
        &f.db.conn().unwrap(),
        &scope(f),
        f.ctx.subject_tenant_id(),
        rev2,
        unit,
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    (rev2, unit)
}
/// A revision's stored state, read through the repository.
async fn stored(f: &Fixture, revision: Uuid) -> String {
    plan_revision_repo::find(
        &f.db.conn().unwrap(),
        &scope(f),
        f.ctx.subject_tenant_id(),
        revision,
    )
    .await
    .unwrap()
    .unwrap()
    .state
}
/// A plan's stored `(published_rev, version)`.
async fn stored_plan(f: &Fixture, plan: Uuid) -> (Option<i32>, i64) {
    let p = plan_repo::find(
        &f.db.conn().unwrap(),
        &scope(f),
        f.ctx.subject_tenant_id(),
        plan,
    )
    .await
    .unwrap()
    .unwrap();
    (p.published_rev, p.version)
}
/// The audit rows about one subject: `(action, actor)`, in order.
async fn audited(f: &Fixture, subject: Uuid) -> Vec<(String, Uuid)> {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    Database::connect(&f.dsn)
        .await
        .unwrap()
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT action, actor_ref FROM pricing_audit WHERE subject_id = ? \
             ORDER BY written_at, action",
            [subject.into()],
        ))
        .await
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.try_get::<String>("", "action").unwrap(),
                r.try_get::<Uuid>("", "actor_ref").unwrap(),
            )
        })
        .collect()
}
fn actions(rows: &[(String, Uuid)]) -> Vec<&str> {
    rows.iter().map(|(a, _)| a.as_str()).collect()
}
/// The `PlanRevisionPublished` events naming `revision`.
async fn published_of(f: &Fixture, revision: Uuid) -> Vec<Value> {
    outbox_events(&f.dsn, PUBLISHED)
        .await
        .into_iter()
        .filter(|e| e["data"]["revisionId"] == revision.to_string())
        .collect()
}
fn states(plan: &Value) -> Vec<String> {
    plan["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["state"].as_str().unwrap().to_owned())
        .collect()
}
/// The `ETag` a read answers.
async fn etag(f: &Fixture, path: &str) -> String {
    f.call("GET", path, json!({}), None, None).await.2
}
async fn unschedule(
    f: &Fixture,
    who: &SecurityContext,
    revision: Uuid,
    key: Option<&str>,
) -> (u16, Value, String) {
    f.call_as(
        who,
        "POST",
        &format!("/plan-revisions/{revision}/unschedule"),
        json!({}),
        None,
        key,
    )
    .await
}
async fn resolve(f: &Fixture, revision: Uuid, date: Date) -> (u16, Value) {
    let (s, b, _) = f
        .call(
            "GET",
            &format!("/resolve?plan_revision_id={revision}&date={date}"),
            json!({}),
            None,
            None,
        )
        .await;
    (s, b)
}

// ------------------------------------------------------------------ the apply chooses

/// D-449: an approval before the sale date stores the revision `scheduled`. Its lock turns into
/// `approved_by_unit_id`, `published_at` stays null, the published rev 1 and the plan's
/// `published_rev` do not move, the unit is applied, and no `PlanRevisionPublished` is enqueued
/// (the unit's `ApprovalUnitDecided` is).
#[tokio::test]
async fn an_approval_before_the_sale_date_schedules_the_revision_and_publishes_nothing() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let reviewer = f.user();
    let (rev2, unit) = waiting(&f, &pro, days(2), &reviewer).await;
    let r2 = get(&f, &format!("/plan-revisions/{rev2}")).await;
    assert_eq!(r2["state"], "scheduled", "{r2}");
    assert_eq!(r2["approved_by_unit_id"], unit);
    assert_eq!(r2["pending_unit_id"], json!(null));
    assert_eq!(r2["published_at"], json!(null));
    assert_eq!(r2["available_from"], days(2).to_string());
    assert_eq!(stored(&f, rev2).await, "scheduled");
    assert_eq!(stored(&f, pro.rev1).await, "published");
    let p = get(&f, &format!("/plans/{}", pro.plan)).await;
    assert_eq!(states(&p), ["published", "scheduled"], "{p}");
    assert_eq!(p["published_rev"], 1);
    let card = get(&f, &format!("/approval-units/{}", unit.as_str().unwrap())).await;
    assert_eq!(card["state"], "approved", "the unit is applied: {card}");
    assert!(published_of(&f, rev2).await.is_empty(), "nothing published");
    assert_eq!(outbox_events(&f.dsn, PUBLISHED).await.len(), 1, "rev 1's");
    assert_eq!(
        outbox_events(&f.dsn, DECIDED).await.len(),
        2,
        "rev 1's unit and rev 2's"
    );
    // Quorum 0 schedules at the submit itself, and says so in the receipt.
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "zero").await;
    let rev2 = copy(&f, pro.plan, "copy").await;
    sale_date(&f, rev2, Some(days(3))).await;
    let (s, receipt) = submit(&f, &f.ctx, rev2, "submit").await;
    assert_eq!(s, 201, "{receipt}");
    assert_eq!(receipt["applied"], true);
    assert_eq!(receipt["unit"]["state"], "approved");
    assert_eq!(receipt["revision"]["state"], "scheduled", "{receipt}");
    assert_eq!(receipt["revision"]["published_at"], json!(null));
    assert!(published_of(&f, rev2).await.is_empty());
    assert_eq!(
        get(&f, &format!("/plans/{}", pro.plan)).await["published_rev"],
        1
    );
}

/// D-449: a sale date of today publishes at once, as a null one does (the existing tests pin the
/// null date unchanged).
#[tokio::test]
async fn a_sale_date_of_today_publishes_at_once() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let rev2 = copy(&f, pro.plan, "copy").await;
    sale_date(&f, rev2, Some(today())).await;
    let (s, receipt) = submit(&f, &f.ctx, rev2, "submit").await;
    assert_eq!(s, 201, "{receipt}");
    assert_eq!(receipt["revision"]["state"], "published", "{receipt}");
    assert_eq!(stored(&f, pro.rev1).await, "superseded");
    assert_eq!(
        get(&f, &format!("/plans/{}", pro.plan)).await["published_rev"],
        2
    );
    let events = published_of(&f, rev2).await;
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0]["data"]["supersededRevisionId"],
        pro.rev1.to_string()
    );
}

// ------------------------------------------------------------------ one at a time

/// D-451: while a revision waits for its date, the copy door opens no new draft: 409
/// `REVISION_SCHEDULED` (withdraw it or wait for its date).
#[tokio::test]
async fn a_new_draft_is_refused_while_a_revision_waits() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let (rev2, _) = waiting(&f, &pro, days(2), &f.user()).await;
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/plans/{}/revisions", pro.plan),
            json!({}),
            None,
            Some("again"),
        )
        .await;
    assert_eq!(s, 409, "{b}");
    assert!(text(&b).contains("REVISION_SCHEDULED"), "{b}");
    assert_eq!(stored(&f, rev2).await, "scheduled");
    let p = get(&f, &format!("/plans/{}", pro.plan)).await;
    assert_eq!(
        states(&p),
        ["published", "scheduled"],
        "no revision was added"
    );
}

/// A door called: method, path, body, If-Match, Idempotency-Key, and the code that refuses it.
type Door<'a> = (
    &'a str,
    String,
    Value,
    Option<String>,
    Option<&'a str>,
    &'a str,
);
/// D-451: no door opens or moves a revision beside a scheduled one. Every door that opens a
/// revision or moves one (the copy, the draft's PATCH and delete, the submit, the item writes,
/// the votes on the applied unit) is refused on the plan, or works on another plan (the plan
/// create, the clone); the plan keeps exactly its published and its scheduled revision. Only the
/// unschedule door moves the waiting revision, and it leaves no scheduled one behind.
#[tokio::test]
async fn no_door_opens_or_moves_a_revision_beside_a_scheduled_one() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let (rev2, unit) = waiting(&f, &pro, days(2), &f.user()).await;
    let waiting_item = items(&f, rev2).await[0].id;
    // The item create is refused before it reads the entry it names.
    let waiting_entry = Uuid::new_v4();
    let unit_id = unit.as_str().unwrap().to_owned();
    let refused: Vec<Door> = vec![
        (
            "POST",
            format!("/plans/{}/revisions", pro.plan),
            json!({}),
            None,
            Some("copy"),
            "REVISION_SCHEDULED",
        ),
        (
            "PATCH",
            format!("/plan-revisions/{rev2}"),
            json!({"available_from":null}),
            Some(etag(&f, &format!("/plan-revisions/{rev2}")).await),
            None,
            "REVISION_NOT_DRAFT",
        ),
        (
            "PATCH",
            format!("/plan-revisions/{}", pro.rev1),
            json!({"available_from":null}),
            Some(etag(&f, &format!("/plan-revisions/{}", pro.rev1)).await),
            None,
            "REVISION_NOT_DRAFT",
        ),
        (
            "DELETE",
            format!("/plan-revisions/{rev2}"),
            json!({}),
            None,
            None,
            "REVISION_NOT_DRAFT",
        ),
        (
            "POST",
            format!("/plan-revisions/{rev2}/submit"),
            json!({}),
            None,
            Some("resubmit"),
            "REVISION_NOT_DRAFT",
        ),
        (
            "POST",
            format!("/plan-revisions/{}/submit", pro.rev1),
            json!({}),
            None,
            Some("resubmit-rev1"),
            "REVISION_NOT_DRAFT",
        ),
        (
            "POST",
            format!("/plan-revisions/{rev2}/items"),
            json!({"sku_id":catalog.sku(SkuType::Usage),"price_book_entry_id":waiting_entry}),
            None,
            Some("item"),
            "REVISION_NOT_DRAFT",
        ),
        (
            "PATCH",
            format!("/plan-items/{waiting_item}"),
            json!({}),
            Some(etag(&f, &format!("/plan-items/{waiting_item}")).await),
            None,
            "REVISION_NOT_DRAFT",
        ),
        (
            "DELETE",
            format!("/plan-items/{waiting_item}"),
            json!({}),
            None,
            None,
            "REVISION_NOT_DRAFT",
        ),
        (
            "POST",
            format!("/approval-units/{unit_id}/approve"),
            json!({"generation":1}),
            None,
            Some("approve-again"),
            "UNIT_ALREADY_DECIDED",
        ),
        (
            "POST",
            format!("/approval-units/{unit_id}/reject"),
            json!({"generation":1,"note":"no"}),
            None,
            Some("reject"),
            "UNIT_ALREADY_DECIDED",
        ),
        (
            "POST",
            format!("/approval-units/{unit_id}/withdraw"),
            json!({}),
            None,
            Some("withdraw"),
            "UNIT_ALREADY_DECIDED",
        ),
    ];
    for (method, path, body, if_match, key, code) in refused {
        let (s, b, _) = f.call(method, &path, body, if_match.as_deref(), key).await;
        assert_eq!(s, 409, "{method} {path}: {b}");
        assert!(text(&b).contains(code), "{method} {path}: {b}");
    }
    // The doors that open a revision of another plan work, and leave this plan alone.
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/plans/{}/clone", pro.plan),
            json!({"code":"COPY","name":"Copy"}),
            None,
            Some("clone"),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(states(&b), ["draft"]);
    let (s, b, _) = f
        .call(
            "POST",
            "/plans",
            json!({"code":"NEW","name":"New","book_id":pro.book}),
            None,
            Some("new"),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    let p = get(&f, &format!("/plans/{}", pro.plan)).await;
    assert_eq!(states(&p), ["published", "scheduled"], "{p}");
    // The unschedule door turns the waiting revision into the one draft: none is left scheduled.
    let (s, b, _) = unschedule(&f, &f.ctx, rev2, Some("unschedule")).await;
    assert_eq!(s, 200, "{b}");
    let p = get(&f, &format!("/plans/{}", pro.plan)).await;
    assert_eq!(states(&p), ["published", "draft"], "{p}");
}

// ------------------------------------------------------------------ unschedule

/// D-452: `POST /plan-revisions/{id}/unschedule` under `plan:submit` returns a waiting revision to
/// an unlocked draft of its author: `approved_by_unit_id` cleared, the version moved, the items
/// and their references kept; the key replays the answer; the audit row is
/// `plan_revision.unschedule`; no event; the applied unit stays applied. The draft is then edited
/// and resubmitted as any draft is: without a date it publishes at once.
#[tokio::test]
async fn unschedule_returns_a_waiting_revision_to_a_draft_that_can_be_resubmitted() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let (rev2, unit) = waiting(&f, &pro, days(2), &f.user()).await;
    let before = get(&f, &format!("/plan-revisions/{rev2}")).await;
    let submitter = holding(&f, "plan:submit");
    let (s, b, _) = unschedule(&f, &submitter, rev2, None).await;
    assert_eq!(s, 400, "an Idempotency-Key is required: {b}");
    let (s, b, _) = f
        .call_as(
            &submitter,
            "POST",
            &format!("/plan-revisions/{rev2}/unschedule"),
            json!({"note":"x"}),
            None,
            Some("body"),
        )
        .await;
    assert_eq!(s, 400, "the door takes no body: {b}");
    let (s, draft, tag) = unschedule(&f, &submitter, rev2, Some("unschedule")).await;
    assert_eq!(s, 200, "{draft}");
    assert_eq!(draft["id"], rev2.to_string());
    assert_eq!(draft["state"], "draft");
    assert_eq!(draft["approved_by_unit_id"], json!(null));
    assert_eq!(draft["pending_unit_id"], json!(null));
    assert_eq!(draft["published_at"], json!(null));
    assert_eq!(draft["available_from"], days(2).to_string());
    assert_eq!(draft["created_by"], f.ctx.subject_id().to_string());
    assert_eq!(draft["version"], before["version"].as_i64().unwrap() + 1);
    assert_eq!(
        tag,
        format!("\"{}\"", draft["version"]),
        "the PATCH's If-Match"
    );
    assert_eq!(draft["items"], before["items"], "the items stay");
    assert_eq!(
        items(&f, rev2).await[0].reference_state,
        "confirmed",
        "the reservation is kept"
    );
    let (s, replay, _) = unschedule(&f, &submitter, rev2, Some("unschedule")).await;
    assert_eq!((s, &replay), (200, &draft), "the key replays its answer");
    let rows = audited(&f, rev2).await;
    assert_eq!(
        rows.iter()
            .filter(|(a, who)| a == "plan_revision.unschedule" && *who == submitter.subject_id())
            .count(),
        1,
        "{rows:?}"
    );
    assert!(published_of(&f, rev2).await.is_empty(), "no event");
    let card = get(&f, &format!("/approval-units/{}", unit.as_str().unwrap())).await;
    assert_eq!(card["state"], "approved", "the unit stays applied");
    let p = get(&f, &format!("/plans/{}", pro.plan)).await;
    assert_eq!(states(&p), ["published", "draft"]);
    assert_eq!(p["published_rev"], 1);
    // The draft is the plan's one open revision, edited by its author and resubmitted.
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/plans/{}/revisions", pro.plan),
            json!({}),
            None,
            Some("copy-again"),
        )
        .await;
    assert_eq!(s, 409, "{b}");
    assert!(text(&b).contains("REVISION_DRAFT_EXISTS"), "{b}");
    sale_date(&f, rev2, None).await;
    policy(&f, 0).await;
    let (s, receipt) = submit(&f, &f.ctx, rev2, "resubmit").await;
    assert_eq!(s, 201, "{receipt}");
    assert_eq!(receipt["revision"]["state"], "published", "{receipt}");
    assert_ne!(receipt["unit"]["id"], unit, "a new unit");
    assert_eq!(stored(&f, pro.rev1).await, "superseded");
    assert_eq!(
        get(&f, &format!("/plans/{}", pro.plan)).await["published_rev"],
        2
    );
    assert_eq!(published_of(&f, rev2).await.len(), 1);
}

/// D-452: authorization first (`plan:submit`, not `plan:author`), then the claim, then the
/// catch-up, then the state: a published revision — stored so, or due and caught up — is 409
/// `REVISION_IN_EFFECT`; a draft, a pending or a superseded one is 409 `REVISION_NOT_SCHEDULED`;
/// an unknown one is 404.
#[tokio::test]
async fn unschedule_refuses_every_revision_that_is_not_waiting() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let (rev2, _) = waiting(&f, &pro, days(2), &f.user()).await;
    let (s, b, _) = unschedule(&f, &holding(&f, "plan:author"), rev2, Some("author")).await;
    assert_eq!(s, 403, "plan:submit is the grant: {b}");
    assert_eq!(stored(&f, rev2).await, "scheduled");
    let (s, b, _) = unschedule(&f, &f.ctx, Uuid::new_v4(), Some("unknown")).await;
    assert_eq!(s, 404, "{b}");
    let (s, b, _) = unschedule(&f, &f.ctx, pro.rev1, Some("published")).await;
    assert_eq!(s, 409, "{b}");
    assert!(text(&b).contains("REVISION_IN_EFFECT"), "{b}");
    // A draft, and a pending revision.
    let other = live(&f, &catalog, "other").await;
    let draft = copy(&f, other.plan, "copy-other").await;
    let (s, b, _) = unschedule(&f, &f.ctx, draft, Some("draft")).await;
    assert_eq!(s, 409, "{b}");
    assert!(text(&b).contains("REVISION_NOT_SCHEDULED"), "{b}");
    policy(&f, 1).await;
    let (s, receipt) = submit(&f, &f.ctx, draft, "pending").await;
    assert_eq!((s, &receipt["applied"]), (201, &json!(false)), "{receipt}");
    let (s, b, _) = unschedule(&f, &f.ctx, draft, Some("pending")).await;
    assert_eq!(s, 409, "{b}");
    assert!(text(&b).contains("REVISION_NOT_SCHEDULED"), "{b}");
    // A due revision: the catch-up makes it published, so it is in effect; its superseded
    // predecessor is not scheduled.
    let due = live(&f, &catalog, "due").await;
    let (due2, _) = seeded(&f, &due, today(), "copy-due").await;
    let (s, b, _) = unschedule(&f, &f.ctx, due2, Some("due")).await;
    assert_eq!(s, 409, "{b}");
    assert!(text(&b).contains("REVISION_IN_EFFECT"), "{b}");
    let (s, b, _) = unschedule(&f, &f.ctx, due.rev1, Some("superseded")).await;
    assert_eq!(s, 409, "{b}");
    assert!(text(&b).contains("REVISION_NOT_SCHEDULED"), "{b}");
}

// ------------------------------------------------------------------ the job

/// D-450: the ticker's switch duty persists a due switch on its date: rev 2 published with
/// `published_at` at 00:00 UTC of its date, rev 1 superseded, `published_rev` 2 and the plan's
/// version unchanged; ONE `PlanRevisionPublished` naming the approver, the unit and rev 1; an
/// audit row `plan_revision.switch` under pricing's system actor. A second tick emits nothing.
#[tokio::test]
async fn the_job_publishes_a_due_revision_on_its_date_once_naming_its_approver() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let reviewer = f.user();
    let (rev2, unit) = waiting(&f, &pro, days(2), &reviewer).await;
    let (_, version) = stored_plan(&f, pro.plan).await;
    let mut ticker = Ticker::new(f.state.clone(), on(days(2)), 10, 100).switch_every(1);
    ticker.tick().await.unwrap();
    assert_eq!(stored(&f, rev2).await, "published");
    assert_eq!(stored(&f, pro.rev1).await, "superseded");
    assert_eq!(stored_plan(&f, pro.plan).await, (Some(2), version));
    let r2 = get(&f, &format!("/plan-revisions/{rev2}")).await;
    assert_eq!(r2["published_at"], midnight(days(2)), "{r2}");
    assert_eq!(r2["approved_by_unit_id"], unit);
    let events = published_of(&f, rev2).await;
    assert_eq!(events.len(), 1, "{events:?}");
    let data = &events[0]["data"];
    assert_eq!(data["planId"], pro.plan.to_string());
    assert_eq!(data["revNo"], 2);
    assert_eq!(data["bookId"], pro.book.to_string());
    assert_eq!(data["supersededRevisionId"], pro.rev1.to_string());
    assert_eq!(data["unitId"], unit);
    assert_eq!(data["actorRef"], reviewer.subject_id().to_string());
    let switched: Vec<_> = audited(&f, rev2)
        .await
        .into_iter()
        .filter(|(a, _)| a == "plan_revision.switch")
        .collect();
    assert_eq!(
        switched,
        vec![(
            "plan_revision.switch".to_owned(),
            bss_products_sdk::PRICING_SYSTEM_ACTOR
        )]
    );
    ticker.tick().await.unwrap();
    assert_eq!(
        published_of(&f, rev2).await.len(),
        1,
        "a second tick emits nothing"
    );
    assert_eq!(
        actions(&audited(&f, rev2).await)
            .iter()
            .filter(|a| **a == "plan_revision.switch")
            .count(),
        1
    );
}

/// D-450, plan rev 2 H1: at quorum 0 the unit applies at its submit and records no decision, so
/// the switch names the submitter.
#[tokio::test]
async fn the_job_names_the_submitter_when_no_one_voted() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let rev2 = copy(&f, pro.plan, "copy").await;
    sale_date(&f, rev2, Some(days(2))).await;
    let submitter = f.user();
    let (s, receipt) = submit(&f, &submitter, rev2, "submit").await;
    assert_eq!(s, 201, "{receipt}");
    assert_eq!(receipt["revision"]["state"], "scheduled", "{receipt}");
    Ticker::new(f.state.clone(), on(days(2)), 10, 100)
        .tick()
        .await
        .unwrap();
    let events = published_of(&f, rev2).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(
        events[0]["data"]["actorRef"],
        submitter.subject_id().to_string()
    );
}

/// D-450, plan rev 2 M4: the switch runs first in the tick with its own error handling, so a
/// reconciliation that fails (Products' registry is absent from the hub while confirmed references
/// wait) fails the tick but never skips the switch.
#[tokio::test]
async fn a_failing_reconcile_does_not_keep_the_switch_from_running() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let (rev2, _) = waiting(&f, &pro, days(2), &f.user()).await;
    assert_eq!(stored(&f, rev2).await, "scheduled", "rev 2 waits");
    let bare = Arc::new(
        bss_pricing::api::rest::authoring::AuthoringState::new(
            f.db.clone(),
            Arc::new(toolkit::ClientHub::default()),
        )
        .await
        .unwrap(),
    );
    let result = Ticker::new(bare, on(days(2)), 10, 1)
        .switch_every(1)
        .tick()
        .await;
    assert!(result.is_err(), "the reconciliation failed: {result:?}");
    assert_eq!(stored(&f, rev2).await, "published", "the switch ran first");
    assert_eq!(published_of(&f, rev2).await.len(), 1);
}

/// D-450, phase 8 review B1: the reference duties' scan fails on every tick (its table is gone),
/// and the switch duty keeps its cadence. The tick count moves before any duty that can return
/// early, so a revision that falls due after the first tick is switched on the 60th failing tick,
/// with the scan still failing.
#[tokio::test]
async fn a_failing_reference_scan_does_not_freeze_the_switch_duty() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let mut ticker = Ticker::new(f.state.clone(), on(today()), 10, 1000);
    ticker.tick().await.unwrap();
    let (rev2, _) = seeded(&f, &pro, today(), "copy-rev2").await;
    plan_support::raw(
        &f,
        "ALTER TABLE pricing_reference_op RENAME TO pricing_reference_op_gone",
    )
    .await;
    let mut switched = None;
    for tick in 1..=180 {
        let result = ticker.tick().await;
        assert!(result.is_err(), "tick {tick}: the scan fails: {result:?}");
        if switched.is_none() && stored(&f, rev2).await == "published" {
            switched = Some(tick);
        }
    }
    assert_eq!(
        switched,
        Some(60),
        "switched on the 60th failing tick after the first"
    );
    assert_eq!(published_of(&f, rev2).await.len(), 1);
}

/// D-450: the switch duty runs on the first tick, then once every 60 ticks.
#[tokio::test]
async fn the_switch_duty_runs_on_the_first_tick_and_then_every_sixty() {
    let (f, catalog) = setup().await;
    let first = live(&f, &catalog, "first").await;
    let (first2, _) = seeded(&f, &first, today(), "copy-first").await;
    let mut ticker = Ticker::new(f.state.clone(), on(today()), 10, 1000);
    ticker.tick().await.unwrap();
    assert_eq!(stored(&f, first2).await, "published", "the first tick");
    let second = live(&f, &catalog, "second").await;
    let (second2, _) = seeded(&f, &second, today(), "copy-second").await;
    for tick in 2..=60 {
        ticker.tick().await.unwrap();
        assert_eq!(stored(&f, second2).await, "scheduled", "tick {tick}");
    }
    ticker.tick().await.unwrap();
    assert_eq!(stored(&f, second2).await, "published", "tick 61");
}

// ------------------------------------------------------------------ the catch-ups

/// D-451: the copy door catches a due switch up first, in its own transaction, with the job's
/// event and audit row; the new draft then copies the revision now in effect. The job afterwards
/// finds nothing to switch and emits nothing.
#[tokio::test]
async fn the_copy_door_catches_a_due_switch_up_and_announces_it_once() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let (rev2, unit) = seeded(&f, &pro, days(-1), "copy-rev2").await;
    let submitted_by = get(&f, &format!("/approval-units/{unit}")).await["submitted_by"].clone();
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/plans/{}/revisions", pro.plan),
            json!({}),
            None,
            Some("copy-rev3"),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(b["rev_no"], 3);
    assert_eq!(
        b["available_from"],
        days(-1).to_string(),
        "the copy is of rev 2, now in effect: {b}"
    );
    assert_eq!(stored(&f, rev2).await, "published");
    assert_eq!(stored(&f, pro.rev1).await, "superseded");
    assert_eq!(stored_plan(&f, pro.plan).await.0, Some(2));
    let events = published_of(&f, rev2).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(
        events[0]["data"]["supersededRevisionId"],
        pro.rev1.to_string()
    );
    assert_eq!(events[0]["data"]["unitId"], unit.to_string());
    assert_eq!(
        events[0]["data"]["actorRef"], submitted_by,
        "no decision was recorded: the submitter"
    );
    assert!(
        actions(&audited(&f, rev2).await).contains(&"plan_revision.switch"),
        "the job's audit row"
    );
    Ticker::new(f.state.clone(), on(today()), 10, 100)
        .tick()
        .await
        .unwrap();
    assert_eq!(
        published_of(&f, rev2).await.len(),
        1,
        "the job finds nothing"
    );
}

/// D-451: the clone door catches the source up first and clones the revision in effect.
#[tokio::test]
async fn the_clone_door_catches_up_and_clones_the_revision_in_effect() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let (rev2, _) = seeded(&f, &pro, today(), "copy-rev2").await;
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/plans/{}/clone", pro.plan),
            json!({"code":"COPY","name":"Copy"}),
            None,
            Some("clone"),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(
        b["revisions"][0]["available_from"],
        today().to_string(),
        "the clone is of rev 2: {b}"
    );
    assert_eq!(stored(&f, rev2).await, "published");
    assert_eq!(published_of(&f, rev2).await.len(), 1);
}

// ------------------------------------------------------------------ the reads derive

/// D-447, D-453: before any job runs, every read shows a due switch: the plan read and list (the
/// revision headers' state and `published_at`, `published_rev`), the plan list by SKU, the
/// revision read, the item read, `/resolve` and the prices' live impact. No read writes.
#[tokio::test]
async fn every_read_shows_a_due_switch_before_it_is_persisted() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let r1_published_at =
        get(&f, &format!("/plan-revisions/{}", pro.rev1)).await["published_at"].clone();
    let (rev2, _) = seeded(&f, &pro, days(-1), "copy-rev2").await;
    let (_, plan_version) = stored_plan(&f, pro.plan).await;
    let header = |p: &Value, id: Uuid| {
        p["revisions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id.to_string())
            .cloned()
            .unwrap()
    };
    let (s, read, tag) = f
        .call(
            "GET",
            &format!("/plans/{}", pro.plan),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{read}");
    assert_eq!(
        tag,
        format!("\"{plan_version}\""),
        "the version does not move"
    );
    let listed = get(&f, "/plans").await["items"][0].clone();
    let by_sku = get(&f, &format!("/plans?sku_id={}", pro.sku)).await["items"][0].clone();
    for p in [&read, &listed, &by_sku] {
        assert_eq!(states(p), ["superseded", "published"], "{p}");
        assert_eq!(p["published_rev"], 2, "{p}");
        assert_eq!(header(p, rev2)["published_at"], midnight(days(-1)));
        assert_eq!(header(p, pro.rev1)["published_at"], r1_published_at);
    }
    let r2 = get(&f, &format!("/plan-revisions/{rev2}")).await;
    assert_eq!(
        (r2["state"].as_str(), r2["published_at"].clone()),
        (Some("published"), json!(midnight(days(-1)))),
        "{r2}"
    );
    assert_eq!(
        get(&f, &format!("/plan-revisions/{}", pro.rev1)).await["state"],
        "superseded"
    );
    let item2 = items(&f, rev2).await[0].id;
    let item1 = items(&f, pro.rev1).await[0].id;
    assert_eq!(
        get(&f, &format!("/plan-items/{item2}")).await["state"],
        "published"
    );
    assert_eq!(
        get(&f, &format!("/plan-items/{item1}")).await["state"],
        "superseded"
    );
    let (s, b) = resolve(&f, rev2, today()).await;
    assert_eq!((s, b["state"].as_str()), (200, Some("published")), "{b}");
    let (s, b) = resolve(&f, pro.rev1, today()).await;
    assert_eq!((s, b["state"].as_str()), (200, Some("superseded")), "{b}");
    // The prices' live impact: a draft price of the entry both revisions sell.
    let sold = price_book_entry_repo::find(
        &f.db.conn().unwrap(),
        &scope(&f),
        f.ctx.subject_tenant_id(),
        pro.entry,
    )
    .await
    .unwrap()
    .unwrap();
    let mut draft = entry_support::price(&sold);
    draft.version_no = 2;
    draft.effective_from = days(30);
    price_repo::insert(&f.db.conn().unwrap(), &scope(&f), draft)
        .await
        .unwrap();
    let preview = get(&f, &format!("/price-books/{}/publish-changes", pro.book)).await;
    let impact: Vec<(String, String)> = preview["impact"]["plans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["revision_id"].as_str().unwrap().to_owned(),
                p["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        impact,
        vec![
            (pro.rev1.to_string(), "superseded".to_owned()),
            (rev2.to_string(), "published".to_owned()),
        ],
        "{preview}"
    );
    // Nothing was written.
    assert_eq!(stored(&f, rev2).await, "scheduled");
    assert_eq!(stored(&f, pro.rev1).await, "published");
    assert_eq!(stored_plan(&f, pro.plan).await, (Some(1), plan_version));
}

/// D-447, D-453, phase 8 review B2: the checks judge a deprecated SKU against the revision in
/// effect today, as every read derives it. Rev 2 adds a SKU and drops rev 1's; both SKUs are
/// deprecated after the approval. Once rev 2 is due, rev 2 carries its own SKU and rev 1's SKU is
/// carried by no revision in effect, and both revisions' checks answer the same before and after
/// the job persists the switch.
#[tokio::test]
async fn the_checks_answer_a_due_revision_the_same_before_and_after_the_switch_is_persisted() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let rev2 = copy(&f, pro.plan, "copy-rev2").await;
    let added = catalog.sku(SkuType::Usage);
    plan_support::item_with_qty(&f, rev2, added, "10").await;
    let carried = items(&f, rev2)
        .await
        .into_iter()
        .find(|i| i.sku_id == pro.sku)
        .unwrap()
        .id;
    let (s, b, _) = f
        .call(
            "DELETE",
            &format!("/plan-items/{carried}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 204, "{b}");
    sale_date(&f, rev2, Some(today())).await;
    let unit = plan_support::lock(&f, rev2).await;
    plan_revision_repo::schedule(
        &f.db.conn().unwrap(),
        &scope(&f),
        f.ctx.subject_tenant_id(),
        rev2,
        unit,
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    catalog.age(added, Lifecycle::Deprecated);
    catalog.age(pro.sku, Lifecycle::Deprecated);
    let deprecated = |checks: &Value| {
        checks["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["code"] == "ITEM_SKU_DEPRECATED")
            .unwrap_or_else(|| panic!("no ITEM_SKU_DEPRECATED in {checks}"))["ok"]
            .clone()
    };
    let mut before = Vec::new();
    for revision in [rev2, pro.rev1] {
        before.push(get(&f, &format!("/plan-revisions/{revision}/checks")).await);
    }
    assert_eq!(
        deprecated(&before[0]),
        true,
        "rev 2, in effect, carries its own SKU: {}",
        before[0]
    );
    assert_eq!(
        deprecated(&before[1]),
        false,
        "no revision in effect carries rev 1's SKU: {}",
        before[1]
    );
    Ticker::new(f.state.clone(), on(today()), 10, 100)
        .tick()
        .await
        .unwrap();
    assert_eq!(stored(&f, rev2).await, "published", "persisted");
    for (revision, before) in [rev2, pro.rev1].into_iter().zip(before) {
        let after = get(&f, &format!("/plan-revisions/{revision}/checks")).await;
        assert_eq!(after, before, "{revision}");
    }
}

/// D-453: the plan list derives in memory over the revisions it already read: its statements on
/// pricing's tables are the same with due scheduled revisions among them as without. D-460 and
/// D-461 amend the count from two to four (the current revisions' items, the units the
/// revisions name).
#[tokio::test]
async fn the_plan_list_derives_in_its_four_statements() {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let catalog = Arc::new(Catalog::default());
    let f = Fixture::on(db, tenant, dsn, catalog.clone()).await;
    for code in ["a", "b", "c"] {
        let pro = live(&f, &catalog, code).await;
        seeded(&f, &pro, today(), &format!("copy-{code}")).await;
    }
    // A draft beside a published revision, so in_effect is not the current revision (D-480).
    let mixed = live(&f, &catalog, "mix").await;
    let draft = copy(&f, mixed.plan, "mix-draft").await;
    item(&f, draft, catalog.sku(SkuType::Usage), None, "included").await;
    recorder.clear();
    let listed = get(&f, "/plans").await;
    let statements: Vec<_> = recorder
        .events()
        .into_iter()
        .filter(|q| {
            q.table
                .as_deref()
                .is_some_and(|t| t.starts_with("pricing_"))
        })
        .map(|q| q.sql)
        .collect();
    assert_eq!(statements.len(), 5, "{statements:#?}");
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 4);
    for p in items {
        if p["current"]["state"] == "draft" {
            assert_eq!(
                p["in_effect"]["sku_ids"],
                json!([mixed.sku.to_string()]),
                "the published revision's SKU, not the draft's: {p}"
            );
            assert_eq!(p["current"]["sku_ids"].as_array().unwrap().len(), 2, "{p}");
        } else {
            assert_eq!(states(p), ["superseded", "published"], "{p}");
            assert_eq!(p["published_rev"], 2);
            assert_eq!(
                p["in_effect"]["sku_ids"].as_array().unwrap().len(),
                1,
                "{p}"
            );
        }
    }
}

// ------------------------------------------------------------------ /resolve

/// D-454: a waiting revision resolves from its date, with the resolved state `scheduled`; before
/// its date it is 409 `REVISION_NOT_YET_AVAILABLE`. The published rev 1 resolves on every date.
#[tokio::test]
async fn resolve_serves_a_waiting_revision_from_its_date() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let (rev2, _) = waiting(&f, &pro, days(2), &f.user()).await;
    for date in [days(2), days(3)] {
        let (s, b) = resolve(&f, rev2, date).await;
        assert_eq!(s, 200, "{date}: {b}");
        assert_eq!(b["state"], "scheduled", "{b}");
        assert_eq!(b["rev_no"], 2);
        assert!(!b["items"][0]["chains"][0]["binding"].is_null(), "{b}");
    }
    for date in [days(1), today()] {
        let (s, b) = resolve(&f, rev2, date).await;
        assert_eq!(s, 409, "{date}: {b}");
        assert!(text(&b).contains("REVISION_NOT_YET_AVAILABLE"), "{b}");
    }
    let (s, b) = resolve(&f, pro.rev1, days(5)).await;
    assert_eq!((s, b["state"].as_str()), (200, Some("published")), "{b}");
}

/// D-454, D-419: once due, stored or derived, a revision resolves like any published one, on
/// every date, and its predecessor like any superseded one: the answers are the same before and
/// after the job persists the switch.
#[tokio::test]
async fn resolve_answers_a_due_revision_the_same_before_and_after_the_switch_is_persisted() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let (rev2, _) = seeded(&f, &pro, today(), "copy-rev2").await;
    let asks = [
        (rev2, days(-3)),
        (rev2, today()),
        (pro.rev1, days(-3)),
        (pro.rev1, today()),
    ];
    let mut derived = Vec::new();
    for (revision, date) in asks {
        let (s, b) = resolve(&f, revision, date).await;
        assert_eq!(s, 200, "{date}: {b}");
        derived.push(b);
    }
    assert_eq!(derived[0]["state"], "published");
    assert_eq!(derived[2]["state"], "superseded");
    Ticker::new(f.state.clone(), on(today()), 10, 100)
        .tick()
        .await
        .unwrap();
    assert_eq!(stored(&f, rev2).await, "published", "persisted");
    for ((revision, date), before) in asks.into_iter().zip(derived) {
        let (s, b) = resolve(&f, revision, date).await;
        assert_eq!((s, &b), (200, &before), "{revision} on {date}");
    }
}

// ------------------------------------------------------------------ the counts

/// D-446, D-453: the counts read the stored state. A plan whose rev 1 is published on book A and
/// whose rev 2 waits on book B is in both books' `stats.plans`; book B cannot be deleted
/// (`BOOK_IN_PLAN`, on a book with no entry); the SKU's `usage.plans` counts the plan once.
#[tokio::test]
async fn a_scheduled_revision_on_another_book_holds_it_in_the_counts() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let other = book(&f, "other").await;
    let e_other = entry(&f, other, pro.sku, "usage", None).await;
    approved(&f, e_other, "2020-01-01").await;
    let rev2 = copy(&f, pro.plan, "copy").await;
    let path = format!("/plan-revisions/{rev2}");
    let (_, _, tag) = f.call("GET", &path, json!({}), None, None).await;
    let (s, b, _) = f
        .call(
            "PATCH",
            &path,
            json!({"book_id":other,"available_from":days(2).to_string()}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["items"][0]["price_book_entry_id"], e_other.to_string());
    let (s, receipt) = submit(&f, &f.ctx, rev2, "submit").await;
    assert_eq!(s, 201, "{receipt}");
    assert_eq!(receipt["revision"]["state"], "scheduled", "{receipt}");
    let stats = |b: Value| {
        (
            b["stats"]["plans"].clone(),
            b["stats"]["plans_superseded_only"].clone(),
        )
    };
    assert_eq!(
        stats(get(&f, &format!("/price-books/{other}")).await),
        (json!(1), json!(0))
    );
    assert_eq!(
        stats(get(&f, &format!("/price-books/{}", pro.book)).await),
        (json!(1), json!(0))
    );
    let usage = bss_pricing::infra::usage::sku_usage(
        &f.db.conn().unwrap(),
        &scope(&f),
        f.ctx.subject_tenant_id(),
        &[pro.sku],
    )
    .await
    .unwrap();
    assert_eq!(usage[0].plans, 1, "one plan, through both books");
    // A book with no entry that only a waiting revision is on cannot be deleted: BOOK_IN_PLAN.
    // D-467: such a revision holds only legacy items stored without an entry, which no submit
    // passes any more (ITEM_ENTRY_MISSING), so it is published and scheduled through the
    // repositories, as the deployed database's legacy rows were.
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let (p, rev1) = plan(&f, "bare", eur).await;
    plan_support::item_with_qty(&f, rev1, catalog.sku(SkuType::Usage), "10").await;
    plan_support::publish(&f, id_of(&p["id"]), rev1).await;
    let empty = book(&f, "empty").await;
    let rev2 = copy(&f, id_of(&p["id"]), "copy").await;
    let path = format!("/plan-revisions/{rev2}");
    let (_, _, tag) = f.call("GET", &path, json!({}), None, None).await;
    let (s, b, _) = f
        .call(
            "PATCH",
            &path,
            json!({"book_id":empty,"available_from":days(2).to_string()}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let unit = plan_support::lock(&f, rev2).await;
    plan_revision_repo::schedule(
        &f.db.conn().unwrap(),
        &scope(&f),
        f.ctx.subject_tenant_id(),
        rev2,
        unit,
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    assert_eq!(
        stats(get(&f, &format!("/price-books/{empty}")).await),
        (json!(1), json!(0))
    );
    let (s, b, _) = f
        .call(
            "DELETE",
            &format!("/price-books/{empty}"),
            json!({}),
            Some("\"1\""),
            None,
        )
        .await;
    assert_eq!(s, 409, "{b}");
    assert!(text(&b).contains("BOOK_IN_PLAN"), "{b}");
}

/// D-446, D-453: the counts read the stored state, so a due revision's stored-published
/// predecessor still holds its book until the switch is persisted, while every read already shows
/// it superseded. Rev 1 is on book A and rev 2, due today, on book B: book A counts the plan until
/// the job's tick, then only its history.
#[tokio::test]
async fn a_due_revisions_predecessor_holds_its_book_until_the_switch_is_persisted() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let other = book(&f, "other").await;
    let e_other = entry(&f, other, pro.sku, "usage", None).await;
    approved(&f, e_other, "2020-01-01").await;
    let rev2 = copy(&f, pro.plan, "copy").await;
    let path = format!("/plan-revisions/{rev2}");
    let (_, _, tag) = f.call("GET", &path, json!({}), None, None).await;
    let (s, b, _) = f
        .call(
            "PATCH",
            &path,
            json!({"book_id":other,"available_from":today().to_string()}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let unit = plan_support::lock(&f, rev2).await;
    plan_revision_repo::schedule(
        &f.db.conn().unwrap(),
        &scope(&f),
        f.ctx.subject_tenant_id(),
        rev2,
        unit,
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    let stats = |b: Value| {
        (
            b["stats"]["plans"].clone(),
            b["stats"]["plans_superseded_only"].clone(),
        )
    };
    let read = get(&f, &format!("/plans/{}", pro.plan)).await;
    assert_eq!(states(&read), ["superseded", "published"], "{read}");
    assert_eq!(
        stats(get(&f, &format!("/price-books/{}", pro.book)).await),
        (json!(1), json!(0)),
        "due, not yet persisted: the stored-published rev 1 still counts"
    );
    assert_eq!(
        stats(get(&f, &format!("/price-books/{other}")).await),
        (json!(1), json!(0))
    );
    Ticker::new(f.state.clone(), on(today()), 10, 100)
        .tick()
        .await
        .unwrap();
    assert_eq!(stored(&f, rev2).await, "published", "persisted");
    assert_eq!(
        stats(get(&f, &format!("/price-books/{}", pro.book)).await),
        (json!(0), json!(1)),
        "persisted: rev 1 is history"
    );
    assert_eq!(
        stats(get(&f, &format!("/price-books/{other}")).await),
        (json!(1), json!(0))
    );
}
