//! A change of the deployment's currency table against stored prices. `XTS` (a code ISO 4217 lists
//! without a scale) is configured at 2, prices are written, then a state over the same database
//! configures it at 3. History wins: an approved price keeps the scale it was written with, and so
//! do the cancel and end rows that name it, which submit and apply at that scale. New content
//! takes the table's scale: a new price, and the return a temporary price makes. A draft written
//! under the old scale is refused at submit and at approval (`CURRENCY_SCALE_CHANGED`, 409) and
//! left as it was, until its author edits it. A table that no longer lists the book's currency
//! refuses every new price and every draft (`BOOK_CURRENCY_INVALID`, 409).
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod entry_support;

use axum::Router;
use entry_support::{Fixture, Script, policy_support, request};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// One `XTS` book with one usage entry, approvals at quorum 1, and two routers over its database:
/// `XTS` at the fixture's scale and `XTS` at the changed one (`changed`).
struct World {
    f: Fixture,
    changed: Router,
    book: String,
    entry: String,
}

impl World {
    /// `XTS` at 2, then at 3.
    async fn new() -> Self {
        Self::with_scales(2, 3).await
    }
    /// `XTS` at `from`, then at `to`.
    async fn with_scales(from: u8, to: u8) -> Self {
        let f = Fixture::with_currency_scales(Arc::new(Script::default()), &[("XTS", from)]).await;
        let (s, book, _) = f
            .call(
                "POST",
                "/price-books",
                json!({"code":"tests","name":"Tests","currency":"XTS"}),
                None,
                Some("book"),
            )
            .await;
        assert_eq!(s, 201, "an XTS book is created while XTS is configured");
        let (s, entry, _) = f
            .call(
                "POST",
                &format!("/price-books/{}/entries", book["id"].as_str().unwrap()),
                json!({
                    "usage_rating_policy": policy_support::input(),
                    "sku_id": Uuid::new_v4(),
                    "model": "per_unit"
                }),
                None,
                Some("entry"),
            )
            .await;
        assert_eq!(s, 201, "the entry is created");
        let (_, _, tag) = f
            .call("GET", "/approval-policy", json!({}), None, None)
            .await;
        let (s, _, _) = f
            .call(
                "PUT",
                "/approval-policy",
                json!({"quorum": 1}),
                Some(&tag),
                None,
            )
            .await;
        assert_eq!(s, 200, "one reviewer approves a unit");
        let changed = f.app_with_currency_scales(&[("XTS", to)]).await;
        Self {
            f,
            changed,
            book: book["id"].as_str().unwrap().to_owned(),
            entry: entry["id"].as_str().unwrap().to_owned(),
        }
    }

    fn prices_path(&self) -> String {
        format!("/price-book-entries/{}/prices", self.entry)
    }

    /// A call as the author through `app`.
    async fn call(
        &self,
        app: &Router,
        method: &str,
        path: &str,
        body: Value,
        key: Option<&str>,
    ) -> (u16, Value) {
        let (status, body, _) = request(app, &self.f.ctx, method, path, body, None, key).await;
        (status, body)
    }

    /// A draft price of the default chain through `app`; its rows as the door answers them.
    async fn draft(&self, app: &Router, body: Value, key: &str) -> Vec<Value> {
        let (status, created) = self
            .call(app, "POST", &self.prices_path(), body, Some(key))
            .await;
        assert_eq!(status, 201, "the draft is created");
        created["items"].as_array().unwrap().clone()
    }

    /// Submit a draft through `app`; the unit it is pending in.
    async fn submit(&self, app: &Router, price: &Value, key: &str) -> (u16, Value) {
        self.call(
            app,
            "POST",
            &format!("/prices/{}/submit", price["id"].as_str().unwrap()),
            json!({}),
            Some(key),
        )
        .await
    }

    /// Approve a unit through `app` as a reviewer who wrote none of its prices.
    async fn approve(&self, app: &Router, unit: &Value, key: &str) -> (u16, Value) {
        let reviewer: SecurityContext = self.f.user();
        let (status, body, _) = request(
            app,
            &reviewer,
            "POST",
            &format!("/approval-units/{}/approve", unit["id"].as_str().unwrap()),
            json!({"generation": 1}),
            None,
            Some(key),
        )
        .await;
        (status, body)
    }

    /// Submit every draft of the book through `app` in one unit (a temporary pair goes whole).
    async fn publish(&self, app: &Router, key: &str) -> (u16, Value) {
        self.call(
            app,
            "POST",
            &format!("/price-books/{}/publish-changes", self.book),
            json!({}),
            Some(key),
        )
        .await
    }

    /// PATCH a draft through `app` at the version the price list reads now.
    async fn patch(&self, app: &Router, price: &Value, body: Value) -> (u16, Value) {
        let version = self.row(price).await["version"].as_i64().unwrap();
        let (status, body, _) = request(
            app,
            &self.f.ctx,
            "PATCH",
            &format!("/prices/{}", price["id"].as_str().unwrap()),
            body,
            Some(&format!("\"{version}\"")),
            None,
        )
        .await;
        (status, body)
    }

    /// An approved price at the fixture's scale: drafted, submitted and approved through the
    /// fixture's router.
    async fn approved(&self, body: Value, key: &str) -> Value {
        let old = self.f.app.clone();
        let price = self.draft(&old, body, key).await[0].clone();
        let (status, receipt) = self.submit(&old, &price, &format!("{key}-submit")).await;
        assert_eq!(status, 201, "the price is submitted");
        let (status, _) = self
            .approve(&old, &receipt["unit"], &format!("{key}-approve"))
            .await;
        assert_eq!(status, 200, "the price is approved");
        price
    }

    /// A price row as the entry's price list reads it now, through the fixture's router.
    async fn row(&self, price: &Value) -> Value {
        self.row_via(&self.f.app, price).await
    }

    /// A price row as the entry's price list reads it now, through `app`.
    async fn row_via(&self, app: &Router, price: &Value) -> Value {
        let (status, body) = self
            .call(app, "GET", &self.prices_path(), json!({}), None)
            .await;
        assert_eq!(status, 200, "the entry's prices are listed");
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == price["id"])
            .expect("the price is listed")
            .clone()
    }
}

fn price(from: &str) -> Value {
    json!({"price": {"rate": "0.5"}, "eligibility": "all", "effective_from": from})
}

/// The code of a refusal: a conflict's reason, or a 400's first violation.
fn code_of(body: &Value) -> &str {
    let context = &body["context"];
    context["reason"]
        .as_str()
        .or_else(|| context["field_violations"][0]["reason"].as_str())
        .unwrap_or_default()
}

/// The refusal's detail: what the caller is told to do.
fn detail_of(body: &Value) -> &str {
    body["detail"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn a_table_change_keeps_stored_scales_and_refuses_a_stale_draft_until_it_is_edited() {
    let w = World::new().await;
    let old = w.f.app.clone();
    // Under XTS at 2: a fee on three places is off the currency increment.
    let (status, body) = w
        .call(
            &old,
            "POST",
            &w.prices_path(),
            json!({"price": {"rate": "0.5"}, "min_fee": "0.125", "eligibility": "all",
                "effective_from": "2030-02-01"}),
            Some("fee-at-2"),
        )
        .await;
    assert_eq!(status, 400, "three places are off a two-place increment");
    assert_eq!(code_of(&body), "MIN_FEE_INVALID", "the fee is refused");
    // An approved price, a pending draft and a draft never submitted, all written at 2.
    let approved = w.draft(&old, price("2030-01-01"), "approved").await[0].clone();
    assert_eq!(
        (&approved["currency"], &approved["currency_scale"]),
        (&json!("XTS"), &json!(2)),
        "a new price takes the table's scale"
    );
    let (status, receipt) = w.submit(&old, &approved, "submit-approved").await;
    assert_eq!(status, 201, "the price is submitted");
    let (status, _) = w.approve(&old, &receipt["unit"], "approve-approved").await;
    assert_eq!(status, 200, "the price is approved at 2");
    let pending = w.draft(&old, price("2031-01-01"), "pending").await[0].clone();
    let (status, receipt) = w.submit(&old, &pending, "submit-pending").await;
    assert_eq!(status, 201, "the draft is submitted");
    let unit = receipt["unit"].clone();
    let unsent = w.draft(&old, price("2032-01-01"), "unsent").await[0].clone();

    // The table changes: XTS at 3.
    let new = w.changed.clone();
    // The approved price reads the scale it was stored with, in every read.
    let (status, read) = w
        .call(
            &new,
            "GET",
            &format!("/prices/{}", approved["id"].as_str().unwrap()),
            json!({}),
            None,
        )
        .await;
    assert_eq!(status, 200, "the approved price is read");
    assert_eq!(
        read["currency_scale"], 2,
        "the pinned read keeps the stored scale"
    );
    assert_eq!(
        w.row_via(&new, &approved).await["currency_scale"],
        2,
        "the authoring read keeps the stored scale"
    );
    // A new price takes 3, and its fee is judged on three places.
    let fresh = w
        .draft(
            &new,
            json!({"price": {"rate": "0.5"}, "min_fee": "0.125", "eligibility": "all",
                "effective_from": "2033-01-01"}),
            "fresh",
        )
        .await;
    assert_eq!(
        fresh[0]["currency_scale"], 3,
        "a new price takes the new scale"
    );
    assert_eq!(
        fresh[0]["min_fee"], "0.125",
        "its fee is on the new increment"
    );

    // The pending draft is refused at approval and left as it was.
    let before = w.row(&pending).await;
    assert_eq!(
        before["currency_scale"], 2,
        "the pending draft was written at 2"
    );
    let (status, body) = w.approve(&new, &unit, "approve-stale").await;
    assert_eq!(status, 409, "a stale draft is not approved");
    assert_eq!(
        code_of(&body),
        "CURRENCY_SCALE_CHANGED",
        "the refusal names the changed scale"
    );
    assert!(
        detail_of(&body).contains("withdraws the unit, edits the draft and submits it again"),
        "at apply the draft is locked, so the unit is withdrawn first"
    );
    assert!(
        detail_of(&body).contains("stored at scale 2, the table now gives XTS scale 3"),
        "the refusal names both scales"
    );
    assert_eq!(
        w.row(&pending).await,
        before,
        "the refused draft is unchanged"
    );
    // A stale draft is refused at submit too, and left as it was.
    let before = w.row(&unsent).await;
    let (status, body) = w.submit(&new, &unsent, "submit-stale").await;
    assert_eq!(status, 409, "a stale draft is not submitted");
    assert_eq!(
        code_of(&body),
        "CURRENCY_SCALE_CHANGED",
        "the refusal names the changed scale"
    );
    assert!(
        detail_of(&body).contains("edit the draft and submit it again")
            && !detail_of(&body).contains("withdraw"),
        "at submit the draft is not locked, so its author edits it"
    );
    assert_eq!(
        w.row(&unsent).await,
        before,
        "the refused draft is unchanged"
    );

    // Withdrawn and edited, the draft takes 3 and passes.
    let (status, _) = w
        .call(
            &new,
            "POST",
            &format!("/approval-units/{}/withdraw", unit["id"].as_str().unwrap()),
            json!({}),
            Some("withdraw"),
        )
        .await;
    assert_eq!(status, 200, "the author withdraws the unit");
    let version = w.row(&pending).await["version"].as_i64().unwrap();
    let (status, edited, _) = request(
        &new,
        &w.f.ctx,
        "PATCH",
        &format!("/prices/{}", pending["id"].as_str().unwrap()),
        json!({"note": "resubmitted at the new scale"}),
        Some(&format!("\"{version}\"")),
        None,
    )
    .await;
    assert_eq!(status, 200, "the author edits the draft");
    assert_eq!(edited["currency_scale"], 3, "an edit takes the new scale");
    let (status, receipt) = w.submit(&new, &pending, "resubmit").await;
    assert_eq!(status, 201, "the edited draft is submitted");
    let (status, _) = w.approve(&new, &receipt["unit"], "approve-edited").await;
    assert_eq!(status, 200, "the edited draft is approved");
    let applied = w.row(&pending).await;
    assert_eq!(
        (&applied["state"], &applied["currency_scale"]),
        (&json!("approved"), &json!(3)),
        "the price is approved at the scale it was edited at"
    );

    // A cancel and an end of the old price carry its stored scale, not the table's.
    for (kind, body) in [
        ("cancel", json!({})),
        ("end", json!({"effective_to": "2030-09-01"})),
    ] {
        let (status, change) = w
            .call(
                &new,
                "POST",
                &format!("/prices/{}/{kind}", approved["id"].as_str().unwrap()),
                body,
                Some(kind),
            )
            .await;
        assert_eq!(status, 201, "the change row is drafted");
        assert_eq!(
            change["currency_scale"], 2,
            "a change row keeps its price's scale"
        );
    }

    // A temporary price over the old price returns to its money at the table's scale.
    let pair = w
        .draft(
            &new,
            json!({"price": {"rate": "0.25"}, "eligibility": "all",
                "effective_from": "2030-03-01", "temporary_until": "2030-05-01"}),
            "temporary",
        )
        .await;
    assert_eq!(pair.len(), 2, "the temporary price has a return");
    assert_eq!(
        pair[1]["return_of_price_id"], approved["id"],
        "it returns to the old price"
    );
    assert_eq!(
        pair[1]["price_json"], approved["price_json"],
        "with the old price's money"
    );
    assert_eq!(
        (&pair[0]["currency_scale"], &pair[1]["currency_scale"]),
        (&json!(3), &json!(3)),
        "the temporary price and its return are new content at the new scale"
    );
}

/// A table that no longer lists a book's currency takes no new price in it: a create, a PATCH, a
/// submit and an approval are each refused with `BOOK_CURRENCY_INVALID` (409), whose detail gives
/// no edit to try. Every stored price reads as it was stored, and a cancel of an approved price,
/// which keeps its price's spec, still submits and applies.
#[tokio::test]
async fn a_currency_the_table_forgot_takes_no_new_price_and_keeps_its_history() {
    let w = World::new().await;
    let old = w.f.app.clone();
    let approved = w.approved(price("2030-01-01"), "approved").await;
    let pending = w.draft(&old, price("2031-01-01"), "pending").await[0].clone();
    let (status, receipt) = w.submit(&old, &pending, "submit-pending").await;
    assert_eq!(status, 201, "the draft is submitted");
    let unsent = w.draft(&old, price("2032-01-01"), "unsent").await[0].clone();

    let iso_only = w.f.app_with_currency_scales(&[]).await;
    let forgotten = |body: &Value| {
        code_of(body) == "BOOK_CURRENCY_INVALID"
            && detail_of(body).contains("no longer lists the book's currency")
            && !detail_of(body).contains("submit it again")
    };
    let (status, body) = w
        .call(
            &iso_only,
            "POST",
            &w.prices_path(),
            price("2033-01-01"),
            Some("forgotten"),
        )
        .await;
    assert_eq!(
        status, 409,
        "no price is written in a currency the table forgot"
    );
    assert!(forgotten(&body), "the create names the forgotten currency");
    let before = w.row(&unsent).await;
    let (status, body) = w
        .patch(&iso_only, &unsent, json!({"note": "an edit cannot help"}))
        .await;
    assert_eq!(status, 409, "no draft is edited in a forgotten currency");
    assert!(forgotten(&body), "the PATCH names the forgotten currency");
    let (status, body) = w.submit(&iso_only, &unsent, "submit-forgotten").await;
    assert_eq!(
        status, 409,
        "a draft in a forgotten currency is not submitted"
    );
    assert!(forgotten(&body), "the submit names the forgotten currency");
    assert_eq!(w.row(&unsent).await, before, "the draft is unchanged");
    let (status, body) = w
        .approve(&iso_only, &receipt["unit"], "approve-forgotten")
        .await;
    assert_eq!(
        status, 409,
        "a draft in a forgotten currency is not approved"
    );
    assert!(
        forgotten(&body),
        "the approval names the forgotten currency"
    );

    let (status, read) = w
        .call(
            &iso_only,
            "GET",
            &format!("/prices/{}", approved["id"].as_str().unwrap()),
            json!({}),
            None,
        )
        .await;
    assert_eq!(status, 200, "the approved price is still served");
    assert_eq!(
        (&read["currency"], &read["currency_scale"]),
        (&json!("XTS"), &json!(2)),
        "with the spec it was stored with"
    );
    // The entry's price list serves every stored price as it was stored, drafts included. The
    // pinned read serves approved and cancelled prices only, so a draft there is its usual 404.
    for stored in [&approved, &pending, &unsent] {
        let row = w.row_via(&iso_only, stored).await;
        assert_eq!(
            (&row["currency"], &row["currency_scale"]),
            (&json!("XTS"), &json!(2)),
            "the price list keeps the stored spec"
        );
    }
    for draft in [&pending, &unsent] {
        let (status, _) = w
            .call(
                &iso_only,
                "GET",
                &format!("/prices/{}", draft["id"].as_str().unwrap()),
                json!({}),
                None,
            )
            .await;
        assert_eq!(status, 404, "the pinned read serves no draft");
    }
    // History still moves: a cancel keeps its price's spec and is never judged on the table.
    let (status, cancel) = w
        .call(
            &iso_only,
            "POST",
            &format!("/prices/{}/cancel", approved["id"].as_str().unwrap()),
            json!({}),
            Some("cancel-forgotten"),
        )
        .await;
    assert_eq!(status, 201, "the cancel is drafted");
    assert_eq!(cancel["currency_scale"], 2, "it keeps its price's scale");
    let (status, receipt) = w.submit(&iso_only, &cancel, "submit-cancel").await;
    assert_eq!(status, 201, "the cancel is submitted");
    let (status, _) = w
        .approve(&iso_only, &receipt["unit"], "approve-cancel")
        .await;
    assert_eq!(status, 200, "the cancel is applied");
    assert_eq!(
        w.row(&approved).await["state"],
        "cancelled",
        "the price is cancelled"
    );
}

/// A cancel and an end of prices stored at 2 keep that scale under a table at 3, and submit and
/// apply: a change row is not judged on the table, so a table change never blocks the history of
/// an old price.
#[tokio::test]
async fn a_cancel_and_an_end_at_the_old_scale_submit_and_apply_under_a_changed_table() {
    let w = World::new().await;
    let first = w.approved(price("2030-01-01"), "first").await;
    let second = w.approved(price("2031-01-01"), "second").await;
    let new = w.changed.clone();
    for (kind, target, body) in [
        ("end", &second, json!({"effective_to": "2032-01-01"})),
        ("cancel", &first, json!({})),
    ] {
        let (status, change) = w
            .call(
                &new,
                "POST",
                &format!("/prices/{}/{kind}", target["id"].as_str().unwrap()),
                body,
                Some(kind),
            )
            .await;
        assert_eq!(status, 201, "the change row is drafted");
        assert_eq!(
            change["currency_scale"], 2,
            "a change row keeps its price's scale"
        );
        let (status, receipt) = w.submit(&new, &change, &format!("{kind}-submit")).await;
        assert_eq!(status, 201, "a change row at the old scale is submitted");
        let (status, _) = w
            .approve(&new, &receipt["unit"], &format!("{kind}-approve"))
            .await;
        assert_eq!(status, 200, "a change row at the old scale is applied");
        assert_eq!(
            w.row(&change).await["currency_scale"],
            2,
            "the applied change row keeps the old scale"
        );
    }
    let ended = w.row(&second).await;
    assert_eq!(
        (&ended["effective_to"], &ended["currency_scale"]),
        (&json!("2032-01-01"), &json!(2)),
        "the end applied and the price kept its scale"
    );
    let cancelled = w.row(&first).await;
    assert_eq!(
        (&cancelled["state"], &cancelled["currency_scale"]),
        (&json!("cancelled"), &json!(2)),
        "the cancel applied and the price kept its scale"
    );
}

/// A temporary price over a price stored at 2, drafted under a table at 3: both halves are new
/// content at 3, and the pair submits and applies.
#[tokio::test]
async fn a_new_scale_temporary_pair_over_an_old_scale_price_submits_and_applies() {
    let w = World::new().await;
    let approved = w.approved(price("2030-01-01"), "approved").await;
    let new = w.changed.clone();
    let pair = w
        .draft(
            &new,
            json!({"price": {"rate": "0.25"}, "eligibility": "all",
                "effective_from": "2030-03-01", "temporary_until": "2030-05-01"}),
            "temporary",
        )
        .await;
    assert_eq!(pair.len(), 2, "the temporary price has a return");
    assert_eq!(
        pair[1]["return_of_price_id"], approved["id"],
        "it returns to the old price"
    );
    let (status, receipt) = w.publish(&new, "publish-pair").await;
    assert_eq!(status, 201, "the pair at the new scale is submitted");
    let (status, _) = w.approve(&new, &receipt["unit"], "approve-pair").await;
    assert_eq!(status, 200, "the pair at the new scale is applied");
    for half in &pair {
        let row = w.row(half).await;
        assert_eq!(
            (&row["state"], &row["currency_scale"]),
            (&json!("approved"), &json!(3)),
            "each half is approved at the new scale"
        );
    }
    assert_eq!(
        w.row(&approved).await["currency_scale"],
        2,
        "the old price keeps its scale"
    );
}

/// A temporary pair drafted at 2 and edited under a table at 3 without new dates: the PATCH
/// re-stamps only the edited half, so the pair stays refused (`CURRENCY_SCALE_CHANGED`, naming the
/// half still at 2) until that half is edited too.
#[tokio::test]
async fn editing_one_half_of_a_pair_leaves_its_partner_at_the_old_scale_until_edited() {
    let w = World::new().await;
    let old = w.f.app.clone();
    w.approved(price("2030-01-01"), "approved").await;
    let pair = w
        .draft(
            &old,
            json!({"price": {"rate": "0.25"}, "eligibility": "all",
                "effective_from": "2030-03-01", "temporary_until": "2030-05-01"}),
            "temporary",
        )
        .await;
    let (promo, partner) = (&pair[0], &pair[1]);
    assert_eq!(partner["currency_scale"], 2, "the pair is drafted at 2");
    let new = w.changed.clone();
    let (status, edited) = w
        .patch(&new, promo, json!({"note": "the promo only"}))
        .await;
    assert_eq!(status, 200, "the temporary half is edited");
    assert_eq!(edited["currency_scale"], 3, "the edited half takes 3");
    assert_eq!(
        w.row(partner).await["currency_scale"],
        2,
        "its partner keeps 2"
    );
    let (status, body) = w.publish(&new, "publish-half").await;
    assert_eq!(status, 409, "the pair is not submitted");
    assert_eq!(
        code_of(&body),
        "CURRENCY_SCALE_CHANGED",
        "the partner's scale is stale"
    );
    assert!(
        detail_of(&body).contains(partner["id"].as_str().unwrap()),
        "the refusal names the half still at the old scale"
    );
    let (status, edited) = w
        .patch(&new, partner, json!({"note": "the return too"}))
        .await;
    assert_eq!(status, 200, "the return is edited");
    assert_eq!(edited["currency_scale"], 3, "the return takes 3");
    let (status, receipt) = w.publish(&new, "publish-pair").await;
    assert_eq!(
        status, 201,
        "the pair is submitted once both halves are at 3"
    );
    let (status, _) = w.approve(&new, &receipt["unit"], "approve-pair").await;
    assert_eq!(status, 200, "the pair is applied");
}

/// A table that narrows the scale (3 to 2) judges an edited draft's fee on the new scale: a fee on
/// three places stored at 3 makes even a note-only PATCH a `MIN_FEE_INVALID`, and the draft is
/// left as it was.
#[tokio::test]
async fn a_narrowed_scale_refuses_an_edit_whose_fee_falls_off_the_new_increment() {
    let w = World::with_scales(3, 2).await;
    let old = w.f.app.clone();
    let draft = w
        .draft(
            &old,
            json!({"price": {"rate": "0.5"}, "min_fee": "0.125", "eligibility": "all",
                "effective_from": "2030-01-01"}),
            "fee-at-3",
        )
        .await[0]
        .clone();
    assert_eq!(
        (&draft["currency_scale"], &draft["min_fee"]),
        (&json!(3), &json!("0.125")),
        "a fee on three places fits a three-place increment"
    );
    let before = w.row(&draft).await;
    let (status, body) = w
        .patch(&w.changed, &draft, json!({"note": "a note only"}))
        .await;
    assert_eq!(status, 400, "the fee is off the new increment");
    assert_eq!(code_of(&body), "MIN_FEE_INVALID", "the fee is refused");
    assert_eq!(w.row(&draft).await, before, "the draft is unchanged");
}

/// A temporary price over an approved price whose fee fits only the old, wider scale (3, narrowed
/// to 2). The return copies that fee and takes today's scale, so the pair is refused
/// `MIN_FEE_INVALID` on a fee the caller did not send; the message names the price the fee is
/// copied from and today's scale. Nothing is written.
#[tokio::test]
async fn a_temporary_price_over_a_fee_off_the_narrowed_scale_names_where_its_fee_comes_from() {
    let w = World::with_scales(3, 2).await;
    let approved = w
        .approved(
            json!({"price": {"rate": "0.5"}, "min_fee": "0.125", "eligibility": "all",
                "effective_from": "2030-01-01"}),
            "approved",
        )
        .await;
    let (status, body) = w
        .call(
            &w.changed,
            "POST",
            &w.prices_path(),
            json!({"price": {"rate": "0.25"}, "eligibility": "all",
                "effective_from": "2030-03-01", "temporary_until": "2030-05-01"}),
            Some("temporary"),
        )
        .await;
    assert_eq!(status, 400, "the return's fee is off the new increment");
    let violation = &body["context"]["field_violations"][0];
    assert_eq!(
        (&violation["field"], &violation["reason"]),
        (&json!("min_fee"), &json!("MIN_FEE_INVALID")),
        "the fee is refused with its own code"
    );
    let expected = format!(
        "the min_fee is copied from price {}, the price the temporary price returns to, and is \
         not a multiple of the increment of today's currency_scale 2",
        approved["id"].as_str().unwrap()
    );
    assert_eq!(
        violation["description"].as_str(),
        Some(expected.as_str()),
        "the message names where the fee comes from and today's scale"
    );
    let (status, listed) = w
        .call(&w.changed, "GET", &w.prices_path(), json!({}), None)
        .await;
    assert_eq!(status, 200, "the entry's prices are listed");
    assert_eq!(
        listed["items"].as_array().map(Vec::len),
        Some(1),
        "no half of the pair is written"
    );
}
