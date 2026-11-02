#![allow(clippy::unwrap_used, clippy::expect_used)]

use bss_pricing_sdk::read::{BindingSelection, ResolvedBindings, ResolvedCell};
use bss_rating_core::engine::current::input::{BillingTiming, PricePin};
use bss_rating_sdk::errors::UsageResource;
use rust_decimal::Decimal;
use time::Date;
use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

use super::{ResolveQuestion, answer_digest, cell_digest, convert, is_unavailable, refusal_code};
use crate::test_providers::subscriptions::recurring_binding;

fn pin(item: u128, price: u128) -> PricePin {
    PricePin {
        item_id: Uuid::from_u128(item),
        dimension_value: None,
        price_id: Uuid::from_u128(price),
    }
}

fn question(pins: Vec<PricePin>, date: Date) -> ResolveQuestion {
    ResolveQuestion {
        catalog_tenant_id: Uuid::from_u128(0xc),
        revision_id: Uuid::from_u128(0xd),
        date,
        item_id: None,
        pins,
    }
}

fn day(ordinal: u16) -> Date {
    Date::from_ordinal_date(2026, ordinal).unwrap()
}

#[test]
fn the_query_digest_ignores_pin_order_and_names_the_date() {
    let a = question(vec![pin(1, 10), pin(2, 20)], day(258));
    let b = question(vec![pin(2, 20), pin(1, 10)], day(258));
    assert_eq!(a.query_digest(), b.query_digest());
    assert_ne!(
        a.query_digest(),
        question(vec![pin(1, 10), pin(2, 20)], day(274)).query_digest()
    );
    let narrowed = ResolveQuestion {
        item_id: Some(Uuid::from_u128(1)),
        ..a.clone()
    };
    assert_ne!(a.query_digest(), narrowed.query_digest());
}

fn covered(binding: &bss_rating_core::engine::current::input::CoreBinding) -> ResolvedCell {
    let pricing = convert::to_pricing(binding).unwrap();
    ResolvedCell {
        selection: BindingSelection {
            item_id: pricing.item_id,
            dimension_value: pricing.dimension_value.clone(),
        },
        binding: Some(pricing),
    }
}

fn uncovered(item: u128) -> ResolvedCell {
    ResolvedCell {
        selection: BindingSelection {
            item_id: Uuid::from_u128(item),
            dimension_value: None,
        },
        binding: None,
    }
}

fn answer(cells: Vec<ResolvedCell>) -> ResolvedBindings {
    ResolvedBindings {
        plan_id: Uuid::from_u128(0xa),
        revision_id: Uuid::from_u128(0xb),
        cells,
    }
}

#[test]
fn a_one_cell_answer_and_a_bound_cell_share_their_digest() {
    let binding = recurring_binding(
        Uuid::from_u128(1),
        Uuid::from_u128(2),
        Decimal::new(5, 0),
        BillingTiming::Advance,
    );
    let one = cell_digest(Uuid::from_u128(0xa), Uuid::from_u128(0xb), &binding).unwrap();
    assert_eq!(
        answer_digest(&answer(vec![covered(&binding)])).unwrap(),
        one
    );
}

#[test]
fn an_answer_s_identity_covers_what_it_leaves_uncovered() {
    let binding = recurring_binding(
        Uuid::from_u128(1),
        Uuid::from_u128(2),
        Decimal::new(5, 0),
        BillingTiming::Advance,
    );
    let bound = answer_digest(&answer(vec![covered(&binding)])).unwrap();
    let with_a = answer_digest(&answer(vec![covered(&binding), uncovered(7)])).unwrap();
    let with_b = answer_digest(&answer(vec![covered(&binding), uncovered(8)])).unwrap();
    assert_ne!(with_a, bound, "an uncovered cell is part of the answer");
    assert_ne!(
        with_a, with_b,
        "the same covered cells, other uncovered cells"
    );
    let only_a = answer_digest(&answer(vec![uncovered(7)])).unwrap();
    let only_b = answer_digest(&answer(vec![uncovered(8)])).unwrap();
    assert_ne!(only_a, only_b, "two fully uncovered answers");
    let reordered =
        answer_digest(&answer(vec![uncovered(8), covered(&binding), uncovered(7)])).unwrap();
    let ordered =
        answer_digest(&answer(vec![covered(&binding), uncovered(7), uncovered(8)])).unwrap();
    assert_eq!(reordered, ordered, "cell order is not part of the identity");
}

#[test]
fn a_refusal_carries_pricing_s_code() {
    let foreign = UsageResource::invalid_argument()
        .with_field_violation("pins", "PIN_FOREIGN", "PIN_FOREIGN")
        .create();
    assert_eq!(refusal_code(&foreign), "PIN_FOREIGN");
    assert!(!is_unavailable(&foreign));
    let conflict = UsageResource::aborted("REVISION_NOT_PUBLISHED")
        .with_reason("REVISION_NOT_PUBLISHED")
        .create();
    assert_eq!(refusal_code(&conflict), "REVISION_NOT_PUBLISHED");
    let denied = UsageResource::permission_denied()
        .with_reason("PERMISSION_DENIED")
        .create();
    assert_eq!(refusal_code(&denied), "PERMISSION_DENIED");
    let incomplete = UsageResource::failed_precondition()
        .with_precondition_violation("cell", "incomplete", "INCOMPLETE_COMMERCIAL_INPUTS")
        .create();
    assert_eq!(refusal_code(&incomplete), "INCOMPLETE_COMMERCIAL_INPUTS");
}

#[test]
fn an_outage_is_retried_not_refused() {
    assert!(is_unavailable(
        &CanonicalError::service_unavailable().create()
    ));
}
