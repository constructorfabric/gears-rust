//! A temporary draft's dates move (phase 7, run 7.2, ask 21): `PATCH /prices/{id}` of the temporary
//! half of a draft takes `effective_from` and `temporary_until`, re-runs the pair builder over the
//! new dates and reconciles every shape it moves between in the same transaction — a pair stays a
//! pair (its return re-derived in place), loses its return (the promo alone, or one closed price),
//! or gains one (numbered after every price of the entry). Both halves are judged as the create
//! judges them; a return's own dates and a price's temporariness stay fixed.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod entry_support;
use bss_pricing::infra::storage::{
    entity::price,
    repo::{price_book_entry_repo, price_repo},
};
use entry_support::policy_support;
use entry_support::{Fixture, Script};
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit_db::secure::AccessScope;
use uuid::Uuid;

/// A book and one confirmed usage entry in `per_unit` (D-427).
async fn priced() -> (Fixture, Value) {
    let f = Fixture::new(Arc::new(Script::default())).await;
    let (book, _) = f.book().await;
    let (status, entry, _) = f
        .call(
            "POST",
            &format!("/price-books/{}/entries", book["id"].as_str().unwrap()),
            json!({"usage_rating_policy":policy_support::input(),"sku_id":Uuid::new_v4(),"model":"per_unit"}),
            None,
            Some("entry"),
        )
        .await;
    assert_eq!(status, 201, "{entry}");
    (f, entry)
}
fn entry_id(entry: &Value) -> Uuid {
    entry["id"].as_str().unwrap().parse().unwrap()
}
fn date(text: &str) -> time::Date {
    time::Date::parse(text, &time::format_description::well_known::Iso8601::DATE).unwrap()
}
/// An approved price written directly, as a unit's apply would leave it: `rate` 0.20, from
/// `from`, explicitly closed at `to` when given, temporary until `until` when given.
async fn approved(
    f: &Fixture,
    entry: &Value,
    version_no: i32,
    from: &str,
    to: Option<&str>,
    until: Option<&str>,
) -> Uuid {
    let tenant = f.ctx.subject_tenant_id();
    let scope = AccessScope::for_tenant(tenant);
    let conn = f.db.conn().unwrap();
    let p = price_book_entry_repo::find(&conn, &scope, tenant, entry_id(entry))
        .await
        .unwrap()
        .unwrap();
    let mut price = entry_support::price(&p);
    price.version_no = version_no;
    price.state = "approved".into();
    price.price_json = json!({"rate":"0.20"});
    price.effective_from = date(from);
    price.effective_to = to.or(until).map(date);
    price.closed_explicitly = to.is_some();
    price.temporary_until = until.map(date);
    price_repo::insert(&conn, &scope, price).await.unwrap().id
}
/// A draft through the door: one price, or a temporary one with its partner when `until` is given.
async fn drafted(f: &Fixture, entry: &Value, from: &str, until: Option<&str>) -> Vec<Value> {
    let mut body = json!({"price":{"rate":"0.05"},"eligibility":"all","effective_from":from});
    if let Some(until) = until {
        body["temporary_until"] = json!(until);
    }
    let (status, created, _) = f
        .call(
            "POST",
            &format!("/price-book-entries/{}/prices", entry_id(entry)),
            body,
            None,
            Some(&Uuid::new_v4().to_string()),
        )
        .await;
    assert_eq!(status, 201, "{created}");
    created["items"].as_array().unwrap().clone()
}
async fn stored(f: &Fixture, id: &Value) -> Option<price::Model> {
    let tenant = f.ctx.subject_tenant_id();
    price_repo::find(
        &f.db.conn().unwrap(),
        &AccessScope::for_tenant(tenant),
        tenant,
        id.as_str().unwrap().parse().unwrap(),
    )
    .await
    .unwrap()
}
async fn patch(f: &Fixture, id: &Value, body: Value, tag: &str) -> (u16, Value, String) {
    f.call(
        "PATCH",
        &format!("/prices/{}", id.as_str().unwrap()),
        body,
        Some(tag),
        None,
    )
    .await
}
/// Submit a pair through publish-changes, which takes the partner with it (a half alone is
/// `PAIR_SPLIT`).
async fn publish_pair(f: &Fixture, entry: &Value, promo: &Value) -> (u16, Value, String) {
    f.call(
        "POST",
        &format!(
            "/price-books/{}/publish-changes",
            entry["book_id"].as_str().unwrap()
        ),
        json!({"price_ids":[promo]}),
        None,
        Some(&Uuid::new_v4().to_string()),
    )
    .await
}
fn code_in(body: &Value, code: &str) -> bool {
    body.to_string().contains(code)
}
/// The audit actions written for one price, in order.
async fn audited(f: &Fixture, id: &Value) -> Vec<(String, i64)> {
    Database::connect(&f.dsn)
        .await
        .unwrap()
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT action, subject_revision FROM pricing_audit WHERE subject_id = ? \
             ORDER BY subject_revision, action",
            [id.as_str().unwrap().parse::<Uuid>().unwrap().into()],
        ))
        .await
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.try_get::<String>("", "action").unwrap(),
                r.try_get::<Option<i64>>("", "subject_revision")
                    .unwrap()
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// Pair → pair: the return is re-derived in place (its id, number and author stay; its start is
/// the new end, its money copied again from the price it returns to — an edited return is stale
/// at submit anyway, D-391), at a new version, with a `price.patch` audit row. The edited price
/// answers with its partner. The reconciled pair is current: submit takes it.
#[tokio::test]
async fn moving_a_pairs_dates_re_derives_its_return_in_place() {
    let (f, entry) = priced().await;
    let back = approved(&f, &entry, 1, "2031-01-01", None, None).await;
    let pair = drafted(&f, &entry, "2031-03-01", Some("2031-03-11")).await;
    let (promo, ret) = (&pair[0]["id"], &pair[1]["id"]);
    // An edited return's money is copied again when its pair moves.
    let edited = patch(&f, ret, json!({"price":{"rate":"0.21"}}), "\"1\"").await;
    assert_eq!(edited.0, 200, "{edited:?}");

    let moved = patch(&f, promo, json!({"temporary_until":"2031-03-20"}), "\"1\"").await;
    assert_eq!(moved.0, 200, "{moved:?}");
    assert_eq!(moved.2, "\"2\"");
    assert_eq!(moved.1["effective_from"], "2031-03-01");
    assert_eq!(moved.1["temporary_until"], "2031-03-20");
    assert_eq!(moved.1["effective_to"], "2031-03-20");
    assert_eq!(moved.1["closed_explicitly"], false);
    assert_eq!(
        moved.1["paired_price_id"], *ret,
        "the partner is the same row"
    );
    let r = stored(&f, ret).await.unwrap();
    assert_eq!(r.effective_from, date("2031-03-20"));
    assert_eq!(r.effective_to, None);
    assert!(!r.closed_explicitly);
    assert_eq!(
        r.version_no,
        i32::try_from(pair[1]["version_no"].as_i64().unwrap()).unwrap()
    );
    assert_eq!(r.version, 3, "created, edited, re-derived");
    assert_eq!(
        r.price_json,
        json!({"rate":"0.20"}),
        "the money is copied again"
    );
    assert_eq!(r.return_of_price_id, Some(back));
    assert_eq!(
        r.paired_price_id.map(|p| p.to_string()),
        promo.as_str().map(str::to_owned)
    );
    assert_eq!(
        audited(&f, ret).await,
        [
            ("price.create".to_owned(), 1),
            ("price.patch".to_owned(), 2),
            ("price.patch".to_owned(), 3)
        ]
    );

    // Both dates at once, then the start alone: the end stays and the return follows it.
    let moved = patch(
        &f,
        promo,
        json!({"effective_from":"2031-03-05","temporary_until":"2031-03-25"}),
        "\"2\"",
    )
    .await;
    assert_eq!(moved.0, 200, "{moved:?}");
    let moved = patch(&f, promo, json!({"effective_from":"2031-03-02"}), "\"3\"").await;
    assert_eq!(moved.0, 200, "{moved:?}");
    assert_eq!(moved.1["effective_from"], "2031-03-02");
    assert_eq!(moved.1["temporary_until"], "2031-03-25");
    assert_eq!(moved.1["paired_price_id"], *ret);
    let r = stored(&f, ret).await.unwrap();
    assert_eq!(r.effective_from, date("2031-03-25"));
    assert_eq!(r.version, 5);

    // D-391: the pair as it now stands is what submit expects (no PAIR_RETURN_STALE).
    let (status, receipt, _) = publish_pair(&f, &entry, promo).await;
    assert_eq!(status, 201, "{receipt}");
}

/// Promo alone → pair → promo alone. A new return takes the entry's NEXT version number, never
/// `promo.version_no + 1`, which another price may hold (the unique `(entry, version_no)` index);
/// it is written first and the promo then names it. Losing the return, the promo is written first
/// (the link cleared) and the return is deleted, with a `price.delete` audit row.
#[tokio::test]
async fn a_return_is_created_numbered_after_every_price_and_deleted_when_the_end_meets_the_next_start()
 {
    let (f, entry) = priced().await;
    let back = approved(&f, &entry, 1, "2031-01-01", None, None).await;
    approved(&f, &entry, 2, "2031-04-01", None, None).await;
    // The chain's next price starts exactly on the end: the promo alone.
    let alone = drafted(&f, &entry, "2031-03-01", Some("2031-04-01")).await;
    assert_eq!(alone.len(), 1, "{alone:?}");
    let promo = &alone[0]["id"];
    assert_eq!(alone[0]["version_no"], 3);
    assert!(alone[0]["paired_price_id"].is_null());
    // Another draft holds `promo.version_no + 1`.
    let other = drafted(&f, &entry, "2031-09-01", None).await;
    assert_eq!(other[0]["version_no"], 4);

    let paired = patch(&f, promo, json!({"temporary_until":"2031-03-20"}), "\"1\"").await;
    assert_eq!(paired.0, 200, "{paired:?}");
    let ret = paired.1["paired_price_id"].clone();
    assert!(ret.is_string(), "{paired:?}");
    assert_eq!(paired.1["effective_to"], "2031-03-20");
    assert_eq!(paired.1["closed_explicitly"], false);
    let r = stored(&f, &ret).await.unwrap();
    assert_eq!(r.version_no, 5, "the entry's next number");
    assert_eq!(r.version, 1);
    assert_eq!(r.state, "draft");
    assert_eq!(r.effective_from, date("2031-03-20"));
    assert_eq!(r.return_of_price_id, Some(back));
    assert_eq!(
        r.paired_price_id.map(|p| p.to_string()),
        promo.as_str().map(str::to_owned)
    );
    assert_eq!(r.created_by, f.ctx.subject_id());
    assert_eq!(r.price_json, json!({"rate":"0.20"}));
    assert_eq!(audited(&f, &ret).await, [("price.create".to_owned(), 1)]);

    // The end on the next start again: the return goes.
    let single = patch(&f, promo, json!({"temporary_until":"2031-04-01"}), "\"2\"").await;
    assert_eq!(single.0, 200, "{single:?}");
    assert!(single.1["paired_price_id"].is_null(), "{single:?}");
    assert_eq!(single.1["effective_to"], "2031-04-01");
    assert_eq!(single.1["closed_explicitly"], false);
    assert!(stored(&f, &ret).await.is_none(), "the return is deleted");
    assert_eq!(
        audited(&f, &ret).await,
        [
            ("price.create".to_owned(), 1),
            ("price.delete".to_owned(), 1)
        ]
    );
    let (status, receipt, _) = f
        .call(
            "POST",
            &format!("/prices/{}/submit", promo.as_str().unwrap()),
            json!({}),
            None,
            Some("submit"),
        )
        .await;
    assert_eq!(status, 201, "{receipt}");
}

/// Pair → one closed price → promo alone → pair: nothing of the chain in force on the new end
/// makes one explicitly closed price; an end on the next approved start makes the promo alone,
/// which that start ends; an end inside a closed price makes a return closed where it ends.
#[tokio::test]
async fn an_end_past_the_chain_closes_the_price_and_an_end_inside_it_pairs_it_again() {
    let (f, entry) = priced().await;
    let back = approved(&f, &entry, 1, "2031-01-01", Some("2031-05-01"), None).await;
    let pair = drafted(&f, &entry, "2031-03-01", Some("2031-03-11")).await;
    assert_eq!(pair.len(), 2);
    let (promo, ret) = (&pair[0]["id"], &pair[1]["id"]);
    assert_eq!(pair[1]["effective_to"], "2031-05-01");
    assert_eq!(pair[1]["closed_explicitly"], true);

    let closed = patch(&f, promo, json!({"temporary_until":"2031-05-10"}), "\"1\"").await;
    assert_eq!(closed.0, 200, "{closed:?}");
    assert!(closed.1["paired_price_id"].is_null());
    assert_eq!(closed.1["effective_to"], "2031-05-10");
    assert_eq!(closed.1["closed_explicitly"], true, "one closed price");
    assert!(stored(&f, ret).await.is_none());

    approved(&f, &entry, 7, "2031-06-01", None, None).await;
    let alone = patch(&f, promo, json!({"temporary_until":"2031-06-01"}), "\"2\"").await;
    assert_eq!(alone.0, 200, "{alone:?}");
    assert!(alone.1["paired_price_id"].is_null());
    assert_eq!(alone.1["effective_to"], "2031-06-01");
    assert_eq!(
        alone.1["closed_explicitly"], false,
        "the next start ends it"
    );

    let again = patch(&f, promo, json!({"temporary_until":"2031-04-01"}), "\"3\"").await;
    assert_eq!(again.0, 200, "{again:?}");
    let ret = again.1["paired_price_id"].clone();
    let r = stored(&f, &ret).await.unwrap();
    assert_eq!(r.effective_from, date("2031-04-01"));
    assert_eq!(r.effective_to, Some(date("2031-05-01")));
    assert!(r.closed_explicitly, "back to a closed price, until its end");
    assert_eq!(r.return_of_price_id, Some(back));
    assert_eq!(r.version_no, 8, "the entry's next: after the approved 7");
}

/// Both halves are judged as the create judges them (D-406, the window rules); a refusal writes
/// nothing.
#[tokio::test]
async fn new_dates_are_judged_as_the_create_judges_them() {
    let (f, entry) = priced().await;
    approved(&f, &entry, 1, "2031-01-01", None, None).await;
    approved(&f, &entry, 2, "2031-05-01", None, Some("2031-05-10")).await;
    approved(&f, &entry, 3, "2031-05-10", None, None).await;
    let pair = drafted(&f, &entry, "2031-03-01", Some("2031-03-11")).await;
    let (promo, ret) = (&pair[0]["id"], &pair[1]["id"]);
    for (body, code) in [
        (
            json!({"temporary_until":"2031-03-01"}),
            "WINDOW_END_INVALID",
        ),
        (
            json!({"temporary_until":"not-a-date"}),
            "WINDOW_END_INVALID",
        ),
        (json!({"effective_from":"2031-03-15"}), "WINDOW_END_INVALID"),
        (
            json!({"effective_from":"2020-01-01"}),
            "WINDOW_START_IN_PAST",
        ),
        (json!({"effective_from":"2031-01-01"}), "WINDOW_OVERLAP"),
        (
            json!({"effective_from":"2031-04-25","temporary_until":"2031-05-05"}),
            "TEMPORARY_SPANS_A_CHANGE",
        ),
        (
            json!({"temporary_until":"2031-06-01"}),
            "TEMPORARY_SPANS_A_CHANGE",
        ),
    ] {
        let refused = patch(&f, promo, body.clone(), "\"1\"").await;
        assert_eq!(refused.0, 400, "{body}: {refused:?}");
        assert!(code_in(&refused.1, code), "{body}: {refused:?}");
    }
    // A plain draft's start inside an approved temporary window is refused as today.
    let plain = &drafted(&f, &entry, "2031-09-01", None).await[0]["id"];
    let refused = patch(&f, plain, json!({"effective_from":"2031-05-03"}), "\"1\"").await;
    assert_eq!(refused.0, 400, "{refused:?}");
    assert!(code_in(&refused.1, "PRICE_INSIDE_TEMPORARY"), "{refused:?}");
    // Nothing moved.
    let (p, r) = (
        stored(&f, promo).await.unwrap(),
        stored(&f, ret).await.unwrap(),
    );
    assert_eq!((p.version, r.version), (1, 1));
    assert_eq!(p.temporary_until, Some(date("2031-03-11")));
    assert_eq!(r.effective_from, date("2031-03-11"));
}

/// A return's own dates stay fixed (edit the temporary half); a price's temporariness is fixed
/// too: no end on a price that is not temporary, and no `null` end on one that is.
#[tokio::test]
async fn a_returns_dates_and_a_prices_temporariness_stay_fixed() {
    let (f, entry) = priced().await;
    approved(&f, &entry, 1, "2031-01-01", None, None).await;
    let pair = drafted(&f, &entry, "2031-03-01", Some("2031-03-11")).await;
    let (promo, ret) = (&pair[0]["id"], &pair[1]["id"]);
    let plain = &drafted(&f, &entry, "2031-09-01", None).await[0]["id"];
    for (id, body, field) in [
        (
            ret,
            json!({"effective_from":"2031-03-12"}),
            "effective_from",
        ),
        (
            ret,
            json!({"temporary_until":"2031-03-20"}),
            "temporary_until",
        ),
        (
            plain,
            json!({"temporary_until":"2031-10-01"}),
            "temporary_until",
        ),
        (promo, json!({"temporary_until":null}), "temporary_until"),
        (promo, json!({"dim_value":"eu"}), "dim_value"),
    ] {
        let refused = patch(&f, id, body.clone(), "\"1\"").await;
        assert_eq!(refused.0, 400, "{body}: {refused:?}");
        assert!(
            code_in(&refused.1, "TEMPORARY_PRICE_FIXED"),
            "{body}: {refused:?}"
        );
        assert!(code_in(&refused.1, field), "{body}: {refused:?}");
    }
    // A return's unchanged start is no change.
    let same = patch(&f, ret, json!({"effective_from":"2031-03-11"}), "\"1\"").await;
    assert_eq!(same.0, 200, "{same:?}");
}

/// If-Match as today, the author's own draft pair only, and a pair with a half that is not an
/// unlocked draft is 409 `PRICE_NOT_DRAFT`.
#[tokio::test]
async fn a_date_patch_needs_if_match_its_author_and_a_draft_pair() {
    let (f, entry) = priced().await;
    approved(&f, &entry, 1, "2031-01-01", None, None).await;
    let pair = drafted(&f, &entry, "2031-03-01", Some("2031-03-11")).await;
    let (promo, ret) = (&pair[0]["id"], &pair[1]["id"]);
    let path = format!("/prices/{}", promo.as_str().unwrap());
    let body = json!({"temporary_until":"2031-03-20"});
    let missing = f.call("PATCH", &path, body.clone(), None, None).await;
    assert_eq!(missing.0, 400, "{missing:?}");
    let stale = patch(&f, promo, body.clone(), "\"7\"").await;
    assert_eq!(stale.0, 409, "{stale:?}");
    assert!(code_in(&stale.1, "STALE_REVISION"), "{stale:?}");
    let other = f
        .call_as(&f.user(), "PATCH", &path, body.clone(), Some("\"1\""), None)
        .await;
    assert_eq!(other.0, 403, "{other:?}");
    assert!(code_in(&other.1, "NOT_DRAFT_AUTHOR"), "{other:?}");
    // The partner is no longer a draft: the pair cannot move.
    let conn = Database::connect(&f.dsn).await.unwrap();
    conn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "UPDATE pricing_price SET state = 'rejected' WHERE id = ?",
        [ret.as_str().unwrap().parse::<Uuid>().unwrap().into()],
    ))
    .await
    .unwrap();
    let refused = patch(&f, promo, body.clone(), "\"1\"").await;
    assert_eq!(refused.0, 409, "{refused:?}");
    assert!(code_in(&refused.1, "PRICE_NOT_DRAFT"), "{refused:?}");
    // A submitted pair is pending: 409 as today.
    let (f, entry) = priced().await;
    approved(&f, &entry, 1, "2031-01-01", None, None).await;
    let pair = drafted(&f, &entry, "2031-03-01", Some("2031-03-11")).await;
    let promo = &pair[0]["id"];
    let (status, receipt, _) = publish_pair(&f, &entry, promo).await;
    assert_eq!(status, 201, "{receipt}");
    let refused = patch(&f, promo, body, "\"1\"").await;
    assert_eq!(refused.0, 409, "{refused:?}");
    assert!(code_in(&refused.1, "PRICE_NOT_DRAFT"), "{refused:?}");
}
