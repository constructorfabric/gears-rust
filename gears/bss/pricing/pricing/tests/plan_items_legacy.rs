//! D-467 (owner, 2026-09-30): a plan item is a SKU and its entry in the plan's book. The columns
//! `treatment`, `included_qty` and `qty_min` stay, and the deployed database holds rows written before D-467
//! (the deployed environment, measured 2026-09-30): three included items without an entry (in a draft, in a
//! pending revision and in a published one), two paid items with a `qty_min` (pending and
//! published), and two pending `plan_revision` units whose content carries the old fields. These
//! tests seed the same shapes through the repositories and the approval store, as the doors of
//! before D-467 wrote them, and read, resolve, copy, check, approve and reject them.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
use bss_approval::{ItemRef, Store, Unit, UnitState, hash::snapshot_hash};
use bss_pricing::infra::storage::{
    RepoError,
    entity::plan_item,
    repo::{
        approval_repo::PricingApprovalStore, plan_item_repo, plan_revision_repo,
        price_book_entry_repo, price_repo,
    },
};
use bss_products_sdk::models::SkuType;
use plan_support::{
    Catalog, Fixture, book, id_of, items, plan, policy_entry as entry, publish, scope, setup, text,
};
use serde_json::{Value, json};
use uuid::Uuid;

const REMOVED: [&str; 3] = ["treatment", "included_qty", "qty_min"];

/// An approved price of `entry` from 2020, written directly: the entry covers any sale date.
async fn approved(f: &Fixture, entry: Uuid) {
    let conn = f.db.conn().unwrap();
    let e = price_book_entry_repo::find(&conn, &scope(f), f.ctx.subject_tenant_id(), entry)
        .await
        .unwrap()
        .unwrap();
    let mut p = plan_support::entry_support::price(&e);
    p.state = "approved".into();
    p.effective_from = time::macros::date!(2020 - 01 - 01);
    price_repo::insert(&conn, &scope(f), p).await.unwrap();
}
/// A row as the doors of before D-467 wrote it, straight into `revision`.
async fn legacy(
    f: &Fixture,
    revision: Uuid,
    sku: Uuid,
    entry: Option<Uuid>,
    treatment: &str,
    quantities: (Option<&str>, Option<i32>),
) -> plan_item::Model {
    let now = time::OffsetDateTime::now_utc();
    plan_item_repo::insert_as_given(
        &f.db.conn().unwrap(),
        &scope(f),
        plan_item::Model {
            id: Uuid::now_v7(),
            tenant_id: f.ctx.subject_tenant_id(),
            revision_id: revision,
            sku_id: sku,
            price_book_entry_id: entry,
            treatment: treatment.into(),
            included_qty: quantities.0.map(str::to_owned),
            qty_min: quantities.1,
            reservation_id: Some(Uuid::new_v4()),
            reference_state: "confirmed".into(),
            version: 1,
            created_by: f.ctx.subject_id(),
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap()
}
/// The deployed database's world: a book with a priced recurring SKU and a priced usage SKU, and a usage SKU
/// with no entry, which only a legacy included item names.
struct World {
    book: Uuid,
    seats: (Uuid, Uuid),
    storage: (Uuid, Uuid),
    backup: Uuid,
}
async fn world(f: &Fixture, catalog: &Catalog) -> World {
    let book = book(f, "eur").await;
    let seats = catalog.sku(SkuType::Recurring);
    let seats_entry = entry(f, book, seats, "recurring", Some("month")).await;
    approved(f, seats_entry).await;
    let storage = catalog.sku(SkuType::Usage);
    let storage_entry = entry(f, book, storage, "usage", None).await;
    approved(f, storage_entry).await;
    World {
        book,
        seats: (seats, seats_entry),
        storage: (storage, storage_entry),
        backup: catalog.sku(SkuType::Usage),
    }
}
/// A draft holding the deployed database's legacy shapes: a paid item with `qty_min` 2 and an included item
/// with 10 units and no entry; `(plan id, revision id, the paid item, the included item)`.
async fn legacy_draft(
    f: &Fixture,
    w: &World,
    code: &str,
) -> (Uuid, Uuid, plan_item::Model, plan_item::Model) {
    let (p, rev) = plan(f, code, w.book).await;
    let paid = legacy(f, rev, w.seats.0, Some(w.seats.1), "paid", (None, Some(2))).await;
    let included = legacy(f, rev, w.backup, None, "included", (Some("10"), None)).await;
    (id_of(&p["id"]), rev, paid, included)
}
async fn get(f: &Fixture, path: &str) -> Value {
    let (s, b, _) = f.call("GET", path, json!({}), None, None).await;
    assert_eq!(s, 200, "{path}: {b}");
    b
}
fn no_removed_key(item: &Value) {
    for key in REMOVED {
        assert!(item.get(key).is_none(), "D-467, no {key}: {item}");
    }
}
fn row<'a>(checks: &'a Value, code: &str) -> &'a Value {
    checks["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == code)
        .unwrap()
}
fn red(checks: &Value) -> Vec<String> {
    checks["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["ok"] == false)
        .map(|c| c["code"].as_str().unwrap().to_owned())
        .collect()
}
/// A revision's business content as the subject of before D-467 fingerprinted it: each item with
/// its treatment and quantities, in SKU order.
fn legacy_content(book: Uuid, rows: &[plan_item::Model]) -> Value {
    let mut rows: Vec<&plan_item::Model> = rows.iter().collect();
    rows.sort_by_key(|i| i.sku_id);
    json!({
        "book_id": book,
        "available_from": null,
        "items": rows.iter().map(|i| json!({
            "sku_id": i.sku_id,
            "price_book_entry_id": i.price_book_entry_id,
            "treatment": i.treatment,
            "included_qty": i.included_qty,
            "qty_min": i.qty_min,
        })).collect::<Vec<_>>(),
    })
}
/// A pending `plan_revision` unit of quorum 1 submitted before D-467 by the revision's author, as
/// that submit wrote it: the unit, its one item whose `after` is the old content, the snapshot
/// and its fingerprint, and the revision's lock.
async fn legacy_pending(f: &Fixture, book: Uuid, revision: Uuid) -> Uuid {
    let rows = items(f, revision).await;
    let after = legacy_content(book, &rows);
    let refs = vec![ItemRef {
        item_type: "plan_revision".into(),
        item_id: revision,
        created_by: f.ctx.subject_id(),
        before: None,
        after: after.clone(),
    }];
    let (id, tenant, scope) = (Uuid::new_v4(), f.ctx.subject_tenant_id(), scope(f));
    let unit = Unit {
        id,
        tenant_id: tenant,
        ref_type: "plan_revision".into(),
        kind: "plan_revision".into(),
        ref_id: revision,
        state: UnitState::Pending,
        common_effective_date: None,
        quorum_required: 1,
        generation: 1,
        submitted_by: f.ctx.subject_id(),
        submitted_at: time::OffsetDateTime::now_utc(),
        submit_note: None,
        decided_at: None,
        decided_note: None,
        snapshot: json!({"revision_id": revision, "rev_no": 1, "before": null, "after": after}),
        snapshot_hash: snapshot_hash(&refs, None),
        version: 1,
    };
    price_repo::transaction(&f.db.db(), move |tx| {
        let (scope, unit, refs) = (scope.clone(), unit.clone(), refs.clone());
        Box::pin(async move {
            PricingApprovalStore {
                scope,
                tenant_id: tenant,
            }
            .insert_unit(tx, &unit, &refs)
            .await
            .map_err(|e| RepoError::Db(e.to_string()))
        })
    })
    .await
    .unwrap();
    let conn = f.db.conn().unwrap();
    let version = plan_revision_repo::find(&conn, &plan_support::scope(f), tenant, revision)
        .await
        .unwrap()
        .unwrap()
        .version;
    assert!(
        plan_revision_repo::try_lock(
            &conn,
            &plan_support::scope(f),
            tenant,
            revision,
            id,
            version
        )
        .await
        .unwrap()
    );
    id
}
async fn vote(f: &Fixture, unit: Uuid, action: &str, body: Value, key: &str) -> (u16, Value) {
    let (s, b, _) = f
        .call_as(
            &f.user(),
            "POST",
            &format!("/approval-units/{unit}/{action}"),
            body,
            None,
            Some(key),
        )
        .await;
    (s, b)
}

/// The published legacy revision (a paid item with `qty_min`, an included item without an entry)
/// reads without the three fields and still resolves; its copy into a new draft keeps the
/// included item, still without an entry, so the draft's checks name it `ITEM_ENTRY_MISSING`,
/// and the copy of the paid item is written `paid` with no quantity. Giving the included copy an
/// entry makes it an ordinary item, and the draft green.
#[tokio::test]
async fn a_published_legacy_revision_reads_resolves_and_copies_its_included_item_as_missing() {
    let (f, catalog) = setup().await;
    let w = world(&f, &catalog).await;
    let (plan_id, rev1, paid, included) = legacy_draft(&f, &w, "legacy").await;
    publish(&f, plan_id, rev1).await;
    let read = get(&f, &format!("/plan-revisions/{rev1}")).await;
    assert_eq!(read["state"], "published");
    for it in read["items"].as_array().unwrap() {
        no_removed_key(it);
    }
    let answered = get(&f, &format!("/plan-items/{}", included.id)).await;
    no_removed_key(&answered);
    assert_eq!(answered["price_book_entry_id"], json!(null));
    let today = time::OffsetDateTime::now_utc().date();
    let resolved = get(
        &f,
        &format!("/resolve?plan_revision_id={rev1}&date={today}"),
    )
    .await;
    let resolved_items = resolved["items"].as_array().unwrap();
    assert_eq!(resolved_items.len(), 2, "{resolved}");
    for it in resolved_items {
        no_removed_key(it);
    }
    let by_id = |id: Uuid| {
        resolved_items
            .iter()
            .find(|i| i["item_id"] == id.to_string())
            .unwrap()
    };
    assert_eq!(by_id(included.id)["price_book_entry_id"], json!(null));
    assert_eq!(
        by_id(included.id)["chains"],
        json!([]),
        "no entry, no chains"
    );
    assert_eq!(by_id(paid.id)["chains"][0]["uncovered"], false);
    let (s, copied, _) = f
        .call(
            "POST",
            &format!("/plans/{plan_id}/revisions"),
            json!({}),
            None,
            Some("copy"),
        )
        .await;
    assert_eq!(s, 201, "{copied}");
    let rev2 = id_of(&copied["id"]);
    let stored = items(&f, rev2).await;
    let copy_of = |sku: Uuid| stored.iter().find(|i| i.sku_id == sku).unwrap().clone();
    let (paid_copy, included_copy) = (copy_of(w.seats.0), copy_of(w.backup));
    assert_eq!(
        (
            paid_copy.treatment.as_str(),
            paid_copy.included_qty,
            paid_copy.qty_min
        ),
        ("paid", None, None),
        "a copy is a new row"
    );
    assert_eq!(
        (
            included_copy.treatment.as_str(),
            included_copy.price_book_entry_id,
            included_copy.included_qty,
        ),
        ("included", None, None),
        "the one entry-less row the column's CHECK admits"
    );
    let checks = get(&f, &format!("/plan-revisions/{rev2}/checks")).await;
    assert_eq!(
        red(&checks),
        vec!["ITEM_ENTRY_MISSING".to_owned()],
        "{checks}"
    );
    assert_eq!(
        row(&checks, "ITEM_ENTRY_MISSING")["subjects"],
        json!([{"item_id":included_copy.id,"sku_id":w.backup,"price_book_entry_id":null}])
    );
    let backup_entry = entry(&f, w.book, w.backup, "usage", None).await;
    approved(&f, backup_entry).await;
    let (s, b, _) = f
        .call(
            "PATCH",
            &format!("/plan-items/{}", included_copy.id),
            json!({"price_book_entry_id":backup_entry}),
            Some(&format!("\"{}\"", included_copy.version)),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    no_removed_key(&b);
    let given = copy_of_row(&f, rev2, w.backup).await;
    assert_eq!(
        (
            given.treatment.as_str(),
            given.price_book_entry_id,
            given.included_qty
        ),
        ("paid", Some(backup_entry), None),
        "a PATCH writes the row in the new shape"
    );
    let checks = get(&f, &format!("/plan-revisions/{rev2}/checks")).await;
    assert_eq!(checks["ready"], true, "{checks}");
    let source = items(&f, rev1).await;
    assert!(
        source
            .iter()
            .any(|i| i.treatment == "included" && i.included_qty.as_deref() == Some("10")),
        "published history is not rewritten: {source:?}"
    );
    assert!(source.iter().any(|i| i.qty_min == Some(2)), "{source:?}");
}
/// D-467: a revision PATCH that changes the plan's book points each item at the new book's twin
/// entry and writes the row in the shape of D-467, as the item PATCH does: a legacy paid item
/// with a `qty_min` and a legacy optional item become `paid` with no quantity. An item the remap
/// does not move (the included item without an entry has no twin) is not written.
#[tokio::test]
async fn a_book_remap_writes_each_moved_legacy_item_as_a_sku_and_its_entry() {
    let (f, catalog) = setup().await;
    let w = world(&f, &catalog).await;
    let (_, rev, paid, included) = legacy_draft(&f, &w, "remap").await;
    let optional = legacy(
        &f,
        rev,
        w.storage.0,
        Some(w.storage.1),
        "optional",
        (None, None),
    )
    .await;
    let other = book(&f, "other").await;
    let seats_twin = entry(&f, other, w.seats.0, "recurring", Some("month")).await;
    let storage_twin = entry(&f, other, w.storage.0, "usage", None).await;
    let path = format!("/plan-revisions/{rev}");
    let (_, _, tag) = f.call("GET", &path, json!({}), None, None).await;
    let (s, b, _) = f
        .call("PATCH", &path, json!({"book_id":other}), Some(&tag), None)
        .await;
    assert_eq!(s, 200, "{b}");
    let rows = items(&f, rev).await;
    let shape = |id: Uuid| {
        let item = rows.iter().find(|i| i.id == id).unwrap();
        (
            item.price_book_entry_id,
            item.treatment.clone(),
            item.included_qty.clone(),
            item.qty_min,
        )
    };
    assert_eq!(
        shape(paid.id),
        (Some(seats_twin), "paid".to_owned(), None, None),
        "the paid item with a qty_min"
    );
    assert_eq!(
        shape(optional.id),
        (Some(storage_twin), "paid".to_owned(), None, None),
        "the optional item"
    );
    let kept = rows.iter().find(|i| i.id == included.id).unwrap();
    assert_eq!(kept, &included, "the item without a twin is not written");
}
async fn copy_of_row(f: &Fixture, revision: Uuid, sku: Uuid) -> plan_item::Model {
    items(f, revision)
        .await
        .into_iter()
        .find(|i| i.sku_id == sku)
        .unwrap()
}

/// A draft holding the deployed database's legacy shapes reads without the fields; its included item is
/// `ITEM_ENTRY_MISSING`, the only red check, so the submit is refused; a PATCH that leaves it
/// without an entry is refused too, and once its author removes it the draft submits.
#[tokio::test]
async fn a_legacy_included_item_in_a_draft_is_item_entry_missing_until_its_author_removes_it() {
    let (f, catalog) = setup().await;
    let w = world(&f, &catalog).await;
    let (_, rev, paid, included) = legacy_draft(&f, &w, "draft").await;
    let read = get(&f, &format!("/plan-revisions/{rev}")).await;
    assert_eq!(read["items"].as_array().unwrap().len(), 2);
    for it in read["items"].as_array().unwrap() {
        no_removed_key(it);
    }
    let checks = get(&f, &format!("/plan-revisions/{rev}/checks")).await;
    assert_eq!(
        red(&checks),
        vec!["ITEM_ENTRY_MISSING".to_owned()],
        "{checks}"
    );
    assert_eq!(
        row(&checks, "ITEM_ENTRY_MISSING")["subjects"],
        json!([{"item_id":included.id,"sku_id":w.backup,"price_book_entry_id":null}])
    );
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/plan-revisions/{rev}/submit"),
            json!({}),
            None,
            Some("refused"),
        )
        .await;
    assert_eq!(s, 400, "{b}");
    assert!(text(&b).contains("REVISION_CHECKS_RED"), "{b}");
    let path = format!("/plan-items/{}", included.id);
    let (s, b, _) = f.call("PATCH", &path, json!({}), Some("\"1\""), None).await;
    assert_eq!(s, 400, "{b}");
    assert!(text(&b).contains("ITEM_ENTRY_MISSING"), "{b}");
    let (s, b, _) = f.call("DELETE", &path, json!({}), None, None).await;
    assert_eq!(s, 204, "{b}");
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/plan-revisions/{rev}/submit"),
            json!({}),
            None,
            Some("submitted"),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    let unit = &b["unit"];
    assert_eq!(
        unit["snapshot"]["after"]["items"],
        json!([{"sku_id":paid.sku_id,"price_book_entry_id":paid.price_book_entry_id}]),
        "D-511 preserves policy-less pre-seam approval content"
    );
}

/// Measured (D-467): a pending unit submitted before D-467, whose revision holds a paid item with
/// `qty_min`, fingerprinted the old content. Its first approve finds the content changed and
/// refreshes it once, at generation 2, recording no vote (400 `UNIT_STALE`); the approve of
/// generation 2 applies, and the published revision resolves without the fields.
#[tokio::test]
async fn a_pending_unit_of_before_d467_refreshes_once_and_its_next_approve_applies() {
    let (f, catalog) = setup().await;
    let w = world(&f, &catalog).await;
    let (created, rev) = plan(&f, "qty-min", w.book).await;
    legacy(&f, rev, w.seats.0, Some(w.seats.1), "paid", (None, Some(2))).await;
    legacy(
        &f,
        rev,
        w.storage.0,
        Some(w.storage.1),
        "paid",
        (None, None),
    )
    .await;
    let unit = legacy_pending(&f, w.book, rev).await;
    let card = get(&f, &format!("/approval-units/{unit}")).await;
    assert_eq!(
        card["snapshot"]["after"]["items"][0]["treatment"], "paid",
        "a stored snapshot stays history until its refresh: {card}"
    );
    let (s, b) = vote(&f, unit, "approve", json!({"generation":1}), "first").await;
    assert_eq!(s, 400, "{b}");
    assert!(text(&b).contains("UNIT_STALE"), "{b}");
    assert_eq!(b["context"]["generation"], 2);
    let card = get(&f, &format!("/approval-units/{unit}")).await;
    assert_eq!(
        (card["state"].as_str(), &card["generation"]),
        (Some("pending"), &json!(2))
    );
    assert_eq!(card["decisions"], json!([]), "the refresh records no vote");
    for it in card["snapshot"]["after"]["items"].as_array().unwrap() {
        no_removed_key(it);
    }
    let (s, b) = vote(&f, unit, "approve", json!({"generation":2}), "second").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["outcome"], "applied");
    let read = get(&f, &format!("/plan-revisions/{rev}")).await;
    assert_eq!(read["state"], "published");
    let today = time::OffsetDateTime::now_utc().date();
    let resolved = get(&f, &format!("/resolve?plan_revision_id={rev}&date={today}")).await;
    for it in resolved["items"].as_array().unwrap() {
        no_removed_key(it);
    }
    assert_eq!(
        get(&f, &format!("/plans/{}", created["id"].as_str().unwrap())).await["published_rev"],
        1
    );
}

/// Measured (D-467): a pending unit submitted before D-467 whose revision holds an included item
/// without an entry refreshes once on its first approve; the approve of generation 2 then meets
/// the apply's checks, where the included item is `ITEM_ENTRY_MISSING`: 409 `APPLY_REFUSED`, nothing
/// published. The unit is never stuck: a reject of generation 2 returns the revision to a draft,
/// whose author removes the item or gives it an entry.
#[tokio::test]
async fn a_pending_unit_with_a_legacy_included_item_is_refused_at_apply_and_can_be_rejected() {
    let (f, catalog) = setup().await;
    let w = world(&f, &catalog).await;
    let (_, rev, _, included) = legacy_draft(&f, &w, "included").await;
    let unit = legacy_pending(&f, w.book, rev).await;
    let (s, b) = vote(&f, unit, "approve", json!({"generation":1}), "first").await;
    assert_eq!(s, 400, "{b}");
    assert!(text(&b).contains("UNIT_STALE"), "{b}");
    let (s, b) = vote(&f, unit, "approve", json!({"generation":2}), "second").await;
    assert_eq!(s, 409, "{b}");
    assert!(text(&b).contains("APPLY_REFUSED"), "{b}");
    assert!(text(&b).contains("ITEM_ENTRY_MISSING"), "{b}");
    let read = get(&f, &format!("/plan-revisions/{rev}")).await;
    assert_eq!(read["state"], "pending", "nothing was published");
    let (s, b) = vote(
        &f,
        unit,
        "reject",
        json!({"generation":2,"note":"the included item has no price"}),
        "reject",
    )
    .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["outcome"], "rejected");
    let read = get(&f, &format!("/plan-revisions/{rev}")).await;
    assert_eq!(read["state"], "draft");
    let checks = get(&f, &format!("/plan-revisions/{rev}/checks")).await;
    assert_eq!(
        row(&checks, "ITEM_ENTRY_MISSING")["subjects"][0]["item_id"],
        json!(included.id)
    );
}

/// A reject of a pending unit of before D-467 meets the same one-off refresh as an approve, and
/// the reject of the new generation closes it.
#[tokio::test]
async fn a_reject_of_a_pending_unit_of_before_d467_refreshes_once_then_rejects() {
    let (f, catalog) = setup().await;
    let w = world(&f, &catalog).await;
    let (_, rev, _, _) = legacy_draft(&f, &w, "reject").await;
    let unit = legacy_pending(&f, w.book, rev).await;
    let note = json!({"generation":1,"note":"no"});
    let (s, b) = vote(&f, unit, "reject", note, "first").await;
    assert_eq!(s, 400, "{b}");
    assert!(text(&b).contains("UNIT_STALE"), "{b}");
    let (s, b) = vote(
        &f,
        unit,
        "reject",
        json!({"generation":2,"note":"no"}),
        "second",
    )
    .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["outcome"], "rejected");
    assert_eq!(
        get(&f, &format!("/plan-revisions/{rev}")).await["state"],
        "draft"
    );
}
