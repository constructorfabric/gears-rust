#![allow(clippy::unwrap_used, clippy::expect_used)]

use bss_rating_core::engine::current::input::{BillingTiming, RatingWindowKind};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{ReadFact, is_positive_decimal, read_fact, read_key};
use crate::domain::fact::model::{Coverage, FactKind, Obligation, Timing};
use crate::domain::inbox::PayloadRefusal;
use crate::test_providers::subscriptions::{
    FactSpec, FakeSubscriptionsBilling, lot, recurring_binding, term, usage_binding,
};

const OCT: &str = "2026-10-01T00:00:00Z";
const NOV: &str = "2026-11-01T00:00:00Z";
const METER: &str = "products.derived/cloudlet@1";

fn party() -> FakeSubscriptionsBilling {
    FakeSubscriptionsBilling::new(Uuid::from_u128(0x5e11))
}

fn recurring() -> FactSpec {
    let item = Uuid::from_u128(0x17);
    let binding = recurring_binding(
        item,
        Uuid::from_u128(0x9),
        Decimal::new(10, 0),
        BillingTiming::Advance,
    );
    party().fact("recurring", item, OCT, NOV).chargeable(
        OCT,
        NOV,
        term("t1", binding),
        vec![lot("A", "10", "t1")],
    )
}

fn usage() -> FactSpec {
    let item = Uuid::from_u128(0x18);
    let binding = usage_binding(
        item,
        Uuid::from_u128(0xa),
        RatingWindowKind::CalendarHourUtc,
        METER,
        None,
    );
    let mut spec = party().fact("usage", item, OCT, NOV);
    "arrears".clone_into(&mut spec.timing);
    spec.chargeable(OCT, NOV, term("u1", binding), Vec::new())
}

fn refused(payload: &Value) -> (PayloadRefusal, String) {
    match read_fact(payload).unwrap() {
        ReadFact::Refused {
            refusal, detail, ..
        } => (refusal, detail),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn set(payload: &mut Value, path: &[&str], value: Value) {
    let mut at = payload;
    for part in &path[..path.len() - 1] {
        at = if let Ok(index) = part.parse::<usize>() {
            &mut at[index]
        } else {
            &mut at[*part]
        };
    }
    at[path[path.len() - 1]] = value;
}

#[test]
fn a_recurring_fact_reads_with_its_slice_lots_and_bound_term() {
    let ReadFact::Accepted(fact) = read_fact(&recurring().payload()).unwrap() else {
        panic!("not accepted");
    };
    assert_eq!(fact.kind, FactKind::Recurring);
    assert_eq!(fact.obligation, Obligation::Active);
    assert_eq!(fact.timing, Timing::Advance);
    assert_eq!(fact.slices.len(), 1);
    assert_eq!(fact.slices[0].coverage, Coverage::Chargeable);
    assert_eq!(fact.slices[0].lots[0].quantity, "10");
    assert_eq!(fact.terms.len(), 1);
    assert_eq!(fact.terms[0].binding_digest.as_str().len(), 64);
}

#[test]
fn a_usage_fact_reads_with_its_policy() {
    let ReadFact::Accepted(fact) = read_fact(&usage().payload()).unwrap() else {
        panic!("not accepted");
    };
    let policy = fact.terms[0].binding.usage_policy.as_ref().unwrap();
    assert_eq!(policy.rating_window, RatingWindowKind::CalendarHourUtc);
}

#[test]
fn v60_a_sub_second_period_start_is_refused_never_truncated() {
    let mut spec = recurring();
    "2026-11-01T00:00:00.5Z".clone_into(&mut spec.anchor_at);
    let (refusal, detail) = refused(&spec.payload());
    assert_eq!(refusal, PayloadRefusal::InstantNotWholeSecond);
    assert!(detail.contains("anchor_at"), "{detail}");
}

#[test]
fn v60_a_sub_second_quantity_change_is_refused() {
    let item = Uuid::from_u128(0x17);
    let binding = recurring_binding(
        item,
        Uuid::from_u128(0x9),
        Decimal::new(10, 0),
        BillingTiming::Advance,
    );
    let cut = "2026-10-15T12:00:00.250Z";
    let spec = party()
        .fact("recurring", item, OCT, NOV)
        .chargeable(
            OCT,
            cut,
            term("t1", binding.clone()),
            vec![lot("A", "10", "t1")],
        )
        .chargeable(cut, NOV, term("t1", binding), vec![lot("A", "12", "t1")]);
    let (refusal, detail) = refused(&spec.payload());
    assert_eq!(refusal, PayloadRefusal::InstantNotWholeSecond);
    assert!(detail.contains("slices[0].to"), "{detail}");
}

#[test]
fn a_gap_in_the_timeline_is_invalid() {
    let mut payload = recurring().payload();
    set(
        &mut payload,
        &["slices", "0", "to"],
        json!("2026-10-30T00:00:00Z"),
    );
    let (refusal, detail) = refused(&payload);
    assert_eq!(refusal, PayloadRefusal::Invalid);
    assert!(detail.contains("end of the served extent"), "{detail}");
}

#[test]
fn a_chargeable_slice_without_terms_is_invalid() {
    let mut payload = recurring().payload();
    set(&mut payload, &["slices", "0", "terms"], json!([]));
    set(&mut payload, &["slices", "0", "lots"], json!([]));
    assert_eq!(refused(&payload).0, PayloadRefusal::Invalid);
}

#[test]
fn a_lot_naming_an_absent_term_is_invalid() {
    let mut payload = recurring().payload();
    set(
        &mut payload,
        &["slices", "0", "lots", "0", "terms_ref"],
        json!("t9"),
    );
    assert_eq!(refused(&payload).0, PayloadRefusal::Invalid);
}

#[test]
fn a_binding_whose_money_digest_does_not_verify_is_invalid() {
    let mut payload = recurring().payload();
    set(
        &mut payload,
        &["terms", "0", "binding", "price", "money_digest"],
        json!("ab".repeat(32)),
    );
    let (refusal, detail) = refused(&payload);
    assert_eq!(refusal, PayloadRefusal::Invalid);
    assert!(detail.contains("INVALID_BINDING"), "{detail}");
}

#[test]
fn a_term_of_another_catalog_tenant_is_invalid() {
    let mut payload = recurring().payload();
    set(
        &mut payload,
        &["terms", "0", "catalog_tenant_id"],
        json!(Uuid::from_u128(0xbad)),
    );
    assert_eq!(refused(&payload).0, PayloadRefusal::Invalid);
}

#[test]
fn a_binding_billed_at_another_timing_than_the_fact_is_invalid() {
    let mut payload = recurring().payload();
    set(&mut payload, &["timing"], json!("arrears"));
    assert_eq!(refused(&payload).0, PayloadRefusal::Invalid);
}

#[test]
fn a_zeroed_obligation_carries_no_slice() {
    let mut spec = recurring();
    "zeroed".clone_into(&mut spec.obligation);
    assert_eq!(refused(&spec.payload()).0, PayloadRefusal::Invalid);
    spec.slices.clear();
    spec.terms.clear();
    assert!(matches!(
        read_fact(&spec.payload()).unwrap(),
        ReadFact::Accepted(_)
    ));
}

#[test]
fn an_unresolved_slice_carries_a_reason_and_no_term() {
    let item = Uuid::from_u128(0x18);
    let binding = usage_binding(
        item,
        Uuid::from_u128(0xa),
        RatingWindowKind::CalendarHourUtc,
        METER,
        None,
    );
    let mid = "2026-10-10T00:00:00Z";
    let mut spec = party()
        .fact("usage", item, OCT, NOV)
        .chargeable(OCT, mid, term("u1", binding), Vec::new())
        .unresolved(mid, NOV, "continuation_missing");
    "arrears".clone_into(&mut spec.timing);
    let ReadFact::Accepted(fact) = read_fact(&spec.payload()).unwrap() else {
        panic!("not accepted");
    };
    assert_eq!(
        fact.slices[1].coverage,
        Coverage::Unresolved {
            reason: "continuation_missing".to_owned()
        }
    );
}

#[test]
fn another_profile_is_unsupported_and_keeps_its_key() {
    let mut payload = recurring().payload();
    set(&mut payload, &["profile"], json!("subscriptions-rating/2"));
    let ReadFact::Unsupported { fact } = read_fact(&payload).unwrap() else {
        panic!("not unsupported");
    };
    assert_eq!(fact.fact_version, 1);
}

#[test]
fn an_unknown_field_is_invalid() {
    let mut payload = recurring().payload();
    set(&mut payload, &["surprise"], json!(true));
    assert_eq!(refused(&payload).0, PayloadRefusal::Invalid);
}

#[test]
fn a_payload_without_a_key_is_unkeyable() {
    let mut payload = recurring().payload();
    set(&mut payload, &["fact_version"], json!(0));
    assert!(read_key(&payload).is_err());
    let mut payload = recurring().payload();
    set(
        &mut payload,
        &["billing_group", "seller_tenant_id"],
        json!("nope"),
    );
    assert!(read_fact(&payload).is_err());
}

#[test]
fn quantities_are_positive_canonical_decimals() {
    for good in ["1", "10", "0.5", "12.25", "100.0"] {
        assert!(is_positive_decimal(good), "{good}");
    }
    for bad in [
        "0",
        "0.0",
        "-1",
        "01",
        "1.",
        ".5",
        "1e3",
        " 1",
        "",
        "1.12345678901234567890123456789",
    ] {
        assert!(!is_positive_decimal(bad), "{bad}");
    }
    assert!(is_positive_decimal(&"9".repeat(60)));
    assert!(!is_positive_decimal(&"9".repeat(61)));
}
