//! What the plans list and the revision reads name for the screens (phase 9 run 9.1): each plan's
//! current revision and the one in effect (D-460), a revision's who and when (D-461) and a pending
//! revision's vote progress (D-462), on the reads and on every write answer that carries the same
//! DTO, in a fixed number of statements.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
use bss_approval::Store;
use bss_pricing::infra::storage::{
    RepoError,
    entity::plan as plan_entity,
    repo::{
        approval_repo::PricingApprovalStore, plan_repo, plan_revision_repo, price_book_entry_repo,
        price_repo,
    },
};
use bss_products_sdk::models::SkuType;
use plan_support::{
    Catalog, Fixture, book, entry_support, holding, id_of, item, plan, policy_entry as entry,
    scope, setup,
};
use serde_json::{Value, json};
use std::sync::Arc;
use time::{Date, Duration, OffsetDateTime};
use toolkit_security::SecurityContext;
use uuid::Uuid;

// ------------------------------------------------------------------ fixture

fn today() -> Date {
    OffsetDateTime::now_utc().date()
}
fn days(n: i64) -> Date {
    today() + Duration::days(n)
}
async fn get(f: &Fixture, path: &str) -> Value {
    let (s, b, _) = f.call("GET", path, json!({}), None, None).await;
    assert_eq!(s, 200, "{path}: {b}");
    b
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
async fn approved(f: &Fixture, entry: Uuid) {
    let tenant = f.ctx.subject_tenant_id();
    let conn = f.db.conn().unwrap();
    let e = price_book_entry_repo::find(&conn, &scope(f), tenant, entry)
        .await
        .unwrap()
        .unwrap();
    let mut p = entry_support::price(&e);
    p.state = "approved".into();
    p.effective_from = Date::from_calendar_date(2020, time::Month::January, 1).unwrap();
    price_repo::insert(&conn, &scope(f), p).await.unwrap();
}
/// A plan whose draft rev 1 holds one confirmed paid usage item on a priced entry of its own
/// book: green, ready to submit.
struct Fresh {
    plan: Uuid,
    rev1: Uuid,
    sku: Uuid,
    created: Value,
}
async fn fresh(f: &Fixture, catalog: &Catalog, code: &str) -> Fresh {
    let eur = book(f, code).await;
    let (created, rev1) = plan(f, code, eur).await;
    let sku = catalog.sku(SkuType::Usage);
    let priced = entry(f, eur, sku, "usage", None).await;
    approved(f, priced).await;
    item(f, rev1, sku, Some(priced), "paid").await;
    Fresh {
        plan: id_of(&created["id"]),
        rev1,
        sku,
        created,
    }
}
async fn submit(f: &Fixture, revision: Uuid, key: &str) -> Value {
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/plan-revisions/{revision}/submit"),
            json!({}),
            None,
            Some(key),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    b
}
async fn vote(
    f: &Fixture,
    who: &SecurityContext,
    unit: &Value,
    action: &str,
    body: Value,
    key: &str,
) -> (u16, Value) {
    let (s, b, _) = f
        .call_as(
            who,
            "POST",
            &format!("/approval-units/{}/{action}", unit.as_str().unwrap()),
            body,
            None,
            Some(key),
        )
        .await;
    (s, b)
}
/// A plan whose rev 1 is published at once (quorum 0).
async fn live(f: &Fixture, catalog: &Catalog, code: &str) -> Fresh {
    let p = fresh(f, catalog, code).await;
    policy(f, 0).await;
    let receipt = submit(f, p.rev1, &format!("{code}-rev1")).await;
    assert_eq!(receipt["revision"]["state"], "published", "{receipt}");
    p
}
async fn copy(f: &Fixture, plan: Uuid, key: &str) -> Value {
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
    b
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
fn current(p: &Value) -> (Value, Value, Value) {
    (
        p["current"]["revision_id"].clone(),
        p["current"]["rev_no"].clone(),
        p["current"]["state"].clone(),
    )
}
fn in_effect(p: &Value) -> (Value, Value) {
    (
        p["in_effect"]["revision_id"].clone(),
        p["in_effect"]["rev_no"].clone(),
    )
}
fn revision_of(p: &Value, rev_no: i64) -> Value {
    p["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["rev_no"] == rev_no)
        .unwrap_or_else(|| panic!("no rev {rev_no} in {p}"))
        .clone()
}
async fn unit_of(f: &Fixture, unit: &Value) -> Value {
    get(f, &format!("/approval-units/{}", unit.as_str().unwrap())).await
}

// ------------------------------------------------------------------ #27 the current revision

/// D-460: `current` is the draft or pending revision, else the scheduled one, else the published
/// one in effect, chosen over the states the revisions read today (D-447); `in_effect` is the
/// published one in effect. A due scheduled revision whose switch is not persisted is both.
/// Both are null without revisions. The plan read agrees with its list row.
#[tokio::test]
async fn the_current_revision_and_the_one_in_effect_over_every_state_mix() {
    let (f, catalog) = setup().await;
    let published = live(&f, &catalog, "published").await;
    let beside_draft = live(&f, &catalog, "draft-beside").await;
    let beside_pending = live(&f, &catalog, "pending-beside").await;
    let beside_scheduled = live(&f, &catalog, "scheduled-beside").await;
    let due = live(&f, &catalog, "due").await;
    let draft = fresh(&f, &catalog, "draft").await;
    let draft_rev2 = id_of(&copy(&f, beside_draft.plan, "copy-draft").await["id"]);
    // A due revision: seeded scheduled with today's date through the repository (the apply
    // publishes such a date at once), so no job or door has persisted its switch.
    let due_rev2 = id_of(&copy(&f, due.plan, "copy-due").await["id"]);
    sale_date(&f, due_rev2, Some(today())).await;
    let due_unit = plan_support::lock(&f, due_rev2).await;
    plan_revision_repo::schedule(
        &f.db.conn().unwrap(),
        &scope(&f),
        f.ctx.subject_tenant_id(),
        due_rev2,
        due_unit,
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    policy(&f, 1).await;
    let pending = fresh(&f, &catalog, "pending").await;
    submit(&f, pending.rev1, "pending-rev1").await;
    let pending_rev2 = id_of(&copy(&f, beside_pending.plan, "copy-pending").await["id"]);
    submit(&f, pending_rev2, "pending-rev2").await;
    let scheduled_rev2 = id_of(&copy(&f, beside_scheduled.plan, "copy-scheduled").await["id"]);
    sale_date(&f, scheduled_rev2, Some(days(3))).await;
    let receipt = submit(&f, scheduled_rev2, "scheduled-rev2").await;
    let (s, b) = vote(
        &f,
        &f.user(),
        &receipt["unit"]["id"],
        "approve",
        json!({"generation":1}),
        "approve-scheduled",
    )
    .await;
    assert_eq!((s, b["outcome"].clone()), (200, json!("applied")), "{b}");
    let empty = plan_repo::insert(
        &f.db.conn().unwrap(),
        &scope(&f),
        plan_entity::Model {
            id: Uuid::now_v7(),
            tenant_id: f.ctx.subject_tenant_id(),
            code: "empty".into(),
            name: "Empty".into(),
            published_rev: None,
            version: 1,
            created_by: f.ctx.subject_id(),
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
            work_revision_id: None,
            work_state: None,
            scheduled_revision_id: None,
            scheduled_from: None,
            published_revision_id: None,
            current_book_id: None,
            current_currency: None,
            last_activity_at: OffsetDateTime::now_utc(),
        },
    )
    .await
    .unwrap()
    .id;

    let s = |id: Uuid| json!(id.to_string());
    let expected = [
        (
            "draft only",
            draft.plan,
            (s(draft.rev1), json!(1), json!("draft")),
            (json!(null), json!(null)),
        ),
        (
            "pending only",
            pending.plan,
            (s(pending.rev1), json!(1), json!("pending")),
            (json!(null), json!(null)),
        ),
        (
            "published only",
            published.plan,
            (s(published.rev1), json!(1), json!("published")),
            (s(published.rev1), json!(1)),
        ),
        (
            "a draft beside the published",
            beside_draft.plan,
            (s(draft_rev2), json!(2), json!("draft")),
            (s(beside_draft.rev1), json!(1)),
        ),
        (
            "a pending beside the published",
            beside_pending.plan,
            (s(pending_rev2), json!(2), json!("pending")),
            (s(beside_pending.rev1), json!(1)),
        ),
        (
            "a waiting scheduled beside the published",
            beside_scheduled.plan,
            (s(scheduled_rev2), json!(2), json!("scheduled")),
            (s(beside_scheduled.rev1), json!(1)),
        ),
        (
            "a due scheduled, its switch not persisted",
            due.plan,
            (s(due_rev2), json!(2), json!("published")),
            (s(due_rev2), json!(2)),
        ),
        (
            "no revision",
            empty,
            (json!(null), json!(null), json!(null)),
            (json!(null), json!(null)),
        ),
    ];
    let listed = get(&f, "/plans").await;
    let rows = listed["items"].as_array().unwrap();
    assert_eq!(rows.len(), expected.len(), "{listed}");
    for (name, plan_id, want_current, want_in_effect) in expected {
        let row = rows
            .iter()
            .find(|p| p["id"] == plan_id.to_string())
            .unwrap_or_else(|| panic!("{name}: not listed"));
        assert_eq!(current(row), want_current, "{name}: {row}");
        assert_eq!(in_effect(row), want_in_effect, "{name}: {row}");
        let read = get(&f, &format!("/plans/{plan_id}")).await;
        assert_eq!(read["current"], row["current"], "{name}: the read agrees");
        assert_eq!(
            read["in_effect"], row["in_effect"],
            "{name}: the read agrees"
        );
    }
    let stored = plan_revision_repo::find(
        &f.db.conn().unwrap(),
        &scope(&f),
        f.ctx.subject_tenant_id(),
        due_rev2,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stored.state, "scheduled", "the read derived, never wrote");
}

/// D-460: `item_count` and `sku_ids` count every item of the current revision, a legacy item
/// stored without an entry too (D-467), while `GET /plans?sku_id=` keeps a plan only for an item with an entry,
/// judged on the stored state (D-434): the two may differ. `created_by` is the current
/// revision's author, not the plan's.
#[tokio::test]
async fn the_current_revisions_skus_count_every_item_and_may_differ_from_the_sku_filter() {
    let (f, catalog) = setup().await;
    let p = fresh(&f, &catalog, "pro").await;
    let included = catalog.sku(SkuType::Usage);
    let legacy = plan_support::item_with_qty(&f, p.rev1, included, "5").await;
    let listed = get(&f, "/plans").await;
    let row = &listed["items"][0];
    assert_eq!(row["current"]["item_count"], 2, "{row}");
    let mut skus = vec![p.sku.to_string(), included.to_string()];
    skus.sort();
    assert_eq!(row["current"]["sku_ids"], json!(skus), "ascending: {row}");
    assert_eq!(row["current"]["created_by"], f.ctx.subject_id().to_string());
    let by_entry = get(&f, &format!("/plans?sku_id={}", p.sku)).await;
    assert_eq!(by_entry["items"].as_array().unwrap().len(), 1);
    assert_eq!(by_entry["items"][0]["current"], row["current"]);
    let by_included = get(&f, &format!("/plans?sku_id={included}")).await;
    assert!(
        by_included["items"].as_array().unwrap().is_empty(),
        "the SKU filter needs an entry, the current revision names every item: {by_included}"
    );
    // D-467: the legacy item is ITEM_ENTRY_MISSING, so its author removes it before the submit.
    let (s, b, _) = f
        .call(
            "DELETE",
            &format!("/plan-items/{}", legacy.id),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 204, "{b}");
    // The copy is authored by another principal: `current.created_by` follows the revision.
    policy(&f, 0).await;
    submit(&f, p.rev1, "rev1").await;
    let other = f.user();
    let (s, b, _) = f
        .call_as(
            &other,
            "POST",
            &format!("/plans/{}/revisions", p.plan),
            json!({}),
            None,
            Some("copy"),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    let read = get(&f, &format!("/plans/{}", p.plan)).await;
    assert_eq!(
        read["current"]["created_by"],
        other.subject_id().to_string()
    );
    assert_eq!(
        read["current"]["item_count"], 1,
        "the copy carries the priced item"
    );
    assert_eq!(read["created_by"], f.ctx.subject_id().to_string());
}

/// The statements on pricing's tables one plan list makes.
async fn list_statements(
    f: &Fixture,
    recorder: &toolkit_db::test_support::QueryRecorder,
    n: usize,
) -> Vec<String> {
    recorder.clear();
    let listed = get(f, "/plans").await;
    assert_eq!(listed["items"].as_array().unwrap().len(), n);
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
/// `n` plans on one book, every other one published (a unit approves it) and the rest pending
/// under a unit, each revision with one item.
async fn seeded_plans(f: &Fixture, catalog: &Catalog, eur: Uuid, from: usize, n: usize) {
    for i in from..from + n {
        let (created, rev1) = plan(f, &format!("p-{i:03}"), eur).await;
        item(f, rev1, catalog.sku(SkuType::Usage), None, "included").await;
        if i % 2 == 0 {
            plan_support::publish(f, id_of(&created["id"]), rev1).await;
            // A draft beside the published revision (D-480): in_effect is not the current one.
            let draft = copy(f, id_of(&created["id"]), &format!("beside-{i}")).await;
            item(
                f,
                id_of(&draft["id"]),
                catalog.sku(SkuType::Usage),
                None,
                "included",
            )
            .await;
        } else {
            plan_support::lock(f, rev1).await;
        }
    }
}

// Probed in run 9.1: a per-plan read of the items or the units is red here.
/// D-485 (amending D-434, D-460 and D-453): `GET /plans` makes five statements whatever the
/// number of plans: the page, the revisions, the current and in-effect items, the units and the
/// revisions' books (D-516: every header's book, still that one statement). The same statements
/// for 10 and for 100 plans.
#[tokio::test]
async fn the_plan_list_reads_in_four_statements_for_10_and_100_plans() {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let catalog = Arc::new(Catalog::default());
    let f = Fixture::on(db, tenant, dsn, catalog.clone()).await;
    let eur = book(&f, "eur").await;
    seeded_plans(&f, &catalog, eur, 0, 10).await;
    let ten = list_statements(&f, &recorder, 10).await;
    seeded_plans(&f, &catalog, eur, 10, 90).await;
    let hundred = list_statements(&f, &recorder, 100).await;
    for (i, sql) in hundred.iter().enumerate() {
        eprintln!("plan list statement {i}: {sql}");
    }
    assert_eq!(ten.len(), 5, "{ten:#?}");
    assert_eq!(ten, hundred, "the same statements, whatever the size");
    let listed = get(&f, "/plans").await;
    for p in listed["items"].as_array().unwrap() {
        let header = &p["revisions"][0];
        assert!(header["submitted_at"].is_string(), "{p}");
        if p["current"]["state"] == "draft" {
            assert_eq!(p["current"]["item_count"], 2, "{p}");
            let sold = &p["in_effect"]["sku_ids"];
            assert_eq!(sold.as_array().unwrap().len(), 1, "the published SKUs: {p}");
            assert_ne!(sold, &p["current"]["sku_ids"], "not the draft's SKUs: {p}");
        } else {
            assert_eq!(p["current"]["item_count"], 1, "{p}");
            assert!(p["in_effect"].is_null(), "{p}");
        }
    }
}

// ------------------------------------------------------------------ #33 who and when

fn instants(r: &Value) -> (Value, Value) {
    (r["submitted_at"].clone(), r["approved_at"].clone())
}

/// D-461: a header names its author and creation (its row's), when it was submitted (its
/// pending or approving unit's `submitted_at`) and when it was approved (the approving unit's
/// `decided_at`, which a scheduled revision needs: its `published_at` is null). A draft, also one
/// back from a reject, a withdraw or an unschedule, has neither. The revision read carries the
/// same two instants.
#[tokio::test]
async fn a_header_says_who_made_the_revision_and_when_it_was_submitted_and_approved() {
    let (f, catalog) = setup().await;
    let pro = live(&f, &catalog, "pro").await;
    let rev1_unit =
        get(&f, &format!("/plan-revisions/{}", pro.rev1)).await["approved_by_unit_id"].clone();
    policy(&f, 1).await;
    let reviewer = f.user();
    // Rev 2: submitted, rejected, back to a draft.
    let rev2 = id_of(&copy(&f, pro.plan, "copy-2").await["id"]);
    let receipt = submit(&f, rev2, "submit-2").await;
    let unit2 = receipt["unit"]["id"].clone();
    let read = get(&f, &format!("/plans/{}", pro.plan)).await;
    let pending_unit = unit_of(&f, &unit2).await;
    assert_eq!(
        instants(&revision_of(&read, 2)),
        (pending_unit["submitted_at"].clone(), json!(null)),
        "pending: {read}"
    );
    let (s, b) = vote(
        &f,
        &reviewer,
        &unit2,
        "reject",
        json!({"generation":1,"note":"no"}),
        "reject-2",
    )
    .await;
    assert_eq!(s, 200, "{b}");
    let back = get(&f, &format!("/plans/{}", pro.plan)).await;
    assert_eq!(
        instants(&revision_of(&back, 2)),
        (json!(null), json!(null)),
        "a draft back from a reject: {back}"
    );
    // Withdrawn by its submitter: a draft again.
    let receipt = submit(&f, rev2, "submit-2b").await;
    let (s, b) = vote(
        &f,
        &f.ctx,
        &receipt["unit"]["id"],
        "withdraw",
        json!({}),
        "withdraw-2",
    )
    .await;
    assert_eq!(s, 200, "{b}");
    let back = get(&f, &format!("/plans/{}", pro.plan)).await;
    assert_eq!(
        instants(&revision_of(&back, 2)),
        (json!(null), json!(null)),
        "a draft back from a withdraw: {back}"
    );
    // Scheduled: approved for a later date, its published_at null.
    sale_date(&f, rev2, Some(days(3))).await;
    let receipt = submit(&f, rev2, "submit-2c").await;
    let unit2 = receipt["unit"]["id"].clone();
    let (s, b) = vote(
        &f,
        &reviewer,
        &unit2,
        "approve",
        json!({"generation":1}),
        "approve-2",
    )
    .await;
    assert_eq!(s, 200, "{b}");
    let decided = unit_of(&f, &unit2).await;
    let read = get(&f, &format!("/plans/{}", pro.plan)).await;
    let scheduled = revision_of(&read, 2);
    assert_eq!(scheduled["state"], "scheduled");
    assert_eq!(scheduled["published_at"], json!(null));
    assert_eq!(
        instants(&scheduled),
        (
            decided["submitted_at"].clone(),
            decided["decided_at"].clone()
        ),
        "scheduled: {read}"
    );
    let rev1_decided = unit_of(&f, &rev1_unit).await;
    assert_eq!(
        instants(&revision_of(&read, 1)),
        (
            rev1_decided["submitted_at"].clone(),
            rev1_decided["decided_at"].clone()
        ),
        "published at quorum 0: approved at its submit: {read}"
    );
    let revision_read = get(&f, &format!("/plan-revisions/{rev2}")).await;
    assert_eq!(
        instants(&revision_read),
        instants(&scheduled),
        "the read agrees"
    );
    // Unscheduled: a draft again, its approving unit let go.
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/plan-revisions/{rev2}/unschedule"),
            json!({}),
            None,
            Some("unschedule-2"),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(instants(&b), (json!(null), json!(null)), "the answer: {b}");
    let back = get(&f, &format!("/plans/{}", pro.plan)).await;
    assert_eq!(
        instants(&revision_of(&back, 2)),
        (json!(null), json!(null)),
        "a draft back from an unschedule: {back}"
    );
    // Published at once, and its predecessor superseded: each keeps its own unit's instants.
    sale_date(&f, rev2, None).await;
    policy(&f, 0).await;
    let receipt = submit(&f, rev2, "submit-2d").await;
    let rev2_unit = unit_of(&f, &receipt["unit"]["id"]).await;
    let read = get(&f, &format!("/plans/{}", pro.plan)).await;
    let (first, second) = (revision_of(&read, 1), revision_of(&read, 2));
    assert_eq!(first["state"], "superseded");
    assert_eq!(
        instants(&first),
        (
            rev1_decided["submitted_at"].clone(),
            rev1_decided["decided_at"].clone()
        )
    );
    assert_eq!(second["state"], "published");
    assert_eq!(
        instants(&second),
        (
            rev2_unit["submitted_at"].clone(),
            rev2_unit["decided_at"].clone()
        )
    );
    for header in [&first, &second] {
        assert_eq!(header["created_by"], f.ctx.subject_id().to_string());
        assert!(header["created_at"].is_string(), "{header}");
    }
    let r2 = get(&f, &format!("/plan-revisions/{rev2}")).await;
    assert_eq!(second["created_at"], r2["created_at"], "the row's own");
}

// ------------------------------------------------------------------ #40 vote progress

/// A unit's store in the fixture tenant.
fn store(f: &Fixture) -> PricingApprovalStore {
    PricingApprovalStore {
        scope: scope(f),
        tenant_id: f.ctx.subject_tenant_id(),
    }
}
/// Refresh a unit to `generation` as the engine's stale refresh does, over its items as stored
/// (so their fingerprint still holds and the next vote is not refreshed again): every vote of an
/// earlier generation turns stale.
async fn refreshed(f: &Fixture, unit: Uuid, generation: i32) {
    let store = store(f);
    price_repo::transaction(&f.db.db(), move |tx| {
        let store = store.clone();
        Box::pin(async move {
            let failed = |e: bss_approval::ApprovalError| RepoError::Db(e.to_string());
            let stored = store.unit(tx, unit).await.map_err(failed)?.unwrap();
            let items = store.items(tx, unit).await.map_err(failed)?;
            let hash = bss_approval::hash::snapshot_hash(&items, stored.common_effective_date);
            store
                .refresh(tx, unit, &items, &stored.snapshot, &hash, generation)
                .await
                .map_err(failed)
        })
    })
    .await
    .unwrap();
}

/// D-462 (O-9a): a pending revision's read, and every write answer of its DTO, carry `approval`
/// with its unit and the counts only: the current generation's approve votes that are not stale
/// (the approval library's predicate, the one the vote door judges by) and the quorum. It moves
/// with every vote as the receipt's `have` does; a duplicate vote adds nothing; a vote made stale
/// by a refresh no longer counts. Anything not pending carries `null`. A plan reader without the
/// approval-unit grant reads it.
#[tokio::test]
async fn a_pending_revision_shows_its_vote_progress_and_nothing_else_does() {
    let (f, catalog) = setup().await;
    let p = fresh(&f, &catalog, "pro").await;
    let path = format!("/plan-revisions/{}", p.rev1);
    assert_eq!(get(&f, &path).await["approval"], json!(null), "a draft");
    policy(&f, 2).await;
    let receipt = submit(&f, p.rev1, "submit").await;
    let unit = receipt["unit"]["id"].clone();
    let progress =
        |approvals: u32| json!({"unit_id":unit,"approvals":approvals,"quorum_required":2});
    assert_eq!(receipt["revision"]["approval"], progress(0), "{receipt}");
    let reader = holding(&f, "plan:read");
    let (s, read, _) = f
        .call_as(&reader, "GET", &path, json!({}), None, None)
        .await;
    assert_eq!(s, 200, "{read}");
    assert_eq!(
        read["approval"],
        progress(0),
        "a plan reader sees the counts"
    );
    let (first, second) = (f.user(), f.user());
    let (s, b) = vote(&f, &first, &unit, "approve", json!({"generation":1}), "a1").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["have"], 1);
    assert_eq!(
        get(&f, &path).await["approval"],
        progress(1),
        "as the receipt's have"
    );
    let (s, b) = vote(&f, &first, &unit, "approve", json!({"generation":1}), "a2").await;
    assert_eq!(s, 409, "{b}");
    assert!(b.to_string().contains("DUPLICATE_VOTE"), "{b}");
    assert_eq!(
        get(&f, &path).await["approval"],
        progress(1),
        "a duplicate adds nothing"
    );
    // A refresh makes the first vote stale: it no longer counts, and its reviewer votes again.
    refreshed(&f, id_of(&unit), 2).await;
    assert_eq!(
        get(&f, &path).await["approval"],
        progress(0),
        "a stale vote"
    );
    let (s, b) = vote(&f, &first, &unit, "approve", json!({"generation":2}), "a3").await;
    assert_eq!((s, b["have"].clone()), (200, json!(1)), "{b}");
    assert_eq!(
        get(&f, &path).await["approval"],
        progress(1),
        "the new generation's vote"
    );
    let (s, b) = vote(&f, &second, &unit, "approve", json!({"generation":2}), "a4").await;
    assert_eq!((s, b["outcome"].clone()), (200, json!("applied")), "{b}");
    assert_eq!(get(&f, &path).await["approval"], json!(null), "published");
    // Quorum 1: a pending unit that one vote applies.
    let q1 = fresh(&f, &catalog, "q1").await;
    policy(&f, 1).await;
    let receipt = submit(&f, q1.rev1, "submit-q1").await;
    assert_eq!(
        receipt["revision"]["approval"],
        json!({"unit_id":receipt["unit"]["id"],"approvals":0,"quorum_required":1})
    );
    // Quorum 0: applied at the submit, nothing pends.
    let q0 = fresh(&f, &catalog, "q0").await;
    policy(&f, 0).await;
    let receipt = submit(&f, q0.rev1, "submit-q0").await;
    assert_eq!(receipt["revision"]["state"], "published");
    assert_eq!(receipt["revision"]["approval"], json!(null), "{receipt}");
}

/// The phase 9 review's R13, R45 and R52 (D-462 amended): a revision's vote progress is counted
/// from its unit's decisions alone (`bss_approval::counted_approvals`), so a pending revision's read
/// reads no unit item; the submit receipt reads its new unit's items and decisions once each and
/// builds both the progress and the unit from them.
#[tokio::test]
async fn the_progress_reads_the_decisions_alone_and_the_receipt_each_row_once() {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let catalog = Arc::new(Catalog::default());
    let f = Fixture::on(db, tenant, dsn, catalog.clone()).await;
    let selects = |table: &str| {
        recorder
            .events()
            .into_iter()
            .filter(|q| {
                q.table.as_deref() == Some(table)
                    && q.sql
                        .trim_start()
                        .to_ascii_uppercase()
                        .starts_with("SELECT")
            })
            .count()
    };
    let p = fresh(&f, &catalog, "pro").await;
    policy(&f, 2).await;
    recorder.clear();
    let receipt = submit(&f, p.rev1, "submit").await;
    assert_eq!(receipt["revision"]["approval"]["approvals"], 0, "{receipt}");
    assert_eq!(receipt["unit"]["caller_can_approve"], false, "{receipt}");
    assert_eq!(
        (
            selects("pricing_approval_unit_item"),
            selects("pricing_approval_decision")
        ),
        (1, 1),
        "the receipt reads the new unit's items and decisions once"
    );
    let unit = receipt["unit"]["id"].clone();
    let (s, b) = vote(
        &f,
        &f.user(),
        &unit,
        "approve",
        json!({"generation":1}),
        "a1",
    )
    .await;
    assert_eq!(s, 200, "{b}");
    recorder.clear();
    let read = get(&f, &format!("/plan-revisions/{}", p.rev1)).await;
    assert_eq!(read["approval"]["approvals"], 1, "{read}");
    assert_eq!(
        (
            selects("pricing_approval_unit_item"),
            selects("pricing_approval_decision")
        ),
        (0, 1),
        "the progress counts the decisions alone"
    );
}

// ------------------------------------------------------------------ M7 the write answers

/// D-460, D-461, D-462 (plan review M7): the new fields are filled on every answer of the two
/// DTOs, from the rows the write holds: the plan create and clone and the rename answer the
/// current revision and the one in effect; the submit receipt's revision says when it was
/// submitted and, applied at once, when it was approved; the copy, the revision PATCH and the
/// unschedule answer a draft, with neither instant and no progress.
#[tokio::test]
async fn every_write_answer_carries_the_new_fields() {
    let (f, catalog) = setup().await;
    let p = fresh(&f, &catalog, "pro").await;
    let created = &p.created;
    let rev1 = p.rev1.to_string();
    assert_eq!(
        created["current"],
        json!({"revision_id":rev1,"rev_no":1,"state":"draft","item_count":0,"sku_ids":[],
               "created_by":f.ctx.subject_id(),
               // D-519: a write answer names nobody; the reads do.
               "created_by_name":null,
               "book":{"id":created["revisions"][0]["book_id"],"code":"pro","name":"pro",
               "currency":"EUR","valid_from":null,"valid_until":null}}),
        "the create answers its empty draft: {created}"
    );
    assert_eq!(created["in_effect"], json!(null));
    let header = &created["revisions"][0];
    assert_eq!(header["created_by"], f.ctx.subject_id().to_string());
    assert!(header["created_at"].is_string(), "{header}");
    assert_eq!(instants(header), (json!(null), json!(null)));
    policy(&f, 0).await;
    let receipt = submit(&f, p.rev1, "submit").await;
    let unit = unit_of(&f, &receipt["unit"]["id"]).await;
    assert_eq!(
        instants(&receipt["revision"]),
        (unit["submitted_at"].clone(), unit["decided_at"].clone()),
        "applied at once: {receipt}"
    );
    assert_eq!(receipt["revision"]["approval"], json!(null));
    let (s, renamed, _) = {
        let path = format!("/plans/{}", p.plan);
        let (_, _, tag) = f.call("GET", &path, json!({}), None, None).await;
        f.call("PATCH", &path, json!({"name":"Pro 2"}), Some(&tag), None)
            .await
    };
    assert_eq!(s, 200, "{renamed}");
    assert_eq!(renamed["current"]["revision_id"], rev1);
    assert_eq!(renamed["current"]["state"], "published");
    assert_eq!(
        renamed["in_effect"],
        json!({"revision_id":rev1,"rev_no":1,"sku_ids":[p.sku]})
    );
    assert_eq!(
        instants(&renamed["revisions"][0]),
        (unit["submitted_at"].clone(), unit["decided_at"].clone())
    );
    let (s, cloned, _) = f
        .call(
            "POST",
            &format!("/plans/{}/clone", p.plan),
            json!({"code":"CLONE","name":"Clone"}),
            None,
            Some("clone"),
        )
        .await;
    assert_eq!(s, 201, "{cloned}");
    assert_eq!(cloned["current"]["state"], "draft");
    assert_eq!(
        cloned["current"]["item_count"], 1,
        "the clone's copied item"
    );
    assert_eq!(cloned["current"]["sku_ids"], json!([p.sku]));
    assert_eq!(cloned["in_effect"], json!(null));
    let copied = copy(&f, p.plan, "copy").await;
    assert_eq!(instants(&copied), (json!(null), json!(null)), "{copied}");
    assert_eq!(copied["approval"], json!(null));
    let rev2 = id_of(&copied["id"]);
    let path = format!("/plan-revisions/{rev2}");
    let (_, _, tag) = f.call("GET", &path, json!({}), None, None).await;
    let (s, patched, _) = f
        .call(
            "PATCH",
            &path,
            json!({"available_from": days(5).to_string()}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{patched}");
    assert_eq!(instants(&patched), (json!(null), json!(null)));
    assert_eq!(patched["approval"], json!(null));
    policy(&f, 1).await;
    let receipt = submit(&f, rev2, "submit-2").await;
    let unit2 = unit_of(&f, &receipt["unit"]["id"]).await;
    assert_eq!(
        instants(&receipt["revision"]),
        (unit2["submitted_at"].clone(), json!(null)),
        "pending: {receipt}"
    );
    assert_eq!(receipt["revision"]["approval"]["approvals"], 0);
}

/// D-515: `current.book` carries the book's id and validity, so the list needs no book index
/// for the link or the validity line. An open book answers null on both dates.
#[tokio::test]
async fn a_plan_rows_book_carries_its_id_and_validity() {
    let (f, _) = setup().await;
    let (s, dated, _) = f
        .call(
            "POST",
            "/price-books",
            json!({
                "code": "dated",
                "name": "Dated",
                "currency": "EUR",
                "valid_from": "2026-01-01",
                "valid_until": "2026-12-31",
            }),
            None,
            Some("book-dated"),
        )
        .await;
    assert_eq!(s, 201, "{dated}");
    let (created, _) = plan(&f, "dated", id_of(&dated["id"])).await;
    let current = &created["current"]["book"];
    assert_eq!(
        current,
        &json!({
            "id": dated["id"],
            "code": "dated",
            "name": "Dated",
            "currency": "EUR",
            "valid_from": "2026-01-01",
            "valid_until": "2026-12-31",
        }),
        "the create answers the book it wrote: {created}"
    );
    let listed = get(&f, "/plans").await;
    let row = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == created["id"])
        .unwrap();
    assert_eq!(row["current"]["book"], *current, "the list: {row}");
    let open = book(&f, "open").await;
    let (opened, _) = plan(&f, "open", open).await;
    assert_eq!(opened["current"]["book"]["id"], open.to_string());
    assert_eq!(opened["current"]["book"]["valid_from"], json!(null));
    assert_eq!(opened["current"]["book"]["valid_until"], json!(null));
}
