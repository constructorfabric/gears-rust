//! Exhaustive state/event contract for durable reference work.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use bss_pricing::domain::{
    price_book_entry::OpState,
    reference_op::{Effect, Event, Op, next},
};
use uuid::Uuid;
#[test]
fn every_state_event_pair_is_typed_and_never_panics() {
    use Effect::{Complete, Confirm, ReadSku, Release, Rereserve, Retry};
    use OpState::{Cancelling, Done, Releasing, Reserving, Written};
    let id = Uuid::from_u128(1);
    let events = [
        Event::Reserved { id },
        Event::ReserveRefused {
            code: "SKU_FENCED".into(),
        },
        Event::RegistryUnavailable,
        Event::SkuRefused {
            code: "BUNDLE_SKU_NOT_PRICEABLE".into(),
        },
        Event::Written,
        Event::Confirmed,
        Event::ConfirmFailed,
        Event::ReleasedOnConfirm,
        Event::Released,
        Event::ReleaseFailed,
        Event::ReservationUnknown,
    ];
    let table = [
        (
            Reserving,
            vec![
                Some((Reserving, ReadSku)),
                Some((Cancelling, Release)),
                Some((Reserving, Retry)),
                Some((Cancelling, Release)),
                Some((Written, Confirm)),
                None,
                None,
                None,
                None,
                None,
                // The door gave up before the write: cancel, releasing the receipt in hand.
                Some((Cancelling, Release)),
            ],
        ),
        (
            Written,
            vec![
                None,
                None,
                None,
                None,
                None,
                Some((Done, Complete)),
                Some((Written, Retry)),
                Some((Done, Rereserve)),
                None,
                None,
                None,
            ],
        ),
        (
            Cancelling,
            vec![
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some((Done, Complete)),
                Some((Cancelling, Retry)),
                None,
            ],
        ),
        (
            Releasing,
            vec![
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some((Done, Complete)),
                Some((Releasing, Retry)),
                None,
            ],
        ),
        (Done, vec![None; 11]),
    ];
    let mut count = 0;
    for (state, expected) in table {
        for (event, expected) in events.iter().zip(expected) {
            let op = Op {
                state,
                reservation_id: Some(id),
                refusal: None,
            };
            let got = next(op, event.clone());
            if let Some((state, effect)) = expected {
                let (op, effects) = got.unwrap();
                assert_eq!(op.state, state);
                assert_eq!(effects, vec![effect]);
            } else {
                let error = got.unwrap_err();
                assert_eq!(error.state, state);
            }
            count += 1;
        }
    }
    assert_eq!(count, 55);
}
#[test]
fn receipts_and_refusals_survive_until_completion() {
    let id = Uuid::new_v4();
    let op = Op {
        state: OpState::Reserving,
        reservation_id: None,
        refusal: None,
    };
    let (op, _) = next(op, Event::Reserved { id }).unwrap();
    assert_eq!(op.reservation_id, Some(id));
    let (op, _) = next(
        op,
        Event::SkuRefused {
            code: "SKU_DRAFT".into(),
        },
    )
    .unwrap();
    let (op, _) = next(op, Event::Released).unwrap();
    assert_eq!(op.reservation_id, Some(id));
    assert_eq!(op.refusal.as_deref(), Some("SKU_DRAFT"));
}
#[test]
fn an_unknown_reserve_outcome_cancels_only_work_without_a_receipt() {
    // The door (or a crash) never learned whether Products reserved: the op is cancelled with
    // no receipt, and the cancellation must find and release whatever reservation exists.
    let (op, effects) = next(
        Op {
            state: OpState::Reserving,
            reservation_id: None,
            refusal: None,
        },
        Event::ReservationUnknown,
    )
    .unwrap();
    assert_eq!(op.state, OpState::Cancelling);
    assert_eq!(op.reservation_id, None);
    assert_eq!(op.refusal.as_deref(), Some("RESERVATION_UNKNOWN"));
    assert_eq!(effects, vec![Effect::Release]);
    let (op, _) = next(op, Event::Released).unwrap();
    assert_eq!(op.state, OpState::Done);
}

/// A stored plan item op keeps its create input in a shape of its own, not the request's (the
/// whole-branch review's PS-19): an op whose input carries a field this build does not know still
/// reads, and so does one written before a field was added, so a change of the wire cannot make
/// the ops in flight corrupt.
#[test]
fn a_stored_item_input_reads_with_a_field_it_does_not_know_or_lacks() {
    use bss_pricing::infra::reference_work::{Target, Work};
    let stored = |input: serde_json::Value| {
        serde_json::json!({
            "target": {"plan_item": {"revision_id": Uuid::nil(), "input": input}},
            "correlation": Uuid::nil(),
            "refusal": null,
            "receipt": null
        })
        .to_string()
    };
    let entry = Uuid::from_u128(7);
    // An op stored before D-467 carries treatment and the quantities: they are ignored.
    let before_d467 = stored(serde_json::json!({
        "sku_id": Uuid::nil(), "price_book_entry_id": entry, "treatment": "paid",
        "included_qty": null, "qty_min": 1, "a_later_field": "x"
    }));
    let work: Work = serde_json::from_str(&before_d467).unwrap();
    let Target::PlanItem {
        input: Some(input), ..
    } = work.target
    else {
        panic!("a plan item target");
    };
    assert_eq!(input.price_book_entry_id, Some(entry));
    let earlier = stored(serde_json::json!({"sku_id": Uuid::nil(), "treatment": "included"}));
    let work: Work = serde_json::from_str(&earlier).unwrap();
    let Target::PlanItem {
        input: Some(input), ..
    } = work.target
    else {
        panic!("a plan item target");
    };
    assert_eq!(
        input.price_book_entry_id, None,
        "a legacy included item's op"
    );
}
