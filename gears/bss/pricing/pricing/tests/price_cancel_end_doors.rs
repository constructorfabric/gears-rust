//! Cancel a scheduled price and end a live one through the prices unit (D-520, D-521).
//!
//! Approved prices are written through the repository at fixed dates; the doors judge them on a
//! movable clock, so a test can let a scheduled price start between two acts.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod entry_support;

use bss_pricing::infra::clock::Clock;
use bss_pricing::infra::commercial_terms::wire;
use bss_pricing::infra::storage::repo::{acceptance_repo, price_book_entry_repo, price_repo};
use entry_support::{Fixture, Script, request, state_with_clock, test_db};
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;
use toolkit_db::secure::AccessScope;
use toolkit_security::SecurityContext;
use uuid::Uuid;

const PUBLISHED: &str = "gts.cf.core.events.event.v1~cf.bss.pricing.prices_published.v1~";

struct Movable(Mutex<OffsetDateTime>);
impl Clock for Movable {
    fn now(&self) -> OffsetDateTime {
        *self.0.lock().unwrap()
    }
}

struct World {
    f: Fixture,
    book: String,
    entry: Uuid,
    clock: Arc<Movable>,
}

impl World {
    async fn at(day: &str) -> Self {
        let (db, _, tenant, dsn) = test_db().await;
        let clock = Arc::new(Movable(Mutex::new(stamp(day))));
        let state = state_with_clock(db.clone(), Arc::new(Script::default()), clock.clone()).await;
        let app = entry_support::app_for(state.clone(), tenant);
        let denied = app.clone();
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant)
            .subject_type("user")
            .build()
            .unwrap();
        let f = Fixture {
            dsn,
            state,
            app,
            denied,
            ctx,
            db,
        };
        let (book, _) = f.book().await;
        let book = book["id"].as_str().unwrap().to_owned();
        let (status, entry, _) = f
            .call(
                "POST",
                &format!("/price-books/{book}/entries"),
                json!({
                    "usage_rating_policy": entry_support::policy_support::input(),
                    "sku_id": Uuid::new_v4(),
                    "model": "per_unit"
                }),
                None,
                Some("entry"),
            )
            .await;
        assert_eq!(status, 201, "{entry}");
        let entry = entry["id"].as_str().unwrap().parse().unwrap();
        let world = Self {
            f,
            book,
            entry,
            clock,
        };
        world.quorum(0).await;
        world
    }

    fn set_day(&self, day: &str) {
        *self.clock.0.lock().unwrap() = stamp(day);
    }

    async fn quorum(&self, quorum: u32) {
        let (_, _, tag) = self
            .f
            .call("GET", "/approval-policy", json!({}), None, None)
            .await;
        let saved = self
            .f
            .call(
                "PUT",
                "/approval-policy",
                json!({"quorum": quorum}),
                Some(&tag),
                None,
            )
            .await;
        assert_eq!(saved.0, 200, "{saved:?}");
    }

    /// An approved price of the default chain, written through the repository.
    async fn seed(&self, version_no: i32, from: &str) -> Uuid {
        self.write(version_no, from, |_| {}).await
    }

    async fn seed_bound(&self, version_no: i32, from: &str) -> Uuid {
        self.write(version_no, from, |p| p.keep_for_bound = true)
            .await
    }

    async fn seed_ended(&self, version_no: i32, from: &str, to: &str) -> Uuid {
        self.write(version_no, from, |p| {
            p.effective_to = Some(date(to));
            p.closed_explicitly = true;
        })
        .await
    }

    /// An approved price whose end the next start set, as normalisation stores it: not an
    /// explicit end.
    async fn seed_closed(&self, version_no: i32, from: &str, to: &str) -> Uuid {
        self.write(version_no, from, |p| p.effective_to = Some(date(to)))
            .await
    }

    /// An approved price as `seed` writes it, then `tweak`ed.
    async fn write(
        &self,
        version_no: i32,
        from: &str,
        tweak: impl FnOnce(&mut bss_pricing::infra::storage::entity::price::Model),
    ) -> Uuid {
        let tenant = self.f.ctx.subject_tenant_id();
        let scope = AccessScope::for_tenant(tenant);
        let conn = self.f.db.conn().unwrap();
        let entry = price_book_entry_repo::find(&conn, &scope, tenant, self.entry)
            .await
            .unwrap()
            .unwrap();
        let mut price = entry_support::price(&entry);
        price.version_no = version_no;
        price.state = "approved".into();
        price.price_json = json!({"rate": "1.00"});
        price.effective_from = date(from);
        tweak(&mut price);
        price_repo::insert(&conn, &scope, price).await.unwrap().id
    }

    /// A consumer's binding: an acceptance whose receipt binds `price` (D-520). `order` is the
    /// receipt's order id, any id. The receipt is the stored v1 fixture with the tenant, the
    /// identities and the bound price replaced; its digests are not recomputed, and the
    /// repository does not recompute them either.
    async fn bind(&self, price: Uuid, order: Uuid) {
        let tenant = self.f.ctx.subject_tenant_id();
        let mut receipt =
            wire::decode_acceptance(include_str!("commercial_receipts/acceptance-v1.json"))
                .unwrap();
        receipt.acceptance_id = Uuid::now_v7();
        receipt.query.tenant_axes.seller_tenant_id = tenant;
        receipt.query.order_id = order;
        receipt.bindings[0].price_book_entry_id = self.entry;
        receipt.bindings[0].price.price_book_entry_id = self.entry;
        receipt.bindings[0].price.price_id = price;
        let row = acceptance_repo::from_receipt(&receipt, self.f.ctx.subject_id()).unwrap();
        acceptance_repo::insert(
            &self.f.db.conn().unwrap(),
            &AccessScope::for_tenant(tenant),
            row,
        )
        .await
        .unwrap();
    }

    async fn call(&self, method: &str, path: &str, body: Value, key: Option<&str>) -> (u16, Value) {
        let (status, body, _) = self.f.call(method, path, body, None, key).await;
        (status, body)
    }

    /// `POST /prices/{id}/cancel` or `/end`, answered 201 with the draft row.
    async fn change(&self, target: Uuid, kind: &str, body: Value, key: &str) -> Value {
        let (status, row) = self
            .call("POST", &format!("/prices/{target}/{kind}"), body, Some(key))
            .await;
        assert_eq!(status, 201, "{row}");
        row
    }

    async fn submit(&self, id: &Value, key: &str) -> (u16, Value) {
        self.call(
            "POST",
            &format!("/prices/{}/submit", id.as_str().unwrap()),
            json!({}),
            Some(key),
        )
        .await
    }

    async fn publish(&self, ids: &[&Value], key: &str) -> (u16, Value) {
        self.call(
            "POST",
            &format!("/price-books/{}/publish-changes", self.book),
            json!({"price_ids": ids}),
            Some(key),
        )
        .await
    }

    async fn approve_as_reviewer(&self, receipt: &Value, key: &str) -> (u16, Value) {
        let reviewer = self.f.user();
        let (status, body, _) = request(
            &self.f.app,
            &reviewer,
            "POST",
            &format!(
                "/approval-units/{}/approve",
                receipt["unit"]["id"].as_str().unwrap()
            ),
            json!({"generation": 1}),
            None,
            Some(key),
        )
        .await;
        (status, body)
    }

    async fn prices(&self) -> Vec<Value> {
        let (status, body) = self
            .call(
                "GET",
                &format!("/price-book-entries/{}/prices", self.entry),
                json!({}),
                None,
            )
            .await;
        assert_eq!(status, 200, "{body}");
        body["items"].as_array().unwrap().clone()
    }

    /// The entry as the book's entry list reads it on `day`: its price in force and its next.
    async fn headline(&self, day: &str) -> Value {
        let (status, body) = self
            .call(
                "GET",
                &format!("/price-books/{}/entries?as_of={day}", self.book),
                json!({}),
                None,
            )
            .await;
        assert_eq!(status, 200, "{body}");
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"].as_str() == Some(&self.entry.to_string()))
            .unwrap()
            .clone()
    }

    /// The `PricesPublished` envelopes in the outbox, oldest first.
    async fn published(&self) -> Vec<Value> {
        Database::connect(&self.f.dsn)
            .await
            .unwrap()
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT CAST(payload AS TEXT) AS payload FROM bss_pricing_outbox_body ORDER BY id"
                    .to_owned(),
            ))
            .await
            .unwrap()
            .iter()
            .map(|r| {
                let text = r.try_get::<String>("", "payload").unwrap();
                let envelope: event_broker_sdk::producer::ProducerOutboxEnvelope =
                    serde_json::from_str(&text).unwrap();
                serde_json::to_value(envelope).unwrap()
            })
            .filter(|e| e["type"] == PUBLISHED)
            .collect()
    }

    /// The prices of the one `PricesPublished` event in the outbox.
    async fn announced(&self) -> Vec<Value> {
        let events = self.published().await;
        assert_eq!(events.len(), 1, "{events:?}");
        events[0]["data"]["prices"].as_array().unwrap().clone()
    }

    /// One price as `PricesPublished` lists it: its window and its state after the apply.
    fn listed(&self, id: &impl ToString, window: (&str, Option<&str>), state: &str) -> Value {
        json!({
            "priceId": id.to_string().trim_matches('"'),
            "priceBookEntryId": self.entry,
            "dimValue": null,
            "effectiveFrom": window.0,
            "effectiveTo": window.1,
            "eligibility": "all",
            "state": state,
        })
    }

    /// A draft `set` price of the default chain, through its door.
    async fn draft(&self, from: &str, key: &str) -> Value {
        let (status, drafted, _) = self
            .f
            .call(
                "POST",
                &format!("/price-book-entries/{}/prices", self.entry),
                json!({"price": {"rate": "2.00"}, "eligibility": "all", "effective_from": from}),
                None,
                Some(key),
            )
            .await;
        assert_eq!(status, 201, "{drafted}");
        drafted["items"][0]["id"].clone()
    }

    fn row<'a>(rows: &'a [Value], id: &impl ToString) -> &'a Value {
        let id = id.to_string();
        rows.iter()
            .find(|row| row["id"].as_str() == Some(id.trim_matches('"')))
            .unwrap_or_else(|| panic!("{id} is not among {rows:?}"))
    }
}

fn stamp(day: &str) -> OffsetDateTime {
    OffsetDateTime::parse(
        &format!("{day}T12:00:00Z"),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap()
}
fn date(day: &str) -> time::Date {
    time::Date::parse(day, &time::format_description::well_known::Iso8601::DATE).unwrap()
}
fn code(body: &Value) -> String {
    body.to_string()
}
#[track_caller]
fn refused(answer: &(u16, Value), status: u16, expected: &str) {
    assert_eq!(answer.0, status, "{}", answer.1);
    assert!(code(&answer.1).contains(expected), "{}", answer.1);
}

// ------------------------------------------------------------------ cancel (D-520)

/// Ask 19's case: A → B (scheduled) → C. Cancelling B re-opens A onto C's start; B reads
/// `cancelled` and is never the price in force or the next price.
#[tokio::test]
async fn cancelling_a_scheduled_price_reopens_the_predecessor_on_the_next_start() {
    let world = World::at("2026-02-15").await;
    let a = world.seed(1, "2026-01-01").await;
    let b = world.seed(2, "2026-03-01").await;
    let c = world.seed(3, "2026-05-01").await;
    let change = world.change(b, "cancel", json!({}), "cancel").await;
    assert_eq!(change["change_kind"], "cancel");
    assert_eq!(change["target_price_id"], b.to_string());
    assert_eq!(change["state"], "draft");
    assert_eq!(change["status"], "draft");
    assert_eq!(
        change["effective_from"], "2026-03-01",
        "the price's own start"
    );
    let again = world
        .call(
            "POST",
            &format!("/prices/{b}/cancel"),
            json!({}),
            Some("cancel"),
        )
        .await;
    assert_eq!(again, (201, change.clone()), "the key replays");
    let (status, receipt) = world.submit(&change["id"], "submit-cancel").await;
    assert_eq!(status, 201, "{receipt}");
    assert_eq!(receipt["applied"], true);
    let rows = world.prices().await;
    let cancelled = World::row(&rows, &b);
    assert_eq!(cancelled["state"], "cancelled");
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["cancelled_by_unit_id"], receipt["unit"]["id"]);
    assert_eq!(World::row(&rows, &a)["effective_to"], "2026-05-01");
    assert_eq!(World::row(&rows, &a)["closed_explicitly"], false);
    assert_eq!(World::row(&rows, &c)["effective_to"], Value::Null);
    let record = World::row(&rows, &change["id"]);
    assert_eq!(record["state"], "approved");
    assert_eq!(
        record["status"], "superseded",
        "an applied change is never a price in force"
    );
    for (day, current, next) in [
        ("2026-02-15", a, Some(c)),
        ("2026-03-15", a, Some(c)),
        ("2026-05-15", c, None),
    ] {
        let entry = world.headline(day).await;
        assert_eq!(entry["current_price"]["id"], current.to_string(), "{day}");
        assert_eq!(
            entry["next_price"]["id"],
            next.map_or(Value::Null, |id| json!(id.to_string())),
            "{day}"
        );
    }
    let (status, by_id, _) = world
        .f
        .call("GET", &format!("/prices/{b}"), json!({}), None, None)
        .await;
    assert_eq!(
        status, 200,
        "a cancelled price stays readable by id: {by_id}"
    );
    assert_eq!(
        by_id["status"], "cancelled",
        "the pinned read says so, with the list's token"
    );
    assert_eq!(by_id["price_id"], b.to_string());
    let (status, live, _) = world
        .f
        .call("GET", &format!("/prices/{a}"), json!({}), None, None)
        .await;
    assert_eq!(status, 200, "{live}");
    assert!(
        live.get("status").is_none(),
        "an approved price's display status depends on the day, which the pinned read never computes (D-422): {live}"
    );
    let (status, _, _) = world
        .f
        .call(
            "GET",
            &format!("/prices/{}", change["id"].as_str().unwrap()),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(status, 404, "a change row is not a price");
}

/// Without a later price the predecessor re-opens to open-ended: its end, stored at the cancelled
/// price's start as normalisation keeps it, goes back to null (review RF-P item 9).
#[tokio::test]
async fn cancelling_the_last_scheduled_price_leaves_the_predecessor_open() {
    let world = World::at("2026-02-15").await;
    let a = world.seed_closed(1, "2026-01-01", "2026-03-01").await;
    let b = world.seed(2, "2026-03-01").await;
    assert_eq!(
        World::row(&world.prices().await, &a)["effective_to"],
        "2026-03-01"
    );
    let change = world.change(b, "cancel", json!({}), "cancel").await;
    let (status, receipt) = world.submit(&change["id"], "submit").await;
    assert_eq!(status, 201, "{receipt}");
    let rows = world.prices().await;
    assert_eq!(World::row(&rows, &a)["effective_to"], Value::Null);
    assert_eq!(World::row(&rows, &b)["state"], "cancelled");
}

/// Every cancel guard answers its own code (decision 4), at the door and again at submit.
#[tokio::test]
async fn the_cancel_guards_answer_their_codes() {
    let world = World::at("2026-02-15").await;
    let live = world.seed(1, "2026-01-01").await;
    let scheduled = world.seed(2, "2026-03-01").await;
    // A consumer's binding names it: `keep_for_bound` alone would not refuse (D-520 amended).
    let bound = world.seed_bound(3, "2026-04-01").await;
    world.bind(bound, Uuid::new_v4()).await;
    let later = world.seed(4, "2026-06-01").await;
    let answer = world
        .call(
            "POST",
            &format!("/prices/{live}/cancel"),
            json!({}),
            Some("live"),
        )
        .await;
    refused(&answer, 409, "PRICE_NOT_SCHEDULED");
    let answer = world
        .call(
            "POST",
            &format!("/prices/{bound}/cancel"),
            json!({}),
            Some("bound"),
        )
        .await;
    refused(&answer, 409, "PRICE_BOUND");
    let answer = world
        .call(
            "POST",
            &format!("/prices/{}/cancel", Uuid::new_v4()),
            json!({}),
            Some("unknown"),
        )
        .await;
    assert_eq!(answer.0, 404, "{}", answer.1);
    let answer = world
        .call(
            "POST",
            &format!("/prices/{scheduled}/cancel"),
            json!({"effective_to": "2026-05-01"}),
            Some("body"),
        )
        .await;
    refused(&answer, 400, "BODY_UNEXPECTED");

    // A change already pending on the price.
    world.quorum(1).await;
    let first = world.change(scheduled, "cancel", json!({}), "first").await;
    let second = world.change(scheduled, "cancel", json!({}), "second").await;
    let (status, receipt) = world.submit(&first["id"], "submit-first").await;
    assert_eq!(status, 201, "{receipt}");
    assert_eq!(receipt["applied"], false);
    let answer = world
        .call(
            "POST",
            &format!("/prices/{scheduled}/cancel"),
            json!({}),
            Some("third"),
        )
        .await;
    refused(&answer, 409, "PRICE_CHANGE_PENDING");
    refused(
        &world.submit(&second["id"], "submit-second").await,
        409,
        "PRICE_CHANGE_PENDING",
    );

    // A price that starts on the day of the submit has started (review RF-P item 9: the boundary),
    // and the door refuses it on that day too.
    let draft = world.change(later, "cancel", json!({}), "later").await;
    world.set_day("2026-06-01");
    refused(
        &world.submit(&draft["id"], "submit-late").await,
        409,
        "PRICE_NOT_SCHEDULED",
    );
    refused(
        &world
            .call(
                "POST",
                &format!("/prices/{later}/cancel"),
                json!({}),
                Some("on-its-start"),
            )
            .await,
        409,
        "PRICE_NOT_SCHEDULED",
    );
}

/// A cancel and an end name an approved price and nothing else (review RF-P item 9): a draft, a
/// price already cancelled, and an applied change row are each refused at the door, the cancel
/// `PRICE_NOT_SCHEDULED` and the end `PRICE_ALREADY_ENDED`.
#[tokio::test]
async fn a_change_names_an_approved_price_and_nothing_else() {
    let world = World::at("2026-02-15").await;
    world.seed(1, "2026-01-01").await;
    let later = world.seed(2, "2026-05-01").await;
    let draft = world.draft("2031-06-01", "draft").await;
    let change = world.change(later, "cancel", json!({}), "cancel").await;
    let (status, receipt) = world.submit(&change["id"], "submit").await;
    assert_eq!(status, 201, "{receipt}");
    assert_eq!(receipt["applied"], true);
    let rows = world.prices().await;
    assert_eq!(World::row(&rows, &later)["state"], "cancelled");
    assert_eq!(World::row(&rows, &change["id"])["state"], "approved");
    let ids = [
        ("draft", draft.as_str().unwrap().to_owned()),
        ("cancelled", later.to_string()),
        ("change row", change["id"].as_str().unwrap().to_owned()),
    ];
    for (what, target) in ids {
        let answer = world
            .call(
                "POST",
                &format!("/prices/{target}/cancel"),
                json!({}),
                Some(&format!("cancel-{what}")),
            )
            .await;
        assert_eq!(answer.0, 409, "cancel of a {what}: {}", answer.1);
        refused(&answer, 409, "PRICE_NOT_SCHEDULED");
        let answer = world
            .call(
                "POST",
                &format!("/prices/{target}/end"),
                json!({"effective_to": "2026-04-01"}),
                Some(&format!("end-{what}")),
            )
            .await;
        assert_eq!(answer.0, 409, "end of a {what}: {}", answer.1);
        refused(&answer, 409, "PRICE_ALREADY_ENDED");
    }
}

/// One unit names a price once: two changes of the same price in one publish are refused.
#[tokio::test]
async fn one_unit_names_a_price_once() {
    let world = World::at("2026-02-15").await;
    world.seed(1, "2026-01-01").await;
    let scheduled = world.seed(2, "2026-03-01").await;
    let cancel = world.change(scheduled, "cancel", json!({}), "cancel").await;
    let end = world
        .change(
            scheduled,
            "end",
            json!({"effective_to": "2026-04-01"}),
            "end",
        )
        .await;
    refused(
        &world.publish(&[&cancel["id"], &end["id"]], "both").await,
        409,
        "PRICE_CHANGE_PENDING",
    );
    let rows = world.prices().await;
    assert_eq!(World::row(&rows, &scheduled)["state"], "approved");
    assert_eq!(World::row(&rows, &cancel["id"])["state"], "draft");
}

/// The race: the price starts between submit and apply. The vote is refused with
/// `PRICE_ALREADY_STARTED`, and nothing changes, the unit's other price included.
#[tokio::test]
async fn a_price_that_starts_before_apply_is_refused_and_the_unit_changes_nothing() {
    let world = World::at("2026-02-15").await;
    world.quorum(1).await;
    let a = world.seed(1, "2026-01-01").await;
    let b = world.seed(2, "2026-03-01").await;
    let (status, drafted, _) = world
        .f
        .call(
            "POST",
            &format!("/price-book-entries/{}/prices", world.entry),
            json!({"price": {"rate": "2.00"}, "eligibility": "all", "effective_from": "2031-06-01"}),
            None,
            Some("set"),
        )
        .await;
    assert_eq!(status, 201, "{drafted}");
    let set = drafted["items"][0]["id"].clone();
    let cancel = world.change(b, "cancel", json!({}), "race").await;
    let (status, receipt) = world.publish(&[&set, &cancel["id"]], "publish").await;
    assert_eq!(status, 201, "{receipt}");
    assert_eq!(receipt["applied"], false);
    // The apply runs on B's own start: it has started (review RF-P item 9: the boundary).
    world.set_day("2026-03-01");
    let answer = world.approve_as_reviewer(&receipt, "approve-race").await;
    refused(&answer, 409, "PRICE_ALREADY_STARTED");
    assert!(
        !code(&answer.1).contains("APPLY_REFUSED"),
        "the race keeps its own code: {}",
        answer.1
    );
    let rows = world.prices().await;
    assert_eq!(World::row(&rows, &b)["state"], "approved");
    assert_eq!(World::row(&rows, &a)["effective_to"], Value::Null);
    assert_eq!(World::row(&rows, &set)["state"], "pending");
    assert_eq!(World::row(&rows, &cancel["id"])["state"], "pending");
    assert!(world.published().await.is_empty(), "no unit applied");
}

// ------------------------------------------------------------------ end (D-521)

/// Ending a live price closes it explicitly at the new end; from that day the entry has no price
/// in force.
#[tokio::test]
async fn ending_a_live_price_closes_it_and_clears_the_price_in_force() {
    let world = World::at("2026-02-15").await;
    let live = world.seed(1, "2026-01-01").await;
    let change = world
        .change(live, "end", json!({"effective_to": "2026-02-20"}), "end")
        .await;
    assert_eq!(change["change_kind"], "end");
    assert_eq!(change["target_price_id"], live.to_string());
    assert_eq!(change["effective_to"], "2026-02-20");
    let (status, receipt) = world.submit(&change["id"], "submit-end").await;
    assert_eq!(status, 201, "{receipt}");
    assert_eq!(receipt["applied"], true);
    let rows = world.prices().await;
    let ended = World::row(&rows, &live);
    assert_eq!(ended["closed_explicitly"], true);
    assert_eq!(ended["effective_to"], "2026-02-20");
    assert_eq!(ended["state"], "approved");
    assert_eq!(World::row(&rows, &change["id"])["status"], "superseded");
    assert_eq!(
        world.headline("2026-02-19").await["current_price"]["id"],
        live.to_string()
    );
    for day in ["2026-02-20", "2026-12-01"] {
        let entry = world.headline(day).await;
        assert_eq!(entry["current_price"], Value::Null, "{day}: {entry}");
    }
}

/// D-390 holds: a successor that starts inside the explicit end still ends it at its start, and
/// the end stays once the successor is cancelled.
#[tokio::test]
async fn an_end_before_the_next_start_is_kept_and_a_cancelled_successor_keeps_it() {
    let world = World::at("2026-02-15").await;
    let live = world.seed(1, "2026-01-01").await;
    let next = world.seed(2, "2026-03-01").await;
    let change = world
        .change(live, "end", json!({"effective_to": "2026-02-25"}), "end")
        .await;
    let (status, receipt) = world.submit(&change["id"], "submit-end").await;
    assert_eq!(status, 201, "{receipt}");
    let cancel = world.change(next, "cancel", json!({}), "cancel").await;
    let (status, receipt) = world.submit(&cancel["id"], "submit-cancel").await;
    assert_eq!(status, 201, "{receipt}");
    let rows = world.prices().await;
    assert_eq!(World::row(&rows, &live)["effective_to"], "2026-02-25");
    assert_eq!(World::row(&rows, &next)["state"], "cancelled");
    assert_eq!(
        world.headline("2026-03-15").await["current_price"],
        Value::Null
    );
}

/// Every end guard answers its own code (decision 4).
#[tokio::test]
async fn the_end_guards_answer_their_codes() {
    let world = World::at("2026-02-15").await;
    let live = world.seed(1, "2026-01-01").await;
    let scheduled = world.seed(2, "2026-03-01").await;
    let ended = world.seed_ended(3, "2025-01-01", "2026-02-01").await;
    // Its end is today: it has ended (review RF-P item 9: the boundary).
    let ends_today = world.seed_ended(4, "2025-06-01", "2026-02-15").await;
    let end = |target: Uuid, to: &'static str, key: &'static str| {
        let world = &world;
        async move {
            world
                .call(
                    "POST",
                    &format!("/prices/{target}/end"),
                    json!({"effective_to": to}),
                    Some(key),
                )
                .await
        }
    };
    refused(
        &end(live, "2026-04-15", "past-the-next").await,
        400,
        "END_DATE_INVALID",
    );
    refused(
        &end(live, "2026-02-15", "today").await,
        400,
        "END_DATE_INVALID",
    );
    refused(
        &end(live, "2026-02-01", "before-today").await,
        400,
        "END_DATE_INVALID",
    );
    refused(
        &end(scheduled, "2026-02-20", "before-start").await,
        400,
        "END_DATE_INVALID",
    );
    refused(
        &end(scheduled, "2026-03-01", "on-start").await,
        400,
        "END_DATE_INVALID",
    );
    refused(
        &end(live, "20260301", "malformed").await,
        400,
        "END_DATE_INVALID",
    );
    refused(
        &end(ended, "2026-03-01", "already-ended").await,
        409,
        "PRICE_ALREADY_ENDED",
    );
    refused(
        &end(ends_today, "2026-02-14", "ends-today").await,
        409,
        "PRICE_ALREADY_ENDED",
    );
    let answer = world
        .call(
            "POST",
            &format!("/prices/{live}/end"),
            json!({}),
            Some("no-body"),
        )
        .await;
    assert_eq!(answer.0, 400, "{}", answer.1);

    world.quorum(1).await;
    let first = world
        .change(live, "end", json!({"effective_to": "2026-02-25"}), "first")
        .await;
    let (status, receipt) = world.submit(&first["id"], "submit-first").await;
    assert_eq!(status, 201, "{receipt}");
    refused(
        &end(live, "2026-02-20", "second").await,
        409,
        "PRICE_CHANGE_PENDING",
    );
    refused(
        &world
            .call(
                "POST",
                &format!("/prices/{live}/cancel"),
                json!({}),
                Some("cancel"),
            )
            .await,
        409,
        "PRICE_NOT_SCHEDULED",
    );
}

/// An end the day passes before its apply is refused at apply.
#[tokio::test]
async fn an_end_that_is_no_longer_after_today_is_refused_at_apply() {
    let world = World::at("2026-02-15").await;
    world.quorum(1).await;
    let live = world.seed(1, "2026-01-01").await;
    let change = world
        .change(live, "end", json!({"effective_to": "2026-02-20"}), "end")
        .await;
    let (status, receipt) = world.submit(&change["id"], "submit").await;
    assert_eq!(status, 201, "{receipt}");
    world.set_day("2026-02-21");
    let answer = world.approve_as_reviewer(&receipt, "approve").await;
    refused(&answer, 409, "APPLY_REFUSED");
    assert!(code(&answer.1).contains("END_DATE_INVALID"), "{}", answer.1);
    let rows = world.prices().await;
    assert_eq!(World::row(&rows, &live)["effective_to"], Value::Null);
    assert_eq!(World::row(&rows, &live)["closed_explicitly"], false);
}

/// A stored `end` row with no new end is a corrupt row, not a refusal of the author's date (review
/// RF-P item 6): no door writes one, so the submit that reads it back answers 500, and nothing
/// changes.
#[tokio::test]
async fn a_stored_end_without_its_date_is_a_corrupt_row() {
    let world = World::at("2026-02-15").await;
    let live = world.seed(1, "2026-01-01").await;
    let change = world
        .change(live, "end", json!({"effective_to": "2026-02-20"}), "end")
        .await;
    let id = change["id"].as_str().unwrap().parse::<Uuid>().unwrap();
    let hex = id.simple().to_string().to_uppercase();
    let written = Database::connect(&world.f.dsn)
        .await
        .unwrap()
        .execute_raw(Statement::from_string(
            DbBackend::Sqlite,
            format!(
                "UPDATE pricing_price SET effective_to = NULL WHERE id = '{id}' OR hex(id) = '{hex}'"
            ),
        ))
        .await
        .unwrap();
    assert_eq!(written.rows_affected(), 1);
    let (status, body) = world.submit(&change["id"], "submit").await;
    assert_eq!(status, 500, "{body}");
    assert!(!code(&body).contains("END_DATE_INVALID"), "{body}");
    let rows = world.prices().await;
    assert_eq!(World::row(&rows, &live)["effective_to"], Value::Null);
    assert_eq!(World::row(&rows, &change["id"])["state"], "draft");
}

// ------------------------------------------------------------------ the unit (D-393)

/// Withdraw and reject leave the price untouched; a mixed unit applies its price and its cancel
/// together, and publishes only the price.
#[tokio::test]
async fn withdraw_and_reject_leave_the_price_untouched_and_a_mixed_unit_applies_both() {
    let world = World::at("2026-02-15").await;
    world.quorum(1).await;
    let scheduled = world.seed(1, "2026-03-01").await;
    let change = world.change(scheduled, "cancel", json!({}), "w").await;
    let (status, receipt) = world.submit(&change["id"], "submit-w").await;
    assert_eq!(status, 201, "{receipt}");
    let (status, withdrawn, _) = world
        .f
        .call(
            "POST",
            &format!(
                "/approval-units/{}/withdraw",
                receipt["unit"]["id"].as_str().unwrap()
            ),
            json!({}),
            None,
            Some("withdraw"),
        )
        .await;
    assert_eq!(status, 200, "{withdrawn}");
    let rows = world.prices().await;
    assert_eq!(World::row(&rows, &scheduled)["state"], "approved");
    assert_eq!(World::row(&rows, &scheduled)["effective_to"], Value::Null);
    assert_eq!(World::row(&rows, &change["id"])["state"], "draft");

    let (status, receipt) = world.submit(&change["id"], "submit-r").await;
    assert_eq!(status, 201, "{receipt}");
    let reviewer = world.f.user();
    let (status, rejected, _) = request(
        &world.f.app,
        &reviewer,
        "POST",
        &format!(
            "/approval-units/{}/reject",
            receipt["unit"]["id"].as_str().unwrap()
        ),
        json!({"generation": 1, "note": "no"}),
        None,
        Some("reject"),
    )
    .await;
    assert_eq!(status, 200, "{rejected}");
    let rows = world.prices().await;
    assert_eq!(World::row(&rows, &scheduled)["state"], "approved");
    assert_eq!(World::row(&rows, &change["id"])["status"], "rejected");

    world.quorum(0).await;
    let (status, drafted, _) = world
        .f
        .call(
            "POST",
            &format!("/price-book-entries/{}/prices", world.entry),
            json!({"price": {"rate": "2.00"}, "eligibility": "all", "effective_from": "2031-06-01"}),
            None,
            Some("set"),
        )
        .await;
    assert_eq!(status, 201, "{drafted}");
    let set = drafted["items"][0]["id"].clone();
    let change = world.change(scheduled, "cancel", json!({}), "mix").await;
    let (status, listing) = world
        .call(
            "GET",
            &format!("/price-books/{}/publish-changes", world.book),
            json!({}),
            None,
        )
        .await;
    assert_eq!(status, 200, "{listing}");
    let proposed = listing["prices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["price"]["id"] == change["id"])
        .unwrap();
    assert_eq!(
        proposed["before"]["id"],
        scheduled.to_string(),
        "a change's before is the price it names"
    );
    let (status, published) = world.publish(&[&set, &change["id"]], "publish-mix").await;
    assert_eq!(status, 201, "{published}");
    assert_eq!(published["applied"], true);
    let rows = world.prices().await;
    assert_eq!(World::row(&rows, &scheduled)["state"], "cancelled");
    let applied = World::row(&rows, &set);
    assert_eq!(applied["state"], "approved");
    assert_eq!(applied["change_kind"], "set");
    let mut expected = vec![
        world.listed(&set, ("2031-06-01", None), "approved"),
        world.listed(&scheduled, ("2026-03-01", None), "cancelled"),
    ];
    expected.sort_by_key(|p| p["priceId"].as_str().unwrap().to_owned());
    assert_eq!(
        world.announced().await,
        expected,
        "the new price and the cancelled one; a change row is not a published price"
    );
}

// ------------------------------------------------------------------ the event (D-520, D-521)

/// A unit that only cancels publishes the cancelled price and the predecessor it re-opens; the
/// price after them did not move and is not listed.
#[tokio::test]
async fn a_cancel_alone_publishes_the_cancelled_price_and_the_reopened_predecessor() {
    let world = World::at("2026-02-15").await;
    let a = world.seed_closed(1, "2026-01-01", "2026-03-01").await;
    let b = world.seed_closed(2, "2026-03-01", "2026-05-01").await;
    world.seed(3, "2026-05-01").await;
    let change = world.change(b, "cancel", json!({}), "cancel").await;
    let (status, receipt) = world.submit(&change["id"], "submit").await;
    assert_eq!(status, 201, "{receipt}");
    assert_eq!(receipt["applied"], true);
    let mut expected = vec![
        world.listed(&a, ("2026-01-01", Some("2026-05-01")), "approved"),
        world.listed(&b, ("2026-03-01", Some("2026-05-01")), "cancelled"),
    ];
    expected.sort_by_key(|p| p["priceId"].as_str().unwrap().to_owned());
    assert_eq!(world.announced().await, expected);
}

/// A unit that only ends publishes the ended price with its new end; the next price did not move
/// and is not listed.
#[tokio::test]
async fn an_end_alone_publishes_the_ended_price_with_its_new_end() {
    let world = World::at("2026-02-15").await;
    let live = world.seed_closed(1, "2026-01-01", "2026-03-01").await;
    world.seed(2, "2026-03-01").await;
    let change = world
        .change(live, "end", json!({"effective_to": "2026-02-20"}), "end")
        .await;
    let (status, receipt) = world.submit(&change["id"], "submit").await;
    assert_eq!(status, 201, "{receipt}");
    assert_eq!(
        world.announced().await,
        [world.listed(&live, ("2026-01-01", Some("2026-02-20")), "approved")]
    );
}

/// A mixed unit publishes its new price, the price it cancels and the predecessor whose end moved
/// onto the new price's start.
#[tokio::test]
async fn a_set_and_a_cancel_publish_the_new_price_the_cancelled_one_and_the_moved_predecessor() {
    let world = World::at("2026-02-15").await;
    let a = world.seed_closed(1, "2026-01-01", "2026-03-01").await;
    let b = world.seed(2, "2026-03-01").await;
    let set = world.draft("2031-06-01", "set").await;
    let cancel = world.change(b, "cancel", json!({}), "cancel").await;
    let (status, receipt) = world.publish(&[&set, &cancel["id"]], "publish").await;
    assert_eq!(status, 201, "{receipt}");
    assert_eq!(receipt["applied"], true);
    let mut expected = vec![
        world.listed(&a, ("2026-01-01", Some("2031-06-01")), "approved"),
        world.listed(&b, ("2026-03-01", None), "cancelled"),
        world.listed(&set, ("2031-06-01", None), "approved"),
    ];
    expected.sort_by_key(|p| p["priceId"].as_str().unwrap().to_owned());
    assert_eq!(world.announced().await, expected);
}

/// A unit of prices alone also lists the predecessor its new price re-closes.
#[tokio::test]
async fn a_set_alone_publishes_the_predecessor_it_recloses() {
    let world = World::at("2026-02-15").await;
    let a = world.seed(1, "2026-01-01").await;
    let set = world.draft("2031-06-01", "set").await;
    let (status, receipt) = world.submit(&set, "submit").await;
    assert_eq!(status, 201, "{receipt}");
    let mut expected = vec![
        world.listed(&a, ("2026-01-01", Some("2031-06-01")), "approved"),
        world.listed(&set, ("2031-06-01", None), "approved"),
    ];
    expected.sort_by_key(|p| p["priceId"].as_str().unwrap().to_owned());
    assert_eq!(world.announced().await, expected);
}

/// A change is not edited: its author deletes it and opens another.
#[tokio::test]
async fn a_change_is_deleted_not_edited() {
    let world = World::at("2026-02-15").await;
    world.seed(1, "2026-01-01").await;
    let scheduled = world.seed(2, "2026-03-01").await;
    let (status, change, tag) = world
        .f
        .call(
            "POST",
            &format!("/prices/{scheduled}/cancel"),
            json!({}),
            None,
            Some("cancel"),
        )
        .await;
    assert_eq!(status, 201, "{change}");
    let path = format!("/prices/{}", change["id"].as_str().unwrap());
    let (status, body, _) = world
        .f
        .call(
            "PATCH",
            &path,
            json!({"price": {"rate": "9.00"}}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(status, 409, "{body}");
    assert!(code(&body).contains("PRICE_NOT_DRAFT"), "{body}");
    let (status, body, _) = world
        .f
        .call("DELETE", &path, json!({}), Some(&tag), None)
        .await;
    assert_eq!(status, 204, "{body}");
    let rows = world.prices().await;
    assert!(rows.iter().all(|row| row["id"] != change["id"]));
    assert_eq!(World::row(&rows, &scheduled)["state"], "approved");
}

// ------------------------------------------------------------------ a binding (D-520 amended)

/// Ask 19's common case: a scheduled price followed by a `new` price is `keep_for_bound`, and no
/// binding names it, so it cancels. Its predecessor re-opens onto the `new` price's start and is
/// now the price kept for bound subscriptions. An acceptance that names the price outside its
/// bindings (here, as its order id) is not a binding of it.
#[tokio::test]
async fn a_scheduled_price_kept_for_bound_with_no_binding_cancels_and_its_predecessor_reopens() {
    let world = World::at("2026-02-15").await;
    let a = world.seed_closed(1, "2026-01-01", "2026-03-01").await;
    let b = world
        .write(2, "2026-03-01", |p| {
            p.effective_to = Some(date("2026-05-01"));
            p.keep_for_bound = true;
        })
        .await;
    let n = world
        .write(3, "2026-05-01", |p| p.eligibility = "new".into())
        .await;
    world.bind(a, b).await;
    let change = world.change(b, "cancel", json!({}), "cancel").await;
    let (status, receipt) = world.submit(&change["id"], "submit").await;
    assert_eq!(status, 201, "{receipt}");
    assert_eq!(receipt["applied"], true);
    let rows = world.prices().await;
    assert_eq!(World::row(&rows, &b)["state"], "cancelled");
    let reopened = World::row(&rows, &a);
    assert_eq!(reopened["effective_to"], "2026-05-01");
    assert_eq!(
        reopened["keep_for_bound"], true,
        "the price before the `new` one now keeps renewals"
    );
    assert_eq!(World::row(&rows, &n)["state"], "approved");
    assert_eq!(World::row(&rows, &n)["effective_to"], Value::Null);
}

/// A real binding refuses a cancel with `PRICE_BOUND`, `keep_for_bound` or not: an acceptance
/// whose bindings name the price. It is refused at the door, at submit and again at apply.
#[tokio::test]
async fn a_price_that_a_binding_names_is_refused_at_every_guard() {
    let world = World::at("2026-02-15").await;
    world.seed_closed(1, "2026-01-01", "2026-03-01").await;
    let b = world
        .write(2, "2026-03-01", |p| {
            p.effective_to = Some(date("2026-05-01"));
            p.keep_for_bound = true;
        })
        .await;
    let c = world.seed_closed(3, "2026-05-01", "2026-07-01").await;
    let d = world.seed(4, "2026-07-01").await;
    world.bind(b, Uuid::new_v4()).await;
    let answer = world
        .call(
            "POST",
            &format!("/prices/{b}/cancel"),
            json!({}),
            Some("door"),
        )
        .await;
    refused(&answer, 409, "PRICE_BOUND");

    let draft = world.change(c, "cancel", json!({}), "c").await;
    world.bind(c, Uuid::new_v4()).await;
    refused(
        &world.submit(&draft["id"], "submit-c").await,
        409,
        "PRICE_BOUND",
    );

    world.quorum(1).await;
    let pending = world.change(d, "cancel", json!({}), "d").await;
    let (status, receipt) = world.submit(&pending["id"], "submit-d").await;
    assert_eq!(status, 201, "{receipt}");
    assert_eq!(receipt["applied"], false);
    world.bind(d, Uuid::new_v4()).await;
    let answer = world.approve_as_reviewer(&receipt, "approve-d").await;
    refused(&answer, 409, "APPLY_REFUSED");
    assert!(code(&answer.1).contains("PRICE_BOUND"), "{}", answer.1);
    let rows = world.prices().await;
    for id in [b, c, d] {
        assert_eq!(World::row(&rows, &id)["state"], "approved", "{id}");
    }
}
