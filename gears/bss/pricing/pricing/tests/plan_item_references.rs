//! Plan items through the durable reference machine (D-407, D-413, D-414), below their REST
//! doors (run 3.3): the create op's Tx B re-read of the revision and the entry, the attach op of
//! a copied item, the removal, and their refusals. The suites shared with entries (crash
//! windows, 503s, grace, forced release, reconciliation) run over both kinds in
//! `reference_ticker.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod entry_support;
use bss_approval::{Store, Unit, UnitState};
use bss_pricing::infra::{
    reference_ticker::Ticker,
    reference_work::{self, Caller as Driver, Clock, WallClock},
    storage::{
        RepoError,
        entity::{plan_item, price_book_entry},
        repo::{
            approval_repo::PricingApprovalStore, plan_item_repo, plan_revision_repo,
            price_book_entry_repo, price_repo, reference_op_repo as ops,
        },
    },
};
use bss_products_sdk::models::{ReferenceKind, ReferenceState};
use entry_support::{Fixture, Kind, Script, Target, outbox_events};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit_db::secure::AccessScope;
use uuid::Uuid;

const LOST: &str = "gts.cf.core.events.event.v1~cf.bss.pricing.plan_reference_lost.v1~";
/// Past the in-flight grace, so the ticker takes over whatever a door left.
struct LaterClock;
impl Clock for LaterClock {
    fn now(&self) -> time::OffsetDateTime {
        time::OffsetDateTime::now_utc() + time::Duration::days(2)
    }
}
async fn setup(mode: usize) -> (Fixture, Arc<Script>, Target) {
    let script = Arc::new(Script::default());
    script.set(mode);
    let f = Fixture::new(script.clone()).await;
    let t = f.target(Kind::Item).await;
    (f, script, t)
}
fn scope(f: &Fixture) -> AccessScope {
    AccessScope::for_tenant(f.ctx.subject_tenant_id())
}
fn id_of(value: &Value) -> Uuid {
    value.as_str().unwrap().parse().unwrap()
}
/// An entry of `book` for `sku`, written directly, so it costs the scripted registry nothing.
async fn entry(f: &Fixture, book: Uuid, sku: Uuid) -> price_book_entry::Model {
    let now = time::OffsetDateTime::now_utc();
    price_book_entry_repo::insert(
        &f.db.conn().unwrap(),
        &scope(f),
        price_book_entry::Model {
            id: Uuid::now_v7(),
            tenant_id: f.ctx.subject_tenant_id(),
            book_id: book,
            sku_id: sku,
            charge_kind: "usage".into(),
            period: None,
            model: "per_unit".into(),
            usage_policy_id: None,
            usage_policy_version: None,
            usage_policy_digest: None,
            usage_sku_version: None,
            dimension_key: None,
            invoice_line_override: None,
            reservation_id: Uuid::new_v4(),
            reference_state: "confirmed".into(),
            version: 1,
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap()
}
/// A second book of the fixture tenant.
async fn another_book(f: &Fixture) -> Uuid {
    let (s, b, _) = f
        .call(
            "POST",
            "/price-books",
            json!({"code":"other","name":"Other","currency":"EUR"}),
            None,
            Some("other-book"),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    id_of(&b["id"])
}
/// A pending `plan_revision` approval unit, so a lock can name it.
async fn unit(f: &Fixture) -> Uuid {
    let (id, tenant, scope) = (Uuid::new_v4(), f.ctx.subject_tenant_id(), scope(f));
    price_repo::transaction(&f.db.db(), move |tx| {
        let scope = scope.clone();
        Box::pin(async move {
            PricingApprovalStore {
                scope,
                tenant_id: tenant,
            }
            .insert_unit(
                tx,
                &Unit {
                    id,
                    tenant_id: tenant,
                    kind: "plan_revision".into(),
                    ref_type: "plan_revision".into(),
                    ref_id: Uuid::new_v4(),
                    state: UnitState::Pending,
                    common_effective_date: None,
                    quorum_required: 1,
                    generation: 1,
                    submitted_by: Uuid::new_v4(),
                    submitted_at: time::OffsetDateTime::now_utc(),
                    submit_note: None,
                    decided_at: None,
                    decided_note: None,
                    snapshot: json!({}),
                    snapshot_hash: "hash".into(),
                    version: 1,
                },
                &[],
            )
            .await
            .map_err(|e| RepoError::Db(e.to_string()))
        })
    })
    .await
    .unwrap();
    id
}
async fn revision_version(f: &Fixture, id: Uuid) -> i64 {
    plan_revision_repo::find(
        &f.db.conn().unwrap(),
        &scope(f),
        f.ctx.subject_tenant_id(),
        id,
    )
    .await
    .unwrap()
    .unwrap()
    .version
}
/// Submit the revision: it is locked by a pending unit, as the submit door will lock it.
async fn lock(f: &Fixture, revision: Uuid) -> Uuid {
    let u = unit(f).await;
    let version = revision_version(f, revision).await;
    assert!(
        plan_revision_repo::try_lock(
            &f.db.conn().unwrap(),
            &scope(f),
            f.ctx.subject_tenant_id(),
            revision,
            u,
            version
        )
        .await
        .unwrap()
    );
    u
}
/// Lock and publish the revision, as an applied unit will.
async fn publish(f: &Fixture, revision: Uuid) {
    let u = lock(f, revision).await;
    plan_revision_repo::publish(
        &f.db.conn().unwrap(),
        &scope(f),
        f.ctx.subject_tenant_id(),
        revision,
        u,
        time::OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
}
async fn stored_item(f: &Fixture, id: Uuid) -> Option<plan_item::Model> {
    plan_item_repo::find(
        &f.db.conn().unwrap(),
        &scope(f),
        f.ctx.subject_tenant_id(),
        id,
    )
    .await
    .unwrap()
}
/// A copied item (D-413): written `unreserved` with no receipt into the draft revision, and its
/// attach op in the same transaction, as the copy and clone doors write them.
async fn copied(f: &Fixture, t: &Target, sku: Uuid) -> (plan_item::Model, Uuid) {
    let now = time::OffsetDateTime::now_utc();
    let item = plan_item::Model {
        id: Uuid::now_v7(),
        tenant_id: f.ctx.subject_tenant_id(),
        revision_id: t.revision,
        sku_id: sku,
        price_book_entry_id: None,
        treatment: "included".into(),
        included_qty: Some("10".into()),
        qty_min: None,
        reservation_id: None,
        reference_state: "unreserved".into(),
        version: 1,
        created_by: f.ctx.subject_id(),
        created_at: now,
        updated_at: now,
    };
    let op = reference_work::attach_op(&f.ctx, &item, Uuid::now_v7(), now).unwrap();
    let op_id = op.op_id;
    let (scope, written) = (scope(f), item.clone());
    price_repo::transaction(&f.db.db(), move |tx| {
        let (scope, item, op) = (scope.clone(), written.clone(), op.clone());
        Box::pin(async move {
            plan_item_repo::insert_as_given(tx, &scope, item).await?;
            ops::insert(tx, &scope, op).await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    (item, op_id)
}
/// The door drives a copy's attach op best-effort; its error never fails the copy.
async fn drive(f: &Fixture, op: Uuid) -> bool {
    reference_work::drive(&f.state, &f.ctx, op, Arc::new(WallClock), Driver::Door)
        .await
        .is_ok()
}
async fn op_of(f: &Fixture, id: Uuid) -> bss_pricing::infra::storage::entity::reference_op::Model {
    ops::find(
        &f.db.conn().unwrap(),
        &scope(f),
        f.ctx.subject_tenant_id(),
        id,
    )
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test]
async fn an_item_create_reserves_a_plan_item_writes_and_confirms() {
    let (f, script, t) = setup(0).await;
    let c = f.caller();
    let sku = Uuid::new_v4();
    let e = entry(&f, t.book, sku).await;
    let input = json!({"sku_id":sku,"price_book_entry_id":e.id});
    let created = t.create(&c, input.clone(), "one").await;
    assert_eq!(created.0, 201, "{created:?}");
    assert_eq!(created.1["reference_state"], "confirmed");
    assert_eq!(created.1["revision_id"], t.revision.to_string());
    assert_eq!(created.1["sku_id"], sku.to_string());
    assert_eq!(created.1["price_book_entry_id"], e.id.to_string());
    // D-467: a plan item is a SKU and its entry; the new row stores `paid` and no quantity.
    for removed in ["treatment", "included_qty", "qty_min"] {
        assert!(created.1.get(removed).is_none(), "{removed}: {created:?}");
    }
    let row = stored_item(&f, id_of(&created.1["id"])).await.unwrap();
    assert_eq!(
        (row.treatment.as_str(), row.included_qty, row.qty_min),
        ("paid", None, None)
    );
    assert_eq!(created.2, "\"2\"", "written, then confirmed");
    assert_eq!(t.create(&c, input, "one").await, created, "the key replays");
    assert_eq!(
        *script.reserve_kinds.lock().unwrap(),
        [ReferenceKind::PlanItem],
        "one reserve, of kind plan_item"
    );
    let refs = script.refs.lock().await;
    assert_eq!(
        refs[&id_of(&created.1["id"])],
        (
            id_of(&created.1["reservation_id"]),
            ReferenceState::Confirmed
        ),
        "the reserve's ref_id is the item id"
    );
    let item = stored_item(&f, id_of(&created.1["id"])).await.unwrap();
    assert_eq!(item.created_by, f.ctx.subject_id());
}

#[tokio::test]
async fn refusal_branches_answer_the_key_and_release_only_after_a_reservation() {
    // R-2 (D-403): the SKU re-read answers what the item door answers for the same SKU, 400
    // ITEM_BUNDLE_SKU or ITEM_SKU_DEPRECATED (it pinned 409 SKU_DEPRECATED before fix run 7); the
    // op keeps its SkuRefused code. A fence at the reserve and a draft SKU stay 409.
    for (mode, status, code, event, releases) in [
        (4, 409, "SKU_FENCED", "SKU_FENCED", 0),
        (8, 400, "ITEM_BUNDLE_SKU", "ITEM_BUNDLE_SKU", 1),
        (9, 400, "ITEM_SKU_DEPRECATED", "SKU_DEPRECATED", 1),
        (10, 409, "SKU_DRAFT", "SKU_DRAFT", 1),
    ] {
        let (f, script, t) = setup(mode).await;
        let c = f.caller();
        let input = t.input(&c).await;
        let refused = t.create(&c, input.clone(), "one").await;
        assert_eq!(refused.0, status, "{mode}: {refused:?}");
        assert!(refused.1.to_string().contains(code), "{refused:?}");
        let ops = ops::page(
            &f.db.conn().unwrap(),
            &scope(&f),
            f.ctx.subject_tenant_id(),
            None,
            None,
            10,
        )
        .await
        .unwrap();
        assert_eq!(ops.len(), 1, "{mode}: {ops:?}");
        assert_eq!(
            ops[0].last_error.as_deref(),
            Some(event),
            "{mode}: the event code"
        );
        script.set(0);
        assert_eq!(t.create(&c, input, "one").await, refused);
        assert_eq!(Script::count(&script.releases), releases, "{code}");
        assert!(
            plan_item_repo::for_revision(
                &f.db.conn().unwrap(),
                &scope(&f),
                f.ctx.subject_tenant_id(),
                t.revision
            )
            .await
            .unwrap()
            .is_empty(),
            "{code}: no item"
        );
    }
}

#[tokio::test]
async fn a_second_item_for_the_same_sku_is_refused_and_its_reservation_released() {
    let (f, script, t) = setup(0).await;
    let c = f.caller();
    let input = t.input(&c).await;
    assert_eq!(t.create(&c, input.clone(), "one").await.0, 201);
    let taken = t.create(&c, input.clone(), "two").await;
    assert_eq!(taken.0, 409, "{taken:?}");
    assert!(taken.1.to_string().contains("ITEM_SKU_TAKEN"), "{taken:?}");
    assert_eq!(t.create(&c, input, "two").await, taken, "the key replays");
    assert_eq!(Script::count(&script.reserve_calls), 2);
    assert_eq!(Script::count(&script.releases), 1);
}

/// Park the create at its reserve, let `between` change the world, then let it finish.
async fn raced(
    f: &Fixture,
    script: &Script,
    t: &Target,
    input: Value,
    between: impl std::future::Future<Output = ()>,
) -> (u16, Value, String) {
    script.set(1);
    let c = f.caller();
    let mut door = Box::pin(t.create(&c, input, "raced"));
    tokio::select! { result = &mut door => panic!("door did not park: {result:?}"), () = script.parked.notified() => {} }
    between.await;
    script.resume.notify_one();
    let answer = door.await;
    script.set(0);
    answer
}
/// A refused create: its code, one released receipt, no item, and the key answered.
async fn assert_refused(
    f: &Fixture,
    script: &Script,
    t: &Target,
    answer: &(u16, Value, String),
    status: u16,
    code: &str,
) {
    assert_eq!(answer.0, status, "{code}: {answer:?}");
    assert!(answer.1.to_string().contains(code), "{code}: {answer:?}");
    assert_eq!(
        Script::count(&script.releases),
        1,
        "{code}: the receipt is released"
    );
    assert!(
        script
            .refs
            .lock()
            .await
            .values()
            .all(|(_, state)| *state == ReferenceState::Released),
        "{code}"
    );
    assert!(
        plan_item_repo::for_revision(
            &f.db.conn().unwrap(),
            &AccessScope::allow_all(),
            f.ctx.subject_tenant_id(),
            t.revision
        )
        .await
        .unwrap()
        .is_empty(),
        "{code}: no item"
    );
    let due = ops::due(&f.db.conn().unwrap(), &scope(f), LaterClock.now(), 10)
        .await
        .unwrap();
    assert!(due.is_empty(), "{code}: the create op is done: {due:?}");
}

// Tx B re-reads the revision (D-407): a submit, a publish or a delete that lands between the
// reserve and the write refuses the item, so SSI orders it against them.
#[tokio::test]
async fn the_write_rereads_the_revision_a_submit_between_reserve_and_write_refuses_the_item() {
    let (f, script, t) = setup(0).await;
    let input = t.input(&f.caller()).await;
    let answer = raced(&f, &script, &t, input.clone(), async {
        lock(&f, t.revision).await;
    })
    .await;
    assert_refused(&f, &script, &t, &answer, 409, "REVISION_NOT_DRAFT").await;
    assert_eq!(
        t.create(&f.caller(), input, "raced").await,
        answer,
        "the key is answered with the refusal"
    );
}
#[tokio::test]
async fn a_revision_published_between_reserve_and_write_refuses_the_item() {
    let (f, script, t) = setup(0).await;
    let answer = raced(&f, &script, &t, t.input(&f.caller()).await, async {
        publish(&f, t.revision).await;
    })
    .await;
    assert_refused(&f, &script, &t, &answer, 409, "REVISION_NOT_DRAFT").await;
}
#[tokio::test]
async fn a_revision_deleted_between_reserve_and_write_refuses_the_item() {
    let (f, script, t) = setup(0).await;
    let answer = raced(&f, &script, &t, t.input(&f.caller()).await, async {
        let version = revision_version(&f, t.revision).await;
        plan_revision_repo::delete_draft(
            &f.db.conn().unwrap(),
            &scope(&f),
            f.ctx.subject_tenant_id(),
            t.revision,
            version,
        )
        .await
        .unwrap();
    })
    .await;
    assert_refused(&f, &script, &t, &answer, 409, "REVISION_NOT_FOUND").await;
}
#[tokio::test]
async fn a_book_change_between_reserve_and_write_makes_the_entry_foreign() {
    let (f, script, t) = setup(0).await;
    let sku = Uuid::new_v4();
    let e = entry(&f, t.book, sku).await;
    let other = another_book(&f).await;
    let answer = raced(
        &f,
        &script,
        &t,
        json!({"sku_id":sku,"price_book_entry_id":e.id}),
        async {
            let conn = f.db.conn().unwrap();
            let mut r =
                plan_revision_repo::find(&conn, &scope(&f), f.ctx.subject_tenant_id(), t.revision)
                    .await
                    .unwrap()
                    .unwrap();
            r.book_id = other;
            plan_revision_repo::update_draft(&conn, &scope(&f), r)
                .await
                .unwrap();
        },
    )
    .await;
    assert_refused(&f, &script, &t, &answer, 400, "ITEM_BOOK_FOREIGN").await;
    assert!(
        answer.1.to_string().contains("price_book_entry_id"),
        "{answer:?}"
    );
}
#[tokio::test]
async fn the_write_refuses_an_entry_of_another_book_or_sku_or_an_unknown_one() {
    for case in ["book", "sku", "unknown"] {
        let (f, script, t) = setup(0).await;
        let sku = Uuid::new_v4();
        let entry_id = match case {
            "book" => entry(&f, another_book(&f).await, sku).await.id,
            "sku" => entry(&f, t.book, Uuid::new_v4()).await.id,
            _ => Uuid::new_v4(),
        };
        let answer = t
            .create(
                &f.caller(),
                json!({"sku_id":sku,"price_book_entry_id":entry_id}),
                "one",
            )
            .await;
        let (status, code) = match case {
            "book" => (400, "ITEM_BOOK_FOREIGN"),
            "sku" => (400, "ITEM_ENTRY_SKU_MISMATCH"),
            _ => (409, "ENTRY_NOT_FOUND"),
        };
        assert_refused(&f, &script, &t, &answer, status, code).await;
    }
}

// ---------------------------------------------------------------- attach (D-413)

#[tokio::test]
async fn an_attach_admits_a_deprecated_sku_and_a_create_does_not() {
    let (f, script, t) = setup(9).await;
    let (item, op) = copied(&f, &t, Uuid::new_v4()).await;
    assert!(drive(&f, op).await, "the door finished the attach");
    let got = stored_item(&f, item.id).await.unwrap();
    assert_eq!(got.reference_state, "confirmed", "{got:?}");
    assert_eq!(
        script.refs.lock().await[&item.id],
        (got.reservation_id.unwrap(), ReferenceState::Confirmed)
    );
    assert_eq!(op_of(&f, op).await.state, "done");
    assert_eq!(
        *script.reserve_kinds.lock().unwrap(),
        [ReferenceKind::PlanItem]
    );
    let refused = t
        .create(&f.caller(), t.input(&f.caller()).await, "new")
        .await;
    assert_eq!(
        refused.0, 400,
        "the item door's own answer (R-2): {refused:?}"
    );
    assert!(
        refused.1.to_string().contains("ITEM_SKU_DEPRECATED"),
        "{refused:?}"
    );
    assert!(outbox_events(&f.dsn, LOST).await.is_empty());
}
#[tokio::test]
async fn an_attach_refused_loses_the_item_once_and_reconciliation_finds_it_again() {
    let (f, script, t) = setup(4).await;
    let (item, op) = copied(&f, &t, Uuid::new_v4()).await;
    assert!(drive(&f, op).await, "a refusal is an answer, not an outage");
    let got = stored_item(&f, item.id).await.unwrap();
    assert_eq!(got.reference_state, "lost", "{got:?}");
    assert_eq!(got.reservation_id, None);
    assert_eq!(Script::count(&script.releases), 0, "nothing was reserved");
    let events = outbox_events(&f.dsn, LOST).await;
    assert_eq!(events.len(), 1, "{events:?}");
    let data = &events[0]["data"];
    assert_eq!(data["planId"], t.plan.to_string());
    assert_eq!(data["revisionId"], t.revision.to_string());
    assert_eq!(data["itemId"], item.id.to_string());
    assert_eq!(data["skuId"], item.sku_id.to_string());
    assert_eq!(data["reservationId"], Value::Null);
    assert_eq!(
        events[0]["tenant_id"],
        f.ctx.subject_tenant_id().to_string()
    );
    assert_eq!(events[0]["subject"], item.id.to_string());
    // Still fenced: reconciliation leaves the lost item alone and announces nothing twice.
    let mut ticker = Ticker::new(f.state.clone(), Arc::new(LaterClock), 10, 1);
    let reserves = Script::count(&script.reserve_calls);
    ticker.tick().await.unwrap();
    assert_eq!(Script::count(&script.reserve_calls), reserves);
    // The fence lifts: the lost item is re-reserved.
    script.set(0);
    ticker.tick().await.unwrap();
    let got = stored_item(&f, item.id).await.unwrap();
    assert_eq!(got.reference_state, "confirmed", "{got:?}");
    assert!(got.reservation_id.is_some());
    assert_eq!(outbox_events(&f.dsn, LOST).await.len(), 1);
}
#[tokio::test]
async fn an_attach_whose_sku_reread_refuses_releases_its_receipt_and_loses_the_item() {
    let (f, script, t) = setup(10).await;
    let (item, op) = copied(&f, &t, Uuid::new_v4()).await;
    assert!(drive(&f, op).await);
    let got = stored_item(&f, item.id).await.unwrap();
    assert_eq!(got.reference_state, "lost", "{got:?}");
    assert_eq!(
        Script::count(&script.releases),
        1,
        "the receipt is released"
    );
    assert_eq!(outbox_events(&f.dsn, LOST).await.len(), 1);
    assert_eq!(op_of(&f, op).await.last_error.as_deref(), Some("SKU_DRAFT"));
}
#[tokio::test]
async fn the_ticker_finishes_an_attach_the_door_could_not() {
    // The reserve's answer is lost: the door gives the attach up for now (the copy is already
    // answered 201). The ticker, unlike for a create, reserves on its own and finishes it.
    let (f, script, t) = setup(5).await;
    let (item, op) = copied(&f, &t, Uuid::new_v4()).await;
    assert!(!drive(&f, op).await, "the door could not finish");
    let pending = op_of(&f, op).await;
    assert_eq!(pending.state, "reserving");
    assert_eq!(pending.reservation_id, None);
    assert_eq!(pending.attempts, 1);
    assert_eq!(
        stored_item(&f, item.id).await.unwrap().reference_state,
        "unreserved"
    );
    let made = script.refs.lock().await[&item.id].0;
    Ticker::new(f.state.clone(), Arc::new(LaterClock), 10, 100)
        .tick()
        .await
        .unwrap();
    let got = stored_item(&f, item.id).await.unwrap();
    assert_eq!(got.reference_state, "confirmed", "{got:?}");
    assert_eq!(
        got.reservation_id,
        Some(made),
        "reserve is idempotent per reference: the lost call's reservation is the one kept"
    );
    assert_eq!(Script::count(&script.releases), 0);
    assert_eq!(op_of(&f, op).await.state, "done");
}
#[tokio::test]
async fn an_attach_and_a_rereserve_move_an_item_of_a_published_or_superseded_revision() {
    // The reference machine is the one writer allowed to touch an item of a published or
    // superseded revision (D-413, D-414).
    for superseded in [false, true] {
        let (f, script, t) = setup(0).await;
        let now = time::OffsetDateTime::now_utc();
        let item = plan_item::Model {
            id: Uuid::now_v7(),
            tenant_id: f.ctx.subject_tenant_id(),
            revision_id: t.revision,
            sku_id: Uuid::new_v4(),
            price_book_entry_id: None,
            treatment: "included".into(),
            included_qty: Some("1".into()),
            qty_min: None,
            reservation_id: None,
            reference_state: "unreserved".into(),
            version: 1,
            created_by: f.ctx.subject_id(),
            created_at: now,
            updated_at: now,
        };
        plan_item_repo::insert_as_given(&f.db.conn().unwrap(), &scope(&f), item.clone())
            .await
            .unwrap();
        publish(&f, t.revision).await;
        if superseded {
            let version = revision_version(&f, t.revision).await;
            plan_revision_repo::supersede(
                &f.db.conn().unwrap(),
                &scope(&f),
                f.ctx.subject_tenant_id(),
                t.revision,
                version,
                now,
            )
            .await
            .unwrap();
        }
        let op = reference_work::attach_op(&f.ctx, &item, Uuid::now_v7(), now).unwrap();
        let op_id = op.op_id;
        ops::insert(&f.db.conn().unwrap(), &scope(&f), op)
            .await
            .unwrap();
        assert!(drive(&f, op_id).await);
        let got = stored_item(&f, item.id).await.unwrap();
        assert_eq!(got.reference_state, "confirmed", "{superseded}: {got:?}");
        // A forced release: reconciliation re-reserves it in place.
        for value in script.refs.lock().await.values_mut() {
            value.1 = ReferenceState::Released;
        }
        Ticker::new(f.state.clone(), Arc::new(LaterClock), 10, 1)
            .tick()
            .await
            .unwrap();
        let healed = stored_item(&f, item.id).await.unwrap();
        assert_eq!(
            healed.reference_state, "confirmed",
            "{superseded}: {healed:?}"
        );
        assert_ne!(healed.reservation_id, got.reservation_id);
    }
}
#[tokio::test]
async fn an_attach_for_an_item_removed_before_its_write_releases_its_receipt() {
    let (f, script, t) = setup(0).await;
    let (item, op) = copied(&f, &t, Uuid::new_v4()).await;
    let deleted = t.delete(&f.caller(), &json!(item.id)).await;
    assert_eq!(deleted.0, 204, "an unreserved item may go: {deleted:?}");
    assert!(drive(&f, op).await);
    assert!(stored_item(&f, item.id).await.is_none());
    assert_eq!(op_of(&f, op).await.state, "done");
    assert_eq!(
        Script::count(&script.releases),
        1,
        "the new receipt is released"
    );
    assert!(
        outbox_events(&f.dsn, LOST).await.is_empty(),
        "nothing to lose"
    );
}

// ---------------------------------------------------------------- remove (D-414)

#[tokio::test]
async fn an_item_delete_releases_its_receipt_after_its_removal() {
    let (f, script, t) = setup(0).await;
    let c = f.caller();
    let created = t.create(&c, t.input(&c).await, "one").await;
    assert_eq!(created.0, 201);
    let deleted = t.delete(&c, &created.1["id"]).await;
    assert_eq!(deleted.0, 204, "{deleted:?}");
    assert!(stored_item(&f, id_of(&created.1["id"])).await.is_none());
    assert_eq!(Script::count(&script.releases), 1);
    assert_eq!(
        script.refs.lock().await[&id_of(&created.1["id"])].1,
        ReferenceState::Released
    );
    let done = ops::page(
        &f.db.conn().unwrap(),
        &scope(&f),
        f.ctx.subject_tenant_id(),
        None,
        None,
        10,
    )
    .await
    .unwrap();
    assert!(
        done.iter().any(|op| op.kind == "delete"
            && op.ref_kind == "plan_item"
            && op.ref_id == id_of(&created.1["id"])
            && op.state == "done"),
        "{done:?}"
    );
}
#[tokio::test]
async fn an_item_delete_waits_for_its_confirm_and_never_touches_a_published_revision() {
    let (f, script, t) = setup(7).await;
    let c = f.caller();
    let pending = t.create(&c, t.input(&c).await, "one").await;
    assert_eq!(pending.1["reference_state"], "confirmation_pending");
    let refused = t.delete(&c, &pending.1["id"]).await;
    assert_eq!(refused.0, 409, "{refused:?}");
    assert!(
        refused.1.to_string().contains("ITEM_CONFIRMATION_PENDING"),
        "{refused:?}"
    );
    script.set(0);
    Ticker::new(f.state.clone(), Arc::new(LaterClock), 10, 100)
        .tick()
        .await
        .unwrap();
    assert_eq!(
        stored_item(&f, id_of(&pending.1["id"]))
            .await
            .unwrap()
            .reference_state,
        "confirmed"
    );
    // Published: its references outlive it (D-414), so its items are never deleted.
    publish(&f, t.revision).await;
    let refused = t.delete(&c, &pending.1["id"]).await;
    assert_eq!(refused.0, 409, "{refused:?}");
    assert!(
        refused.1.to_string().contains("REVISION_NOT_DRAFT"),
        "{refused:?}"
    );
    assert!(stored_item(&f, id_of(&pending.1["id"])).await.is_some());
    let missing = t.delete(&c, &json!(Uuid::new_v4())).await;
    assert_eq!(missing.0, 404, "{missing:?}");
    assert_eq!(
        missing.1["context"]["resource_name"], "plan_item",
        "{missing:?}"
    );
}

// Behaviour LOW-2 for items: a door that fails after its reserve and before its item write
// cancels the create before it answers. Tx B stays contended past its retries, so the answer is
// 409 CONTENDED; no ticker pass turns that answer into an item, and a same-key retry runs afresh.
#[tokio::test]
async fn a_contended_item_write_after_the_reserve_cancels_the_create() {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    let (f, script, t) = setup(0).await;
    let exec = |sql: &str| {
        let (dsn, sql) = (f.dsn.clone(), sql.to_owned());
        async move {
            Database::connect(&dsn)
                .await
                .unwrap()
                .execute_raw(Statement::from_string(DbBackend::Sqlite, sql))
                .await
                .unwrap();
        }
    };
    exec(
        "CREATE TRIGGER item_busy BEFORE INSERT ON pricing_plan_item \
         BEGIN SELECT RAISE(ABORT, '(code: 5) database is locked'); END",
    )
    .await;
    let input = t.input(&f.caller()).await;
    let first = t.create(&f.caller(), input.clone(), "one").await;
    assert_eq!(first.0, 409, "{first:?}");
    assert!(first.1.to_string().contains("CONTENDED"), "{first:?}");
    assert_eq!(
        Script::count(&script.reserve_calls),
        1,
        "the reserve succeeded"
    );
    exec("DROP TRIGGER item_busy").await;
    Ticker::new(f.state.clone(), Arc::new(LaterClock), 10, 100)
        .tick()
        .await
        .unwrap();
    assert!(
        plan_item_repo::for_revision(
            &f.db.conn().unwrap(),
            &scope(&f),
            f.ctx.subject_tenant_id(),
            t.revision
        )
        .await
        .unwrap()
        .is_empty(),
        "the answered 409 wrote no item"
    );
    assert_eq!(
        Script::count(&script.releases),
        1,
        "the cancellation released the receipt"
    );
    let retry = t.create(&f.caller(), input, "one").await;
    assert_eq!(retry.0, 201, "a same-key retry runs afresh: {retry:?}");
    assert_eq!(retry.1["reference_state"], "confirmed");
}

/// D-465: the create op's SKU re-read judges a deprecated SKU as the item door does. Every SKU
/// reads deprecated here (mode 9). On a draft of a plan whose published revision in effect
/// carries the SKU, the create is written and confirmed; any other SKU is refused with the
/// door's own answer, 400 `ITEM_SKU_DEPRECATED`, its op keeping `SKU_DEPRECATED`.
#[tokio::test]
async fn the_create_ops_re_read_admits_a_deprecated_sku_its_plan_sells() {
    let (f, _script, t) = setup(9).await;
    let tenant = f.ctx.subject_tenant_id();
    let now = time::OffsetDateTime::now_utc();
    let carried = Uuid::new_v4();
    let carried_entry = entry(&f, t.book, carried).await.id;
    let other = Uuid::new_v4();
    let other_entry = entry(&f, t.book, other).await.id;
    plan_item_repo::insert_as_given(
        &f.db.conn().unwrap(),
        &scope(&f),
        plan_item::Model {
            id: Uuid::now_v7(),
            tenant_id: tenant,
            revision_id: t.revision,
            sku_id: carried,
            price_book_entry_id: Some(carried_entry),
            treatment: "paid".into(),
            included_qty: None,
            qty_min: None,
            reservation_id: Some(Uuid::new_v4()),
            reference_state: "confirmed".into(),
            version: 1,
            created_by: f.ctx.subject_id(),
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();
    publish(&f, t.revision).await;
    let rev2 = plan_revision_repo::insert(
        &f.db.conn().unwrap(),
        &scope(&f),
        bss_pricing::infra::storage::entity::plan_revision::Model {
            id: Uuid::now_v7(),
            tenant_id: tenant,
            plan_id: t.plan,
            rev_no: 2,
            book_id: t.book,
            state: "draft".into(),
            available_from: None,
            pending_unit_id: None,
            approved_by_unit_id: None,
            published_at: None,
            version: 1,
            created_by: f.ctx.subject_id(),
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap()
    .id;
    let create = |sku: Uuid, entry: Uuid, key: &'static str| {
        let input = json!({"sku_id":sku,"price_book_entry_id":entry});
        let digest = bss_pricing::api::rest::preconditions::request_digest(&input).unwrap();
        let body: bss_pricing::api::rest::authoring::dto::PricingPlanItemCreate =
            serde_json::from_value(input).unwrap();
        bss_pricing::api::rest::authoring::plan_items::create(
            f.state.clone(),
            scope(&f),
            f.ctx.clone(),
            rev2,
            Uuid::now_v7(),
            key.to_owned(),
            digest,
            body,
        )
    };
    let written = entry_support::answer(create(carried, carried_entry, "carried").await).await;
    assert_eq!(written.0, 201, "{written:?}");
    let item = stored_item(&f, id_of(&written.1["id"])).await.unwrap();
    assert_eq!(item.revision_id, rev2);
    assert_eq!(item.reference_state, "confirmed", "{item:?}");
    let refused = entry_support::answer(create(other, other_entry, "other").await).await;
    assert_eq!(refused.0, 400, "{refused:?}");
    assert!(
        refused.1.to_string().contains("ITEM_SKU_DEPRECATED"),
        "{refused:?}"
    );
    let ops = ops::page(&f.db.conn().unwrap(), &scope(&f), tenant, None, None, 100)
        .await
        .unwrap();
    let refused_op = ops.iter().find(|op| op.sku_id == other).unwrap();
    assert_eq!(refused_op.last_error.as_deref(), Some("SKU_DEPRECATED"));
}

/// The phase 9 review's R21: the reference machine's connection is refused only when it is asked
/// for inside a transaction. That is a storage failure, logged with its cause, never "invalid
/// durable pricing reference work", which names a corrupt op record.
#[tokio::test]
async fn a_connection_refused_inside_a_transaction_is_a_storage_failure_not_a_corrupt_op() {
    let (f, _, _) = setup(0).await;
    let (state, ctx) = (f.state.clone(), f.ctx.clone());
    let diagnostic = price_repo::transaction(&f.db.db(), move |_tx| {
        let (state, ctx) = (state.clone(), ctx.clone());
        Box::pin(async move {
            let error = reference_work::drive(
                &state,
                &ctx,
                Uuid::now_v7(),
                Arc::new(WallClock),
                Driver::Door,
            )
            .await
            .unwrap_err();
            Ok(error.diagnostic().unwrap_or_default().to_owned())
        })
    })
    .await
    .unwrap();
    assert_eq!(diagnostic, "pricing storage failure");
}
