//! Crash windows drop the actual door futures at deterministic registry awaits.
//!
//! Every suite here runs once per reference kind (D-407): an entry through the entries REST door,
//! a plan item through the plan-item op-level API (its REST door is run 3.3's).
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod entry_support;
use bss_pricing::{
    domain::price_book_entry::OpState,
    infra::{
        reference_ticker::Ticker,
        reference_work::Clock,
        storage::repo::{
            idempotency_repo as idem, price_book_entry_repo, price_repo, reference_op_repo as ops,
        },
    },
};
use entry_support::{Caller, Fixture, KINDS, Kind, Script, Target};
use serde_json::json;
use std::sync::Arc;
use toolkit_db::secure::AccessScope;
use uuid::Uuid;
struct FixedClock(time::OffsetDateTime);
impl Clock for FixedClock {
    fn now(&self) -> time::OffsetDateTime {
        self.0
    }
}
fn clock() -> Arc<dyn Clock> {
    Arc::new(FixedClock(
        time::OffsetDateTime::now_utc() + time::Duration::days(2),
    ))
}
async fn setup(kind: Kind) -> (Fixture, Arc<Script>, Target, serde_json::Value) {
    let script = Arc::new(Script::default());
    let f = Fixture::new(script.clone()).await;
    let t = f.target(kind).await;
    let input = t.input(&f.caller()).await;
    (f, script, t, input)
}
fn id_of(value: &serde_json::Value) -> Uuid {
    value.as_str().unwrap().parse().unwrap()
}
async fn crash_create(kind: Kind, mode: usize, expected_state: &str) {
    let (f, script, t, input) = setup(kind).await;
    let c = f.caller();
    script.set(mode);
    let mut door = Box::pin(t.create(&c, input.clone(), "crash"));
    tokio::select! { result = &mut door => panic!("{kind:?}: door did not park: {result:?}"), () = script.parked.notified() => {} }
    drop(door);
    let scope = AccessScope::for_tenant(f.ctx.subject_tenant_id());
    let before = ops::due(&f.db.conn().unwrap(), &scope, clock().now(), 10)
        .await
        .unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].state, expected_state);
    assert_eq!(before[0].ref_kind, t.ref_kind());
    script.set(0);
    let mut ticker = Ticker::new(f.state.clone(), clock(), 10, 100);
    ticker.tick().await.unwrap();
    let conn = f.db.conn().unwrap();
    let after = ops::find(&conn, &scope, f.ctx.subject_tenant_id(), before[0].op_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.state, "done");
    let (state, _) = t.stored(&f.state, before[0].ref_id).await.unwrap();
    assert_eq!(state, "confirmed", "{kind:?}");
    let replay = t.create(&c, input, "crash").await;
    assert_eq!(replay.0, 201, "{kind:?}: {replay:?}");
    assert_eq!(replay.1["id"], before[0].ref_id.to_string());
    assert_answered(&f, &t, "crash").await;
}
/// The key holds its stored answer.
async fn assert_answered(f: &Fixture, t: &Target, key: &str) {
    assert!(matches!(
        key_claim(f, t, key).await,
        Some(idem::IdempotencyClaim::Answered { .. })
    ));
}
/// The key and its op as the store holds them after the given op finished.
async fn key_claim(f: &Fixture, t: &Target, key: &str) -> Option<idem::IdempotencyClaim> {
    idem::lookup_idempotency_key(
        &f.db.conn().unwrap(),
        &AccessScope::for_tenant(f.ctx.subject_tenant_id()),
        f.ctx.subject_tenant_id(),
        &format!("/bss-pricing/v1{}", t.endpoint()),
        key,
        time::OffsetDateTime::now_utc(),
    )
    .await
    .unwrap()
}
/// A create cancelled before its reservation outcome was known: done, recorded `cancelled`,
/// nothing written, and its key free for a fresh attempt.
async fn assert_cancelled_without_a_write(f: &Fixture, t: &Target, op_id: Uuid, key: &str) {
    let scope = AccessScope::for_tenant(f.ctx.subject_tenant_id());
    let conn = f.db.conn().unwrap();
    let op = ops::find(&conn, &scope, f.ctx.subject_tenant_id(), op_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op.state, "done");
    let work = bss_pricing::infra::reference_work::Work::read(&op).unwrap();
    assert_eq!(work.outcome.as_deref(), Some("cancelled"), "{op:?}");
    assert!(
        t.stored(&f.state, op.ref_id).await.is_none(),
        "a cancelled create writes nothing"
    );
    assert_eq!(key_claim(f, t, key).await, None, "the claim was released");
}
#[tokio::test]
async fn crash_after_tx_a_cancels_the_create_and_frees_the_key() {
    // The door died before it learned whether Products reserved. The ticker never reserves on
    // a user's behalf: it cancels, releases the key, and releases whatever reservation exists.
    for kind in KINDS {
        let (f, script, t, input) = setup(kind).await;
        let c = f.caller();
        script.set(1);
        let mut door = Box::pin(t.create(&c, input.clone(), "crash"));
        tokio::select! { result = &mut door => panic!("door did not park: {result:?}"), () = script.parked.notified() => {} }
        drop(door);
        let scope = AccessScope::for_tenant(f.ctx.subject_tenant_id());
        let before = ops::due(&f.db.conn().unwrap(), &scope, clock().now(), 10)
            .await
            .unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].state, "reserving");
        assert_eq!(before[0].reservation_id, None);
        script.set(0);
        Ticker::new(f.state.clone(), clock(), 10, 100)
            .tick()
            .await
            .unwrap();
        assert_cancelled_without_a_write(&f, &t, before[0].op_id, "crash").await;
        assert_eq!(
            Script::count(&script.releases),
            1,
            "{kind:?}: the cancellation's own reserve found the reservation and released it"
        );
        assert!(
            script
                .refs
                .lock()
                .await
                .get(&before[0].ref_id)
                .is_none_or(|r| r.1 == bss_products_sdk::models::ReferenceState::Released)
        );
        let retry = t.create(&c, input, "crash").await;
        assert_eq!(retry.0, 201, "{kind:?}: {retry:?}");
        assert_ne!(
            retry.1["id"],
            before[0].ref_id.to_string(),
            "a fresh reference"
        );
        assert!(matches!(
            key_claim(&f, &t, "crash").await,
            Some(idem::IdempotencyClaim::Answered { .. })
        ));
    }
}
#[tokio::test]
async fn crash_after_tx_b_recovers_and_answers_key() {
    for kind in KINDS {
        crash_create(kind, 2, "written").await;
    }
}
#[tokio::test]
async fn crash_after_delete_tx_recovers_release() {
    for kind in KINDS {
        let (f, script, t, input) = setup(kind).await;
        let c = f.caller();
        let created = t.create(&c, input, "one").await;
        assert_eq!(created.0, 201);
        script.set(3);
        let mut door = Box::pin(t.delete(&c, &created.1["id"]));
        tokio::select! { result = &mut door => panic!("door did not park: {result:?}"), () = script.parked.notified() => {} }
        drop(door);
        assert!(
            t.stored(&f.state, id_of(&created.1["id"])).await.is_none(),
            "{kind:?}: the removal committed before the release"
        );
        script.set(0);
        let mut ticker = Ticker::new(f.state.clone(), clock(), 10, 100);
        ticker.tick().await.unwrap();
        assert_eq!(Script::count(&script.releases), 1, "{kind:?}");
        let scope = AccessScope::for_tenant(f.ctx.subject_tenant_id());
        assert!(
            ops::due(&f.db.conn().unwrap(), &scope, clock().now(), 10)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
#[tokio::test]
async fn a_lost_reserve_response_writes_nothing_and_its_reservation_is_released() {
    // Products reserved but the answer never arrived: the door answers 503, writes nothing and
    // frees the key; the ticker's cancellation finds that reservation and releases it.
    for kind in KINDS {
        let (f, script, t, input) = setup(kind).await;
        let c = f.caller();
        script.set(5);
        let first = t.create(&c, input.clone(), "one").await;
        assert_eq!(first.0, 503, "{kind:?}: {first:?}");
        assert!(
            first.1.to_string().contains("REGISTRY_UNAVAILABLE"),
            "{first:?}"
        );
        let scope = AccessScope::for_tenant(f.ctx.subject_tenant_id());
        let cancelling = ops::page(
            &f.db.conn().unwrap(),
            &scope,
            f.ctx.subject_tenant_id(),
            Some(OpState::Cancelling),
            None,
            10,
        )
        .await
        .unwrap();
        assert_eq!(cancelling.len(), 1, "the door cancelled its own op");
        assert_eq!(key_claim(&f, &t, "one").await, None, "and released the key");
        let (lost_ref, lost_receipt) = {
            let refs = script.refs.lock().await;
            let (id, (receipt, _)) = refs.iter().next().unwrap();
            (*id, *receipt)
        };
        assert_eq!(lost_ref, cancelling[0].ref_id);
        Ticker::new(f.state.clone(), clock(), 10, 100)
            .tick()
            .await
            .unwrap();
        assert_cancelled_without_a_write(&f, &t, cancelling[0].op_id, "one").await;
        assert_eq!(Script::count(&script.releases), 1);
        assert_eq!(
            script.refs.lock().await[&lost_ref],
            (
                lost_receipt,
                bss_products_sdk::models::ReferenceState::Released
            )
        );
        let retry = t.create(&c, input, "one").await;
        assert_eq!(retry.0, 201, "{kind:?}: {retry:?}");
        assert_ne!(retry.1["id"], lost_ref.to_string());
        assert_ne!(retry.1["reservation_id"], lost_receipt.to_string());
    }
}
#[tokio::test]
async fn ticker_recovers_a_confirm_timeout_and_answers_the_key() {
    for kind in KINDS {
        let (f, script, t, input) = setup(kind).await;
        let c = f.caller();
        script.set(6);
        assert_eq!(t.create(&c, input.clone(), "one").await.0, 503, "{kind:?}");
        let receipt_before = script.refs.lock().await.values().next().unwrap().0;
        script.set(0);
        Ticker::new(f.state.clone(), clock(), 10, 100)
            .tick()
            .await
            .unwrap();
        let replay = t.create(&c, input, "one").await;
        assert_eq!(replay.0, 201, "{kind:?}");
        assert_eq!(replay.1["reservation_id"], receipt_before.to_string());
        assert_eq!(script.refs.lock().await.len(), 1);
        assert_eq!(Script::count(&script.releases), 0);
    }
}
#[tokio::test]
async fn forced_release_reconciles_unfenced_and_fenced_references() {
    use bss_products_sdk::models::ReferenceState;
    for kind in KINDS {
        for fenced in [false, true] {
            let (f, script, t, input) = setup(kind).await;
            let c = f.caller();
            let first = t.create(&c, input, "one").await;
            assert_eq!(first.0, 201);
            for value in script.refs.lock().await.values_mut() {
                value.1 = ReferenceState::Released;
            }
            if fenced {
                script.set(4);
            }
            Ticker::new(f.state.clone(), clock(), 1, 1)
                .tick()
                .await
                .unwrap();
            let read = t.read(&c, &first.1["id"]).await;
            assert_eq!(
                read["reference_state"],
                if fenced { "lost" } else { "confirmed" },
                "{kind:?}: {read:?}"
            );
            if !fenced {
                assert_ne!(first.1["reservation_id"], read["reservation_id"]);
            }
            if t.kind == Kind::Entry {
                // A lost entry takes no new price, and the export shows the same state.
                if fenced {
                    let scope = AccessScope::for_tenant(f.ctx.subject_tenant_id());
                    let conn = f.db.conn().unwrap();
                    let entry = price_book_entry_repo::find(
                        &conn,
                        &scope,
                        f.ctx.subject_tenant_id(),
                        id_of(&first.1["id"]),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    let error = price_repo::insert(&conn, &scope, entry_support::price(&entry))
                        .await
                        .unwrap_err();
                    assert!(error.to_string().contains("ENTRY_REFERENCE_LOST"));
                }
                let export = f
                    .call(
                        "GET",
                        &format!("/price-books/{}/export", t.book),
                        json!({}),
                        None,
                        None,
                    )
                    .await;
                assert_eq!(
                    export.1["entries"][0]["entry"]["reference_state"],
                    read["reference_state"]
                );
            }
        }
    }
}
#[tokio::test]
async fn operator_list_filters_paginates_and_validates_query() {
    for kind in KINDS {
        let (f, script, t, input) = setup(kind).await;
        script.set(6);
        t.create(&f.caller(), input, "one").await;
        let result = f
            .call(
                "GET",
                "/reference-ops?state=written&limit=1",
                json!({}),
                None,
                None,
            )
            .await;
        assert_eq!(result.0, 200, "{result:?}");
        assert_eq!(result.1["items"].as_array().unwrap().len(), 1);
        // The op names its reference as (ref_kind, ref_id) (D-412).
        let item = &result.1["items"][0];
        assert_eq!(item["kind"], "create", "{item}");
        assert_eq!(item["ref_kind"], t.ref_kind(), "{item}");
        assert!(
            item["ref_id"].as_str().unwrap().parse::<Uuid>().is_ok(),
            "{item}"
        );
        assert!(item.get("price_book_entry_id").is_none(), "{item}");
        let id = result.1["items"][0]["op_id"].as_str().unwrap();
        assert_eq!(
            f.call(
                "GET",
                &format!("/reference-ops?cursor={id}&limit=1"),
                json!({}),
                None,
                None
            )
            .await
            .1["items"],
            json!([])
        );
        assert_eq!(
            f.call("GET", "/reference-ops?state=wat", json!({}), None, None)
                .await
                .0,
            400
        );
        assert_eq!(
            f.call("GET", "/reference-ops?limit=0", json!({}), None, None)
                .await
                .0,
            400
        );
    }
}
#[test]
fn backoff_is_exponential_and_bounded() {
    use bss_pricing::infra::reference_work::backoff;
    for (attempts, seconds) in [
        (0, 1),
        (1, 2),
        (8, 256),
        (9, 300),
        (10, 300),
        (i32::MAX, 300),
    ] {
        assert_eq!(backoff(attempts).whole_seconds(), seconds);
    }
    assert_eq!(OpState::Done.as_str(), "done");
}

/// Every lost-reference envelope of the target's kind in the fixture's outbox.
async fn lost_events(f: &Fixture, t: &Target) -> Vec<serde_json::Value> {
    entry_support::outbox_events(&f.dsn, t.lost_event().0).await
}
#[tokio::test]
async fn a_reservation_released_before_confirm_is_rereserved_not_lost() {
    // An operator force-released the reservation between Tx B and the confirm. The SKU is not
    // fenced, so the reference is re-reserved (D-401); the create is answered, never as `lost`.
    for kind in KINDS {
        let (f, script, t, input) = setup(kind).await;
        let c = f.caller();
        script.set(7);
        let created = t.create(&c, input.clone(), "one").await;
        assert_eq!(created.0, 201, "{kind:?}: {created:?}");
        assert_eq!(created.1["reference_state"], "confirmation_pending");
        assert_eq!(t.create(&c, input, "one").await, created);
        let scope = AccessScope::for_tenant(f.ctx.subject_tenant_id());
        let open = ops::page(
            &f.db.conn().unwrap(),
            &scope,
            f.ctx.subject_tenant_id(),
            Some(OpState::Reserving),
            None,
            10,
        )
        .await
        .unwrap();
        assert_eq!(open.len(), 1, "one rereserve op is open");
        assert_eq!(open[0].kind, "rereserve");
        assert_eq!(open[0].ref_kind, t.ref_kind());
        assert_eq!(
            open[0].ref_id.to_string(),
            created.1["id"].as_str().unwrap()
        );
        script.set(0);
        Ticker::new(f.state.clone(), clock(), 10, 100)
            .tick()
            .await
            .unwrap();
        let read = t.read(&c, &created.1["id"]).await;
        assert_eq!(read["reference_state"], "confirmed", "{kind:?}: {read}");
        assert_ne!(read["reservation_id"], created.1["reservation_id"]);
        assert!(lost_events(&f, &t).await.is_empty());
        assert_eq!(Script::count(&script.releases), 0);
    }
}
#[tokio::test]
async fn a_released_reference_is_lost_only_behind_a_fence_and_found_again_when_it_lifts() {
    for kind in KINDS {
        let (f, script, t, input) = setup(kind).await;
        let c = f.caller();
        script.set(7);
        let created = t.create(&c, input, "one").await;
        assert_eq!(created.1["reference_state"], "confirmation_pending");
        // The SKU is now fenced: the re-reservation is refused SKU_FENCED, so it is lost.
        script.set(4);
        let mut ticker = Ticker::new(f.state.clone(), clock(), 10, 1);
        ticker.tick().await.unwrap();
        let read = t.read(&c, &created.1["id"]).await;
        assert_eq!(read["reference_state"], "lost", "{kind:?}: {read}");
        let events = lost_events(&f, &t).await;
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["data"][t.lost_event().1], created.1["id"]);
        assert_eq!(
            events[0]["tenant_id"],
            f.ctx.subject_tenant_id().to_string()
        );
        // Still fenced: reconciliation leaves the lost reference alone and announces nothing
        // twice.
        let reserves = Script::count(&script.reserve_calls);
        ticker.tick().await.unwrap();
        assert_eq!(Script::count(&script.reserve_calls), reserves);
        assert_eq!(lost_events(&f, &t).await.len(), 1);
        // The fence lifts: reconciliation re-reserves the lost reference.
        script.set(0);
        ticker.tick().await.unwrap();
        let read = t.read(&c, &created.1["id"]).await;
        assert_eq!(read["reference_state"], "confirmed", "{kind:?}: {read}");
        assert_ne!(read["reservation_id"], created.1["reservation_id"]);
        assert_eq!(lost_events(&f, &t).await.len(), 1);
    }
}

/// A SKU in retire review keeps its lifecycle and sets `retire_pending` (P-D-248). A lost
/// reference of that SKU is not re-reserved: `admits` refuses it beside a type-change fence.
#[tokio::test]
async fn a_retire_pending_sku_is_not_rereserved() {
    for kind in KINDS {
        let (f, script, t, input) = setup(kind).await;
        let c = f.caller();
        script.set(7);
        let created = t.create(&c, input, "one").await;
        assert_eq!(created.1["reference_state"], "confirmation_pending");
        script.set(22);
        let mut ticker = Ticker::new(f.state.clone(), clock(), 10, 1);
        ticker.tick().await.unwrap();
        let read = t.read(&c, &created.1["id"]).await;
        assert_eq!(read["reference_state"], "lost", "{kind:?}: {read}");
        let reserves = Script::count(&script.reserve_calls);
        ticker.tick().await.unwrap();
        assert_eq!(
            Script::count(&script.reserve_calls),
            reserves,
            "{kind:?}: a retire-pending SKU mints no rereserve"
        );
    }
}
#[tokio::test]
async fn a_rereserve_refused_for_another_reason_is_retried_never_lost() {
    for kind in KINDS {
        let (f, script, t, input) = setup(kind).await;
        let c = f.caller();
        script.set(7);
        let created = t.create(&c, input, "one").await;
        assert_eq!(created.1["reference_state"], "confirmation_pending");
        script.set(16);
        Ticker::new(f.state.clone(), clock(), 10, 100)
            .tick()
            .await
            .unwrap();
        let read = t.read(&c, &created.1["id"]).await;
        assert_eq!(
            read["reference_state"], "confirmation_pending",
            "{kind:?}: {read}"
        );
        assert!(lost_events(&f, &t).await.is_empty());
        let scope = AccessScope::for_tenant(f.ctx.subject_tenant_id());
        let open = ops::page(
            &f.db.conn().unwrap(),
            &scope,
            f.ctx.subject_tenant_id(),
            Some(OpState::Reserving),
            None,
            10,
        )
        .await
        .unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].attempts, 1, "retried with backoff, not cancelled");
    }
}

#[tokio::test]
async fn cancelling_releasing_backoff_and_threshold_never_drop_work() {
    for kind in KINDS {
        for cancelling in [false, true] {
            let (f, script, t, input) = setup(kind).await;
            let c = f.caller();
            if cancelling {
                script.set(14);
            }
            let first = t.create(&c, input.clone(), "one").await;
            if cancelling {
                assert_eq!(first.0, 503, "{kind:?}: {first:?}");
            } else {
                assert_eq!(first.0, 201, "{kind:?}: {first:?}");
                script.set(12);
                // The delete committed; its release proceeds through the op and the ticker.
                assert_eq!(t.delete(&c, &first.1["id"]).await.0, 204);
            }
            let scope = AccessScope::for_tenant(f.ctx.subject_tenant_id());
            let mut now = clock().now();
            for attempt in 2..=11 {
                Ticker::new(f.state.clone(), Arc::new(FixedClock(now)), 1, 100)
                    .tick()
                    .await
                    .unwrap();
                let ops = ops::page(
                    &f.db.conn().unwrap(),
                    &scope,
                    f.ctx.subject_tenant_id(),
                    Some(if cancelling {
                        OpState::Cancelling
                    } else {
                        OpState::Releasing
                    }),
                    None,
                    10,
                )
                .await
                .unwrap();
                assert_eq!(ops.len(), 1);
                assert_eq!(ops[0].attempts, attempt);
                assert_eq!(
                    ops[0].next_attempt_at,
                    now + bss_pricing::infra::reference_work::backoff(attempt)
                );
                now += time::Duration::seconds(301);
            }
            script.set(0);
            Ticker::new(f.state.clone(), Arc::new(FixedClock(now)), 1, 100)
                .tick()
                .await
                .unwrap();
            assert!(
                ops::due(&f.db.conn().unwrap(), &scope, now, 10)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(Script::count(&script.releases), 1);
            if cancelling {
                let response = t.create(&c, input, "one").await;
                // An item's re-read answers the item door's 400 (R-2, D-403); an entry's, 409.
                let (status, code) = match kind {
                    Kind::Entry => (409, "BUNDLE_SKU_NOT_PRICEABLE"),
                    Kind::Item => (400, "ITEM_BUNDLE_SKU"),
                };
                assert_eq!(response.0, status, "{kind:?}: {response:?}");
                assert!(response.1.to_string().contains(code), "{response:?}");
            }
        }
    }
}
/// A create cancelled for its SKU (a bundle) whose release then fails keeps its refusal: the
/// retries do not overwrite the recorded code with `REGISTRY_UNAVAILABLE`, and the op ends `done`
/// naming the refusal it met, as `/reference-ops` shows it (whole-branch review PS-05).
#[tokio::test]
async fn a_cancelled_create_keeps_its_refusal_through_a_failed_release() {
    for kind in KINDS {
        let (f, script, t, input) = setup(kind).await;
        let c = f.caller();
        // A bundle SKU: the re-read refuses the create, and every release fails.
        script.set(14);
        assert_eq!(t.create(&c, input.clone(), "one").await.0, 503, "{kind:?}");
        let refusal = match kind {
            Kind::Entry => "BUNDLE_SKU_NOT_PRICEABLE",
            Kind::Item => "ITEM_BUNDLE_SKU",
        };
        let scope = AccessScope::for_tenant(f.ctx.subject_tenant_id());
        let page = |state| {
            let (db, scope, tenant) = (f.db.clone(), scope.clone(), f.ctx.subject_tenant_id());
            async move {
                ops::page(&db.conn().unwrap(), &scope, tenant, Some(state), None, 10)
                    .await
                    .unwrap()
            }
        };
        let mut now = clock().now();
        for _ in 0..2 {
            Ticker::new(f.state.clone(), Arc::new(FixedClock(now)), 1, 100)
                .tick()
                .await
                .unwrap();
            let open = page(OpState::Cancelling).await;
            assert_eq!(open.len(), 1, "{kind:?}");
            assert_eq!(
                open[0].last_error.as_deref(),
                Some(refusal),
                "{kind:?}: a failed release keeps the refusal"
            );
            now += time::Duration::seconds(301);
        }
        script.set(0);
        Ticker::new(f.state.clone(), Arc::new(FixedClock(now)), 1, 100)
            .tick()
            .await
            .unwrap();
        let done = page(OpState::Done).await;
        assert_eq!(done.len(), 1, "{kind:?}");
        assert_eq!(
            done[0].last_error.as_deref(),
            Some(refusal),
            "{kind:?}: the op ends naming the refusal it met"
        );
    }
}
#[tokio::test]
async fn door_losing_completion_race_rereads_the_tickers_receipt() {
    for kind in KINDS {
        let (f, script, t, input) = setup(kind).await;
        let c = f.caller();
        script.set(2);
        let mut door = Box::pin(t.create(&c, input.clone(), "one"));
        tokio::select! { result = &mut door => panic!("did not park: {result:?}"), () = script.parked.notified() => {} }
        script.set(0);
        Ticker::new(f.state.clone(), clock(), 1, 100)
            .tick()
            .await
            .unwrap();
        script.resume.notify_one();
        let result = door.await;
        assert_eq!(result.0, 201, "{kind:?}: {result:?}");
        assert_eq!(result, t.create(&c, input, "one").await);
    }
}
#[tokio::test]
async fn reconciliation_is_periodic_bounded_and_uses_the_system_actor() {
    for kind in KINDS {
        let (f, script, t, _) = setup(kind).await;
        let c = f.caller();
        let mut created = Vec::new();
        for n in 0..3 {
            let result = t.create(&c, t.input(&c).await, &format!("{n}")).await;
            assert_eq!(result.0, 201);
            created.push(result.1);
        }
        for value in script.refs.lock().await.values_mut() {
            value.1 = bss_products_sdk::models::ReferenceState::Released;
        }
        let mut ticker = Ticker::new(f.state.clone(), clock(), 1, 2);
        for tick in 1_usize..=6 {
            ticker.tick().await.unwrap();
            let repaired = script
                .refs
                .lock()
                .await
                .values()
                .filter(|(_, state)| *state == bss_products_sdk::models::ReferenceState::Confirmed)
                .count();
            // Reconciliation runs every second tick and repairs one reference per run.
            assert_eq!(repaired, tick.div_euclid(2), "{kind:?}");
        }
        let actors = script.actors.lock().await;
        assert_eq!(&actors[3..], &[bss_products_sdk::PRICING_SYSTEM_ACTOR; 3]);
        assert_eq!(created.len(), 3);
    }
}
#[tokio::test]
async fn a_live_door_owns_its_op_for_the_grace_period() {
    // The ticker runs every second. An op a door is still driving must not be due
    // to it, or every create races a second registry caller under another actor.
    for kind in KINDS {
        let (f, script, t, input) = setup(kind).await;
        let c = f.caller();
        script.set(1);
        let mut door = Box::pin(t.create(&c, input, "live"));
        tokio::select! { result = &mut door => panic!("door did not park: {result:?}"), () = script.parked.notified() => {} }
        let scope = AccessScope::for_tenant(f.ctx.subject_tenant_id());
        let now = time::OffsetDateTime::now_utc();
        let conn = f.db.conn().unwrap();
        assert!(
            ops::due(&conn, &scope, now, 10).await.unwrap().is_empty(),
            "{kind:?}: a fresh op is not due while its door may still be driving it"
        );
        let later =
            now + bss_pricing::infra::reference_work::IN_FLIGHT_GRACE + time::Duration::seconds(1);
        assert_eq!(
            ops::due(&conn, &scope, later, 10).await.unwrap().len(),
            1,
            "an abandoned op becomes due"
        );
        drop(door);
    }
}

#[tokio::test]
async fn one_tenants_states_failure_skips_only_that_tenant_and_the_cursor_moves_on() {
    use entry_support::{app_for, user_of};
    for kind in KINDS {
        let (f, script, t, input) = setup(kind).await;
        let first = t.create(&f.caller(), input, "a").await;
        assert_eq!(first.0, 201, "{first:?}");
        // A second tenant on the same pricing database, created after the first.
        let other = Uuid::new_v4();
        let (app, ctx) = (app_for(f.state.clone(), other), user_of(other));
        let theirs = Caller {
            app: &app,
            state: &f.state,
            ctx: &ctx,
        };
        let t2 = Target::new(kind, &theirs).await;
        let second = t2.create(&theirs, t2.input(&theirs).await, "b").await;
        assert_eq!(second.0, 201, "{second:?}");
        for value in script.refs.lock().await.values_mut() {
            value.1 = bss_products_sdk::models::ReferenceState::Released;
        }
        *script.states_down_for.lock().unwrap() = Some(f.ctx.subject_tenant_id());
        // One reference per reconciliation pass, every pass.
        let mut ticker = Ticker::new(f.state.clone(), clock(), 1, 1);
        ticker.tick().await.unwrap();
        ticker.tick().await.unwrap();
        let (a_state, a_receipt) = t.stored(&f.state, id_of(&first.1["id"])).await.unwrap();
        let (b_state, b_receipt) = t2.stored(&f.state, id_of(&second.1["id"])).await.unwrap();
        assert_eq!(a_state, "confirmed");
        assert_eq!(
            a_receipt.unwrap().to_string(),
            first.1["reservation_id"].as_str().unwrap(),
            "{kind:?}: the failing tenant was skipped"
        );
        assert_eq!(b_state, "confirmed");
        assert_ne!(
            b_receipt.unwrap().to_string(),
            second.1["reservation_id"].as_str().unwrap(),
            "{kind:?}: the next tenant was still reconciled"
        );
    }
}
#[tokio::test]
async fn a_reservation_products_does_not_know_is_treated_as_released() {
    for kind in KINDS {
        let (f, script, t, input) = setup(kind).await;
        let c = f.caller();
        let created = t.create(&c, input, "one").await;
        assert_eq!(created.0, 201, "{created:?}");
        // Products' database was restored from a backup that predates this reservation.
        script.refs.lock().await.clear();
        Ticker::new(f.state.clone(), clock(), 10, 1)
            .tick()
            .await
            .unwrap();
        let read = t.read(&c, &created.1["id"]).await;
        assert_eq!(read["reference_state"], "confirmed", "{kind:?}: {read}");
        assert_ne!(read["reservation_id"], created.1["reservation_id"]);
    }
}
// Behaviour LOW-1: Products restored from a backup older than the reserve answers 404 at the
// confirm. That reservation is gone, exactly as if it was released before its confirm: the
// create is answered, the reference is re-reserved (D-401), and it is lost only behind a fence.
#[tokio::test]
async fn a_confirm_answered_404_is_a_reservation_released_before_confirm() {
    for kind in KINDS {
        for fenced in [false, true] {
            let (f, script, t, input) = setup(kind).await;
            let c = f.caller();
            script.set(20);
            let created = t.create(&c, input.clone(), "one").await;
            assert_eq!(created.0, 201, "{kind:?} {fenced}: {created:?}");
            assert_eq!(created.1["reference_state"], "confirmation_pending");
            assert_eq!(
                t.create(&c, input, "one").await,
                created,
                "the key is answered"
            );
            let scope = AccessScope::for_tenant(f.ctx.subject_tenant_id());
            let open = ops::page(
                &f.db.conn().unwrap(),
                &scope,
                f.ctx.subject_tenant_id(),
                Some(OpState::Reserving),
                None,
                10,
            )
            .await
            .unwrap();
            assert_eq!(open.len(), 1, "one rereserve op, no confirm retried");
            assert_eq!(open[0].kind, "rereserve");
            script.set(if fenced { 4 } else { 0 });
            Ticker::new(f.state.clone(), clock(), 10, 100)
                .tick()
                .await
                .unwrap();
            let read = t.read(&c, &created.1["id"]).await;
            if fenced {
                assert_eq!(read["reference_state"], "lost", "{kind:?}: {read}");
                assert_eq!(lost_events(&f, &t).await.len(), 1);
            } else {
                assert_eq!(read["reference_state"], "confirmed", "{kind:?}: {read}");
                assert_ne!(read["reservation_id"], created.1["reservation_id"]);
                assert!(lost_events(&f, &t).await.is_empty());
            }
        }
    }
}
// Behaviour LOW-1 at release: a reservation Products does not know is released already. The
// delete's release op finishes instead of retrying forever.
#[tokio::test]
async fn a_release_answered_404_counts_as_released() {
    for kind in KINDS {
        let (f, script, t, input) = setup(kind).await;
        let c = f.caller();
        let created = t.create(&c, input, "one").await;
        assert_eq!(created.0, 201, "{created:?}");
        assert_eq!(created.1["reference_state"], "confirmed");
        // Products' database was restored from a backup that predates this reservation.
        script.refs.lock().await.clear();
        script.set(20);
        let deleted = t.delete(&c, &created.1["id"]).await;
        assert_eq!(deleted.0, 204, "{kind:?}: {deleted:?}");
        let scope = AccessScope::for_tenant(f.ctx.subject_tenant_id());
        let releasing = ops::page(
            &f.db.conn().unwrap(),
            &scope,
            f.ctx.subject_tenant_id(),
            Some(OpState::Releasing),
            None,
            10,
        )
        .await
        .unwrap();
        assert!(
            releasing.is_empty(),
            "the release op is done, not retried: {releasing:?}"
        );
        let done = ops::page(
            &f.db.conn().unwrap(),
            &scope,
            f.ctx.subject_tenant_id(),
            Some(OpState::Done),
            None,
            10,
        )
        .await
        .unwrap();
        assert!(
            done.iter()
                .any(|op| op.kind == "delete" && op.ref_kind == t.ref_kind()),
            "{done:?}"
        );
        assert_eq!(
            Script::count(&script.releases),
            0,
            "nothing was released twice"
        );
    }
}
