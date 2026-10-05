//! Plans and revisions through the production router (run 3.3, Task 3.3.1): create with a
//! draft rev 1, rename, copy of the published revision under D-413, the revision PATCH with its
//! book remap, the draft delete with a delete op per item (D-414), and the draft ownership of
//! D-404.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
use bss_products_sdk::models::{ReferenceKind, SkuType};
use plan_support::entry_support::policy_support;
use plan_support::{
    book, entry, holding, id_of, item, items, lock, ops_for, plan, publish, raw, request, setup,
    stranger, text,
};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn a_plan_is_created_with_a_draft_revision_one_on_its_book_and_its_key_replays() {
    let (f, _) = setup().await;
    let eur = book(&f, "eur").await;
    let body = json!({"code":"PRO","name":"Pro","book_id":eur});
    assert_eq!(
        f.call("POST", "/plans", body.clone(), None, None).await.0,
        400,
        "an Idempotency-Key is required"
    );
    let created = f
        .call("POST", "/plans", body.clone(), None, Some("one"))
        .await;
    assert_eq!(created.0, 201, "{created:?}");
    assert_eq!(created.2, "\"1\"");
    let p = &created.1;
    assert_eq!(p["code"], "PRO");
    assert_eq!(p["name"], "Pro");
    assert_eq!(p["published_rev"], json!(null));
    assert_eq!(p["created_by"], f.ctx.subject_id().to_string());
    let revisions = p["revisions"].as_array().unwrap();
    assert_eq!(revisions.len(), 1, "{p}");
    assert_eq!(revisions[0]["rev_no"], 1);
    assert_eq!(revisions[0]["state"], "draft");
    assert_eq!(revisions[0]["book_id"], eur.to_string());
    assert_eq!(
        f.call("POST", "/plans", body.clone(), None, Some("one"))
            .await,
        created,
        "the key replays"
    );
    let other = f
        .call(
            "POST",
            "/plans",
            json!({"code":"PRO2","name":"Pro","book_id":eur}),
            None,
            Some("one"),
        )
        .await;
    assert_eq!(other.0, 409, "{other:?}");
    assert!(text(&other.1).contains("IDEMPOTENCY_CONFLICT"), "{other:?}");
    let id = p["id"].as_str().unwrap();
    let read = f
        .call("GET", &format!("/plans/{id}"), json!({}), None, None)
        .await;
    assert_eq!((read.0, &read.1, read.2.as_str()), (200, p, "\"1\""));
    let (status, list, _) = f.call("GET", "/plans", json!({}), None, None).await;
    assert_eq!(status, 200, "{list}");
    assert_eq!(list["items"], json!([p]));
    let rev = revisions[0]["id"].as_str().unwrap();
    let (status, r, tag) = f
        .call(
            "GET",
            &format!("/plan-revisions/{rev}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(tag, "\"1\"");
    assert_eq!(r["plan_id"], id);
    assert_eq!(r["rev_no"], 1);
    assert_eq!(r["state"], "draft");
    assert_eq!(r["book_id"], eur.to_string());
    assert_eq!(r["available_from"], json!(null));
    assert_eq!(r["created_by"], f.ctx.subject_id().to_string());
    assert_eq!(r["items"], json!([]));
}

#[tokio::test]
async fn a_plan_needs_a_code_of_its_own_and_a_book_of_its_tenant() {
    let (f, _) = setup().await;
    let eur = book(&f, "eur").await;
    let (s, b, _) = f
        .call(
            "POST",
            "/plans",
            json!({"code":"  ","name":"Pro","book_id":eur}),
            None,
            Some("blank"),
        )
        .await;
    assert_eq!(s, 400, "{b}");
    assert!(text(&b).contains("PLAN_CODE_REQUIRED"), "{b}");
    let (s, b, _) = f
        .call(
            "POST",
            "/plans",
            json!({"code":"PRO","name":"Pro","book_id":Uuid::new_v4()}),
            None,
            Some("nobook"),
        )
        .await;
    assert_eq!(s, 404, "{b}");
    plan(&f, "pro", eur).await;
    let (s, b, _) = f
        .call(
            "POST",
            "/plans",
            json!({"code":"PRO","name":"Again","book_id":eur}),
            None,
            Some("again"),
        )
        .await;
    assert_eq!(s, 409, "{b}");
    assert!(text(&b).contains("PLAN_CODE_TAKEN"), "{b}");
    let (_, list, _) = f.call("GET", "/plans", json!({}), None, None).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
}

#[tokio::test]
async fn a_plan_is_renamed_under_if_match() {
    let (f, _) = setup().await;
    let (p, _) = plan(&f, "pro", book(&f, "eur").await).await;
    let path = format!("/plans/{}", p["id"].as_str().unwrap());
    let rename = json!({"name":"Professional"});
    assert_eq!(
        f.call("PATCH", &path, rename.clone(), None, None).await.0,
        400,
        "If-Match is required"
    );
    let stale = f
        .call("PATCH", &path, rename.clone(), Some("\"7\""), None)
        .await;
    assert_eq!(stale.0, 409, "{stale:?}");
    assert!(text(&stale.1).contains("STALE_REVISION"), "{stale:?}");
    let (s, b, tag) = f.call("PATCH", &path, rename, Some("\"1\""), None).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(tag, "\"2\"");
    assert_eq!(b["name"], "Professional");
    assert_eq!(b["code"], "PRO");
    let unknown = f
        .call(
            "PATCH",
            &format!("/plans/{}", Uuid::new_v4()),
            json!({"name":"x"}),
            Some("\"1\""),
            None,
        )
        .await;
    assert_eq!(unknown.0, 404, "{unknown:?}");
}

#[tokio::test]
async fn a_copy_carries_the_published_book_availability_and_items_and_attaches_them() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let (p, rev1) = plan(&f, "pro", eur).await;
    let plan_id = id_of(&p["id"]);
    let (seats, storage) = (catalog.sku(SkuType::Recurring), catalog.sku(SkuType::Usage));
    let seats_entry = entry(&f, eur, seats, "recurring", Some("month")).await;
    let (s, _, _) = f
        .call(
            "PATCH",
            &format!("/plan-revisions/{rev1}"),
            json!({"available_from":"2031-03-01"}),
            Some("\"1\""),
            None,
        )
        .await;
    assert_eq!(s, 200);
    let paid = item(&f, rev1, seats, Some(seats_entry), "paid").await;
    let free = item(&f, rev1, storage, None, "included").await;
    publish(&f, plan_id, rev1).await;
    let before = items(&f, rev1).await;
    let path = format!("/plans/{plan_id}/revisions");
    assert_eq!(
        f.call("POST", &path, json!({}), None, None).await.0,
        400,
        "an Idempotency-Key is required"
    );
    let copied = f.call("POST", &path, json!({}), None, Some("copy")).await;
    assert_eq!(copied.0, 201, "{copied:?}");
    assert_eq!(copied.2, "\"1\"");
    let r = &copied.1;
    assert_eq!(r["rev_no"], 2);
    assert_eq!(r["state"], "draft");
    assert_eq!(r["book_id"], eur.to_string());
    assert_eq!(r["available_from"], "2031-03-01");
    assert_eq!(r["created_by"], f.ctx.subject_id().to_string());
    let answered = r["items"].as_array().unwrap();
    assert_eq!(answered.len(), 2, "{r}");
    for (copy, source) in answered.iter().zip([&paid, &free]) {
        assert_ne!(copy["id"], source.id.to_string(), "a copy has its own id");
        assert_eq!(copy["sku_id"], source.sku_id.to_string());
        assert_eq!(
            copy["price_book_entry_id"],
            json!(source.price_book_entry_id.map(|e| e.to_string()))
        );
        assert!(copy.get("treatment").is_none(), "D-467: {copy}");
        assert_eq!(
            copy["reference_state"], "unreserved",
            "D-413: written unreserved"
        );
        assert_eq!(copy["reservation_id"], json!(null));
    }
    // D-467: a copy is a new row, `paid` and no quantity; the legacy included item, stored
    // without an entry, stays one (its draft's checks show it ITEM_ENTRY_MISSING).
    let stored: Vec<_> = items(&f, id_of(&r["id"]))
        .await
        .into_iter()
        .map(|i| (i.sku_id, i.treatment, i.included_qty, i.qty_min))
        .collect();
    assert!(
        stored.contains(&(paid.sku_id, "paid".to_owned(), None, None)),
        "{stored:?}"
    );
    assert!(
        stored.contains(&(free.sku_id, "included".to_owned(), None, None)),
        "{stored:?}"
    );
    assert_eq!(
        f.call("POST", &path, json!({}), None, Some("copy")).await,
        copied,
        "the key replays the recorded answer"
    );
    // The door drove one attach op per item: each item holds its own confirmed receipt.
    assert_eq!(
        *catalog.reserve_kinds.lock().unwrap(),
        [ReferenceKind::PlanItem, ReferenceKind::PlanItem]
    );
    let rev2 = id_of(&r["id"]);
    let (s, read, _) = f
        .call(
            "GET",
            &format!("/plan-revisions/{rev2}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{read}");
    for it in read["items"].as_array().unwrap() {
        assert_eq!(it["reference_state"], "confirmed", "{it}");
        assert!(it["reservation_id"].is_string(), "{it}");
        let ops = ops_for(&f, id_of(&it["id"])).await;
        assert_eq!(ops.len(), 1, "one attach op per item");
        assert_eq!(
            (ops[0].kind.as_str(), ops[0].state.as_str()),
            ("attach", "done")
        );
        assert_eq!(ops[0].ref_kind, "plan_item");
    }
    assert_eq!(
        items(&f, rev1).await,
        before,
        "the source revision is untouched"
    );
    let again = f.call("POST", &path, json!({}), None, Some("again")).await;
    assert_eq!(again.0, 409, "{again:?}");
    assert!(
        text(&again.1).contains("REVISION_DRAFT_EXISTS"),
        "{again:?}"
    );
    let (_, plan_now, _) = f
        .call("GET", &format!("/plans/{plan_id}"), json!({}), None, None)
        .await;
    assert_eq!(plan_now["published_rev"], 1);
    assert_eq!(plan_now["revisions"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_copy_needs_a_published_revision_and_no_open_one() {
    let (f, _) = setup().await;
    let eur = book(&f, "eur").await;
    let (p, rev1) = plan(&f, "pro", eur).await;
    let path = format!("/plans/{}/revisions", p["id"].as_str().unwrap());
    let draft = f.call("POST", &path, json!({}), None, Some("draft")).await;
    assert_eq!(draft.0, 409, "{draft:?}");
    assert!(
        text(&draft.1).contains("REVISION_DRAFT_EXISTS"),
        "{draft:?}"
    );
    lock(&f, rev1).await;
    let pending = f
        .call("POST", &path, json!({}), None, Some("pending"))
        .await;
    assert_eq!(pending.0, 409, "a pending revision is open: {pending:?}");
    assert!(
        text(&pending.1).contains("REVISION_DRAFT_EXISTS"),
        "{pending:?}"
    );
    // D-417: a plan that loses its only, never-published revision is deleted with it, so no door
    // can reach a plan without a revision; the store still refuses a copy of one.
    let basic = bare_plan(&f, "basic").await;
    let none = f
        .call(
            "POST",
            &format!("/plans/{basic}/revisions"),
            json!({}),
            None,
            Some("none"),
        )
        .await;
    assert_eq!(none.0, 409, "{none:?}");
    assert!(text(&none.1).contains("PLAN_UNPUBLISHED"), "{none:?}");
    let unknown = f
        .call(
            "POST",
            &format!("/plans/{}/revisions", Uuid::new_v4()),
            json!({}),
            None,
            Some("unknown"),
        )
        .await;
    assert_eq!(unknown.0, 404, "{unknown:?}");
    let body = f
        .call("POST", &path, json!({"x":1}), None, Some("body"))
        .await;
    assert_eq!(body.0, 400, "the copy takes no fields: {body:?}");
}

// D-413 probe: the copy writes its revision, every item and every attach op in ONE transaction.
// A trigger refuses the second copied item: nothing of the copy may survive, and the key is free.
#[tokio::test]
async fn the_copy_writes_the_revision_and_every_item_in_one_transaction() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let (p, rev1) = plan(&f, "pro", eur).await;
    let plan_id = id_of(&p["id"]);
    item(&f, rev1, catalog.sku(SkuType::Usage), None, "included").await;
    item(&f, rev1, catalog.sku(SkuType::Usage), None, "included").await;
    publish(&f, plan_id, rev1).await;
    raw(
        &f,
        &format!(
            "CREATE TRIGGER second_copy BEFORE INSERT ON pricing_plan_item \
             WHEN (SELECT count(*) FROM pricing_plan_item WHERE revision_id = NEW.revision_id) \
             >= 1 BEGIN SELECT RAISE(ABORT, 'probe {rev1}'); END"
        ),
    )
    .await;
    let path = format!("/plans/{plan_id}/revisions");
    let failed = f.call("POST", &path, json!({}), None, Some("copy")).await;
    assert_eq!(failed.0, 500, "{failed:?}");
    let (_, now, _) = f
        .call("GET", &format!("/plans/{plan_id}"), json!({}), None, None)
        .await;
    assert_eq!(
        now["revisions"].as_array().unwrap().len(),
        1,
        "no revision of the failed copy survives: {now}"
    );
    let (_, all_ops, _) = f.call("GET", "/reference-ops", json!({}), None, None).await;
    assert_eq!(all_ops["items"], json!([]), "no attach op survives");
    assert_eq!(catalog.reserves(), 0);
    raw(&f, "DROP TRIGGER second_copy").await;
    let retried = f.call("POST", &path, json!({}), None, Some("copy")).await;
    assert_eq!(retried.0, 201, "the key was never claimed: {retried:?}");
    assert_eq!(retried.1["items"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_revision_patch_sets_availability_and_remaps_items_on_a_book_change() {
    let (f, catalog) = setup().await;
    let (eur, other) = (book(&f, "eur").await, book(&f, "other").await);
    let (_, rev) = plan(&f, "pro", eur).await;
    let (seats, storage, support) = (
        catalog.sku(SkuType::Recurring),
        catalog.sku(SkuType::Usage),
        catalog.sku(SkuType::Recurring),
    );
    let seats_eur = entry(&f, eur, seats, "recurring", Some("month")).await;
    let storage_eur = entry(&f, eur, storage, "usage", None).await;
    entry(&f, other, seats, "recurring", Some("year")).await;
    let seats_other = entry(&f, other, seats, "recurring", Some("month")).await;
    let seats_item = item(&f, rev, seats, Some(seats_eur), "paid").await;
    let storage_item = item(&f, rev, storage, Some(storage_eur), "paid").await;
    let support_item = item(&f, rev, support, None, "included").await;
    let path = format!("/plan-revisions/{rev}");
    let (s, b, tag) = f
        .call(
            "PATCH",
            &path,
            json!({"book_id":other,"available_from":"2031-03-01"}),
            Some("\"1\""),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(tag, "\"2\"");
    assert_eq!(b["book_id"], other.to_string());
    assert_eq!(b["available_from"], "2031-03-01");
    let entry_of = |id: Uuid| {
        b["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id.to_string())
            .unwrap()["price_book_entry_id"]
            .clone()
    };
    assert_eq!(
        entry_of(seats_item.id),
        seats_other.to_string(),
        "the same (SKU, charge kind, period) in the new book"
    );
    assert_eq!(
        entry_of(storage_item.id),
        storage_eur.to_string(),
        "an unmatched item keeps its old entry"
    );
    assert_eq!(entry_of(support_item.id), json!(null));
    let (s, b, tag) = f
        .call(
            "PATCH",
            &path,
            json!({"available_from":null}),
            Some("\"2\""),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(tag, "\"3\"");
    assert_eq!(b["available_from"], json!(null));
    assert_eq!(b["book_id"], other.to_string());
    let bad = f
        .call(
            "PATCH",
            &path,
            json!({"available_from":"soon"}),
            Some("\"3\""),
            None,
        )
        .await;
    assert_eq!(bad.0, 400, "{bad:?}");
    let nobook = f
        .call(
            "PATCH",
            &path,
            json!({"book_id":Uuid::new_v4()}),
            Some("\"3\""),
            None,
        )
        .await;
    assert_eq!(nobook.0, 404, "{nobook:?}");
    let items_field = f
        .call("PATCH", &path, json!({"items":[]}), Some("\"3\""), None)
        .await;
    assert_eq!(
        items_field.0, 400,
        "no item list in the revision PATCH (D-407)"
    );
}

#[tokio::test]
async fn a_revision_is_edited_only_as_a_draft_by_its_author_under_if_match() {
    let (f, _) = setup().await;
    let eur = book(&f, "eur").await;
    let (_, rev) = plan(&f, "pro", eur).await;
    let path = format!("/plan-revisions/{rev}");
    let change = json!({"available_from":"2031-03-01"});
    assert_eq!(
        f.call("PATCH", &path, change.clone(), None, None).await.0,
        400,
        "If-Match is required"
    );
    let stale = f
        .call("PATCH", &path, change.clone(), Some("\"9\""), None)
        .await;
    assert_eq!(stale.0, 409, "{stale:?}");
    assert!(text(&stale.1).contains("STALE_REVISION"), "{stale:?}");
    let colleague = f.user();
    let theirs = f
        .call_as(
            &colleague,
            "PATCH",
            &path,
            change.clone(),
            Some("\"1\""),
            None,
        )
        .await;
    assert_eq!(theirs.0, 403, "D-404: {theirs:?}");
    assert!(text(&theirs.1).contains("NOT_DRAFT_AUTHOR"), "{theirs:?}");
    let gone = f
        .call_as(&colleague, "DELETE", &path, json!({}), None, None)
        .await;
    assert_eq!(gone.0, 403, "D-404: {gone:?}");
    assert!(text(&gone.1).contains("NOT_DRAFT_AUTHOR"), "{gone:?}");
    lock(&f, rev).await;
    let pending = f
        .call("PATCH", &path, change.clone(), Some("\"2\""), None)
        .await;
    assert_eq!(pending.0, 409, "{pending:?}");
    assert!(
        text(&pending.1).contains("REVISION_NOT_DRAFT"),
        "{pending:?}"
    );
    let (q, rev2) = plan(&f, "basic", eur).await;
    publish(&f, id_of(&q["id"]), rev2).await;
    let published = format!("/plan-revisions/{rev2}");
    let (_, current, tag) = f.call("GET", &published, json!({}), None, None).await;
    assert_eq!(current["state"], "published");
    let refused = f.call("PATCH", &published, change, Some(&tag), None).await;
    assert_eq!(
        refused.0, 409,
        "a published revision is immutable: {refused:?}"
    );
    assert!(
        text(&refused.1).contains("REVISION_NOT_DRAFT"),
        "{refused:?}"
    );
    let delete = f.call("DELETE", &published, json!({}), None, None).await;
    assert_eq!(delete.0, 409, "{delete:?}");
    assert!(text(&delete.1).contains("REVISION_NOT_DRAFT"), "{delete:?}");
    let (_, after, _) = f.call("GET", &published, json!({}), None, None).await;
    assert_eq!(after["book_id"], eur.to_string());
}

#[tokio::test]
async fn a_draft_revision_delete_removes_its_items_with_a_delete_op_each() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let (_, rev) = plan(&f, "pro", eur).await;
    let first = item(&f, rev, catalog.sku(SkuType::Usage), None, "included").await;
    let second = item(&f, rev, catalog.sku(SkuType::Usage), None, "included").await;
    let path = format!("/plan-revisions/{rev}");
    let (s, b, _) = f.call("DELETE", &path, json!({}), None, None).await;
    assert_eq!(s, 204, "{b}");
    assert_eq!(f.call("GET", &path, json!({}), None, None).await.0, 404);
    assert!(items(&f, rev).await.is_empty());
    for gone in [&first, &second] {
        let ops = ops_for(&f, gone.id).await;
        assert_eq!(ops.len(), 1, "one delete op per item (D-414)");
        assert_eq!(ops[0].kind, "delete");
        assert_eq!(ops[0].reservation_id, gone.reservation_id);
        assert_eq!(ops[0].state, "done", "the door drove the release");
    }
    assert_eq!(catalog.releases(), 2);
    assert_eq!(f.call("DELETE", &path, json!({}), None, None).await.0, 404);
}

/// A plan row with no revision at all, written straight to the store: no door leaves one (D-417).
async fn bare_plan(f: &plan_support::Fixture, code: &str) -> Uuid {
    use bss_pricing::infra::storage::{entity::plan as p, repo::plan_repo};
    let now = time::OffsetDateTime::now_utc();
    plan_repo::insert(
        &f.db.conn().unwrap(),
        &plan_support::scope(f),
        p::Model {
            id: Uuid::now_v7(),
            tenant_id: f.ctx.subject_tenant_id(),
            code: code.into(),
            name: code.into(),
            published_rev: None,
            version: 1,
            created_by: f.ctx.subject_id(),
            created_at: now,
            updated_at: now,
            work_revision_id: None,
            work_state: None,
            scheduled_revision_id: None,
            scheduled_from: None,
            published_revision_id: None,
            current_book_id: None,
            current_currency: None,
            last_activity_at: now,
        },
    )
    .await
    .unwrap()
    .id
}
/// The audit actions written about one subject, in order.
async fn audited(f: &plan_support::Fixture, subject: &str) -> Vec<String> {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    Database::connect(&f.dsn)
        .await
        .unwrap()
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT action FROM pricing_audit WHERE subject_id = ? ORDER BY written_at, action",
            [subject.parse::<Uuid>().unwrap().into()],
        ))
        .await
        .unwrap()
        .iter()
        .map(|r| r.try_get::<String>("", "action").unwrap())
        .collect()
}

// D-417: the last revision of a never-published plan takes the plan with it, in the same
// transaction, and frees its code.
#[tokio::test]
async fn deleting_a_never_published_plans_last_revision_deletes_the_plan_and_frees_its_code() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let (p, rev) = plan(&f, "pro", eur).await;
    let gone = item(&f, rev, catalog.sku(SkuType::Usage), None, "included").await;
    let id = p["id"].as_str().unwrap();
    let (s, b, _) = f
        .call(
            "DELETE",
            &format!("/plan-revisions/{rev}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 204, "{b}");
    let (s, b, _) = f
        .call("GET", &format!("/plans/{id}"), json!({}), None, None)
        .await;
    assert_eq!(s, 404, "the plan went with its last revision: {b}");
    let (_, list, _) = f.call("GET", "/plans", json!({}), None, None).await;
    assert_eq!(list["items"], json!([]));
    assert_eq!(audited(&f, id).await, vec!["plan.create", "plan.delete"]);
    assert_eq!(
        ops_for(&f, gone.id).await[0].kind,
        "delete",
        "the item still has its delete op"
    );
    let (s, again, _) = f
        .call(
            "POST",
            "/plans",
            json!({"code":"PRO","name":"Pro again","book_id":eur}),
            None,
            Some("again"),
        )
        .await;
    assert_eq!(s, 201, "the code is free again: {again}");
    assert_ne!(again["id"], p["id"]);
}

// D-417: a plan with a published revision keeps itself, its code and its published revision when
// its draft is deleted.
#[tokio::test]
async fn a_draft_delete_leaves_a_plan_with_a_published_revision_as_it_was() {
    let (f, _) = setup().await;
    let eur = book(&f, "eur").await;
    let (p, rev1) = plan(&f, "pro", eur).await;
    let id = p["id"].as_str().unwrap();
    publish(&f, id_of(&p["id"]), rev1).await;
    let (s, copy, _) = f
        .call(
            "POST",
            &format!("/plans/{id}/revisions"),
            json!({}),
            None,
            Some("copy"),
        )
        .await;
    assert_eq!(s, 201, "{copy}");
    let (_, before, _) = f
        .call("GET", &format!("/plans/{id}"), json!({}), None, None)
        .await;
    let (s, b, _) = f
        .call(
            "DELETE",
            &format!("/plan-revisions/{}", copy["id"].as_str().unwrap()),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 204, "{b}");
    let (s, after, _) = f
        .call("GET", &format!("/plans/{id}"), json!({}), None, None)
        .await;
    assert_eq!(s, 200, "{after}");
    assert_eq!(after["published_rev"], 1);
    assert_eq!(
        after["version"], before["version"],
        "the plan row is untouched"
    );
    assert_eq!(after["revisions"].as_array().unwrap().len(), 1, "{after}");
    assert_eq!(after["revisions"][0]["state"], "published");
    assert!(!audited(&f, id).await.contains(&"plan.delete".to_owned()));
    let (s, taken, _) = f
        .call(
            "POST",
            "/plans",
            json!({"code":"PRO","name":"Pro again","book_id":eur}),
            None,
            Some("again"),
        )
        .await;
    assert_eq!(s, 409, "{taken}");
    assert!(text(&taken).contains("PLAN_CODE_TAKEN"), "{taken}");
}

const LOST: &str = "gts.cf.core.events.event.v1~cf.bss.pricing.plan_reference_lost.v1~";
/// Past the in-flight grace, so the ticker takes over whatever a door left.
struct LaterClock;
impl bss_pricing::infra::reference_work::Clock for LaterClock {
    fn now(&self) -> time::OffsetDateTime {
        time::OffsetDateTime::now_utc() + time::Duration::days(2)
    }
}
/// A plan whose published revision 1 holds three included items: `(plan id, copy path)`.
async fn published_with_three_items(
    f: &plan_support::Fixture,
    catalog: &plan_support::Catalog,
) -> (Uuid, String) {
    let eur = book(f, "eur").await;
    let (p, rev1) = plan(f, "pro", eur).await;
    for _ in 0..3 {
        item(f, rev1, catalog.sku(SkuType::Usage), None, "included").await;
    }
    let id = id_of(&p["id"]);
    publish(f, id, rev1).await;
    (id, format!("/plans/{id}/revisions"))
}
async fn ticker_tick(f: &plan_support::Fixture) {
    bss_pricing::infra::reference_ticker::Ticker::new(
        f.state.clone(),
        std::sync::Arc::new(LaterClock),
        10,
        100,
    )
    .tick()
    .await
    .unwrap();
}

// R-1 (D-413, D-401): an attach loses its item only on a LOSING refusal, as a rereserve does.
// Products refusing the copy's caller (no products `read` for the SKU re-read, or no products
// `reference` for the reserve) says nothing about the SKU admitting a reference: the copied items
// stay pending, no PlanReferenceLost is written, and the ticker finishes them as the system actor.
#[tokio::test]
async fn a_copy_by_a_caller_products_refuses_leaves_its_items_pending_for_the_ticker() {
    for refused in ["read", "reference"] {
        let (f, catalog) = setup().await;
        let (_, path) = published_with_three_items(&f, &catalog).await;
        let system = [bss_products_sdk::PRICING_SYSTEM_ACTOR];
        if refused == "read" {
            catalog.readers(system);
        } else {
            catalog.referencers(system);
        }
        let (s, copy, _) = f.call("POST", &path, json!({}), None, Some("copy")).await;
        assert_eq!(s, 201, "{refused}: {copy}");
        let rev2 = id_of(&copy["id"]);
        let copied = items(&f, rev2).await;
        assert_eq!(copied.len(), 3);
        for it in &copied {
            assert_eq!(
                it.reference_state, "unreserved",
                "{refused}: not lost: {it:?}"
            );
        }
        assert!(
            plan_support::entry_support::outbox_events(&f.dsn, LOST)
                .await
                .is_empty(),
            "{refused}: no PlanReferenceLost"
        );
        ticker_tick(&f).await;
        for it in items(&f, rev2).await {
            assert_eq!(it.reference_state, "confirmed", "{refused}: {it:?}");
            assert!(it.reservation_id.is_some());
            let ops = ops_for(&f, it.id).await;
            assert!(ops.iter().all(|op| op.state == "done"), "{ops:?}");
        }
        assert!(
            plan_support::entry_support::outbox_events(&f.dsn, LOST)
                .await
                .is_empty()
        );
    }
}

// R-3 (D-413): the copy drives its attach ops best-effort and stops at the first one that fails;
// the rest stay durable for the ticker. A registry that answers 503 is called once for a
// three-item copy, not three times.
#[tokio::test]
async fn a_copy_stops_driving_its_attach_ops_at_the_first_failure() {
    use std::sync::atomic::Ordering::SeqCst;
    let (f, catalog) = setup().await;
    let (_, path) = published_with_three_items(&f, &catalog).await;
    catalog.down.store(true, SeqCst);
    let before = catalog.calls();
    let (s, copy, _) = f.call("POST", &path, json!({}), None, Some("copy")).await;
    assert_eq!(s, 201, "the copy is committed: {copy}");
    assert_eq!(
        catalog.calls() - before,
        1,
        "one registry call for the whole copy"
    );
    let rev2 = id_of(&copy["id"]);
    let copied = items(&f, rev2).await;
    assert_eq!(copied.len(), 3);
    for it in &copied {
        assert_eq!(it.reference_state, "unreserved");
        let ops = ops_for(&f, it.id).await;
        assert_eq!(ops.len(), 1, "each attach op is durable: {ops:?}");
        assert_eq!(
            (ops[0].kind.as_str(), ops[0].state.as_str()),
            ("attach", "reserving")
        );
    }
    catalog.down.store(false, SeqCst);
    ticker_tick(&f).await;
    for it in items(&f, rev2).await {
        assert_eq!(it.reference_state, "confirmed", "{it:?}");
    }
}

// Second review B-1 (D-413): Products authorizes the ticker's system actor to the tenant, so a
// definite refusal given to it is about the SKU, never a caller's grant, and would never change. A
// copied item whose SKU Products no longer knows (404) is retried after the door caller's refusal
// (that one may be the caller's own), then lost at the ticker's first attempt with its
// PlanReferenceLost and its receipt released — never an attach retried forever.
#[tokio::test]
async fn an_attach_refused_to_the_system_actor_loses_its_item_at_the_first_tick() {
    let (f, catalog) = setup().await;
    let (_, path) = published_with_three_items(&f, &catalog).await;
    catalog.skus.lock().unwrap().clear();
    let (s, copy, _) = f.call("POST", &path, json!({}), None, Some("copy")).await;
    assert_eq!(s, 201, "the copy is committed: {copy}");
    let rev2 = id_of(&copy["id"]);
    for it in items(&f, rev2).await {
        assert_eq!(it.reference_state, "unreserved", "the door retries: {it:?}");
    }
    assert!(
        plan_support::entry_support::outbox_events(&f.dsn, LOST)
            .await
            .is_empty()
    );
    ticker_tick(&f).await;
    let copied = items(&f, rev2).await;
    assert_eq!(copied.len(), 3);
    for it in &copied {
        assert_eq!(it.reference_state, "lost", "{it:?}");
        let ops = ops_for(&f, it.id).await;
        assert!(ops.iter().all(|op| op.state == "done"), "{ops:?}");
    }
    assert_eq!(
        plan_support::entry_support::outbox_events(&f.dsn, LOST)
            .await
            .len(),
        3,
        "one PlanReferenceLost per item"
    );
}

#[tokio::test]
async fn plan_doors_need_the_plan_permissions_and_hide_other_tenants() {
    let (f, _) = setup().await;
    let eur = book(&f, "eur").await;
    let (p, rev) = plan(&f, "pro", eur).await;
    let id = p["id"].as_str().unwrap();
    let reader = holding(&f, "plan:read");
    for (method, path) in [
        ("GET", "/plans".to_owned()),
        ("GET", format!("/plans/{id}")),
        ("GET", format!("/plan-revisions/{rev}")),
    ] {
        let (s, b, _) = request(&f.app, &reader, method, &path, json!({}), None, None).await;
        assert_eq!(s, 200, "{method} {path}: {b}");
        let (s, _, _) = request(
            &f.app,
            &holding(&f, "price_book:read"),
            method,
            &path,
            json!({}),
            None,
            None,
        )
        .await;
        assert_eq!(s, 403, "{method} {path}");
    }
    for path in [format!("/plans/{id}"), format!("/plan-revisions/{rev}")] {
        let (s, _, _) = request(&f.app, &stranger(), "GET", &path, json!({}), None, None).await;
        assert_eq!(s, 404, "another tenant's plan is not disclosed: {path}");
    }
    let (s, list, _) = request(&f.app, &stranger(), "GET", "/plans", json!({}), None, None).await;
    assert_eq!((s, &list["items"]), (200, &json!([])));
    // The author keeps their subject and holds exactly `plan:author`.
    let author = granted(&f, "plan:author");
    for (method, path, body, tag, key) in [
        (
            "POST",
            "/plans".to_owned(),
            json!({"code":"X","name":"x","book_id":eur}),
            None,
            Some("k"),
        ),
        (
            "PATCH",
            format!("/plans/{id}"),
            json!({"name":"x"}),
            Some("\"1\""),
            None,
        ),
        (
            "POST",
            format!("/plans/{id}/revisions"),
            json!({}),
            None,
            Some("k"),
        ),
        (
            "PATCH",
            format!("/plan-revisions/{rev}"),
            json!({"available_from":null}),
            Some("\"1\""),
            None,
        ),
        (
            "DELETE",
            format!("/plan-revisions/{rev}"),
            json!({}),
            None,
            None,
        ),
    ] {
        let (s, b, _) = request(&f.app, &reader, method, &path, body.clone(), tag, key).await;
        assert_eq!(s, 403, "a reader may not author: {method} {path}: {b}");
        let (s, b, _) = request(&f.app, &stranger(), method, &path, body.clone(), tag, key).await;
        assert_eq!(
            s, 403,
            "a stranger may not write here: {method} {path}: {b}"
        );
        let (s, b, _) = request(&f.app, &author, method, &path, body, tag, key).await;
        if method == "POST" && path == "/plans" {
            // A new plan names its book, which `plan:author` alone does not read (D-456).
            assert_eq!(s, 403, "{method} {path}: {b}");
            assert!(b.to_string().contains("PRICE_BOOK_READ_REQUIRED"), "{b}");
        } else {
            assert!(s < 300 || s == 409, "{method} {path}: {s} {b}");
        }
    }
}

/// The fixture's own subject, holding exactly one `label:action` grant.
fn granted(f: &plan_support::Fixture, grant: &str) -> toolkit_security::SecurityContext {
    toolkit_security::SecurityContext::builder()
        .subject_id(f.ctx.subject_id())
        .subject_tenant_id(f.ctx.subject_tenant_id())
        .subject_type(grant)
        .build()
        .unwrap()
}

/// D-427: a book change remaps an item to the new book's entry of the same (SKU, charge kind,
/// period, model). A twin of another model is not a match, even when it comes first.
#[tokio::test]
async fn a_book_change_remaps_an_item_to_the_twin_of_its_own_model() {
    let (f, catalog) = setup().await;
    let (eur, other) = (book(&f, "eur").await, book(&f, "other").await);
    let (_, rev) = plan(&f, "pro", eur).await;
    let seats = catalog.sku(SkuType::Recurring);
    let mut ids = Vec::new();
    for (book, model, key) in [
        (eur, "flat", "eur-flat"),
        (other, "per_unit", "other-per-unit"),
        (other, "flat", "other-flat"),
    ] {
        let (s, b, _) = f
            .call(
                "POST",
                &format!("/price-books/{book}/entries"),
                json!({"sku_id":seats,"period":"month","model":model}),
                None,
                Some(key),
            )
            .await;
        assert_eq!(s, 201, "{model}: {b}");
        ids.push(id_of(&b["id"]));
    }
    let seats_item = item(&f, rev, seats, Some(ids[0]), "paid").await;
    let (s, b, _) = f
        .call(
            "PATCH",
            &format!("/plan-revisions/{rev}"),
            json!({"book_id":other}),
            Some("\"1\""),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let remapped = b["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == seats_item.id.to_string())
        .unwrap()["price_book_entry_id"]
        .clone();
    assert_eq!(
        remapped,
        ids[2].to_string(),
        "the flat twin, not the per_unit entry listed first"
    );
}

/// Fix run W1c M1 (D-424, products P-D-222): Products' registry trusts pricing's system actor
/// in-process, and a door hands the registry its caller's context. A REST caller whose token
/// carries that actor, under a policy that grants it every pricing action and a Products that
/// grants nobody `read` or `reference`, is refused at the door, 403 `SYSTEM_ACTOR_RESERVED`: no
/// entry is written and Products is never asked, so the trust never reaches a REST caller.
#[tokio::test]
async fn a_rest_caller_asserting_pricings_system_actor_never_reaches_the_registry() {
    let (f, catalog) = setup().await;
    let tenant = f.ctx.subject_tenant_id();
    let eur = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Usage);
    catalog.readers([]);
    catalog.referencers([]);
    let app = plan_support::entry_support::app_granting_every_subject(f.state.clone(), tenant);
    let actor = toolkit_security::SecurityContext::builder()
        .subject_id(bss_products_sdk::PRICING_SYSTEM_ACTOR)
        .subject_tenant_id(tenant)
        .subject_type("bss-pricing.system")
        .build()
        .unwrap();
    let (s, b, _) = request(
        &app,
        &actor,
        "POST",
        &format!("/price-books/{eur}/entries"),
        json!({"usage_rating_policy":policy_support::input(),"sku_id":sku,"model":"per_unit"}),
        None,
        Some("asserted"),
    )
    .await;
    assert_eq!(s, 403, "{b}");
    assert_eq!(b["context"]["reason"], "SYSTEM_ACTOR_RESERVED", "{b}");
    assert_eq!(
        (catalog.reads(), catalog.calls(), catalog.reserves()),
        (0, 0, 0),
        "Products was never asked"
    );
    let (s, listed, _) = f
        .call(
            "GET",
            &format!("/price-books/{eur}/entries"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{listed}");
    assert_eq!(listed["items"], json!([]), "nothing written: {listed}");
}

/// D-463: `POST /plans` takes an optional `available_from`, judged as the revision PATCH judges
/// it: a date is rev 1's sale date, omitted or null is "at publish", and a malformed one is 400
/// `DATE_INVALID` with nothing written, among the body's refusals: before the 404 of a book the
/// tenant does not hold.
#[tokio::test]
async fn a_plan_is_created_with_its_sale_date() {
    let (f, _) = setup().await;
    let eur = book(&f, "eur").await;
    let dated = f
        .call(
            "POST",
            "/plans",
            json!({"code":"DATED","name":"Dated","book_id":eur,"available_from":"2031-03-01"}),
            None,
            Some("dated"),
        )
        .await;
    assert_eq!(dated.0, 201, "{dated:?}");
    assert_eq!(dated.1["revisions"][0]["available_from"], "2031-03-01");
    let rev1 = id_of(&dated.1["revisions"][0]["id"]);
    let read = f
        .call(
            "GET",
            &format!("/plan-revisions/{rev1}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(read.1["available_from"], "2031-03-01", "stored: {read:?}");
    for (code, from) in [("omitted", None), ("null", Some(json!(null)))] {
        let mut body = json!({"code":code.to_uppercase(),"name":code,"book_id":eur});
        if let Some(from) = from {
            body["available_from"] = from;
        }
        let created = f.call("POST", "/plans", body, None, Some(code)).await;
        assert_eq!(created.0, 201, "{created:?}");
        assert_eq!(
            created.1["revisions"][0]["available_from"],
            json!(null),
            "{code}"
        );
    }
    for (key, book_id) in [("bad", eur), ("bad-and-unknown-book", Uuid::new_v4())] {
        let refused = f
            .call(
                "POST",
                "/plans",
                json!({"code":key.to_uppercase(),"name":key,"book_id":book_id,"available_from":"2031-13-01"}),
                None,
                Some(key),
            )
            .await;
        assert_eq!(refused.0, 400, "{key}: {refused:?}");
        assert!(text(&refused.1).contains("DATE_INVALID"), "{refused:?}");
    }
    let (_, listed, _) = f.call("GET", "/plans", json!({}), None, None).await;
    assert_eq!(
        listed["items"].as_array().unwrap().len(),
        3,
        "nothing written: {listed}"
    );
}
