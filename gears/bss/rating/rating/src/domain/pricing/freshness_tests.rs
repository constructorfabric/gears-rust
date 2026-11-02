#![allow(clippy::unwrap_used, clippy::expect_used)]

use bss_rating_core::engine::current::input::{CellSelection, CoreCell, RatingWindowKind};
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use super::{Freshness, Stale, prove};
use crate::domain::digest::Sha256Hex;
use crate::domain::fact::model::AcceptedTerm;
use crate::domain::pricing::{StoredAnswer, cell_digest};
use crate::test_providers::subscriptions::{seal, usage_binding};

const ITEM: u128 = 0x18;
const PRICE: u128 = 0xa;

fn at(text: &str) -> OffsetDateTime {
    OffsetDateTime::parse(text, &Rfc3339).unwrap()
}

fn term() -> AcceptedTerm {
    let binding = usage_binding(
        Uuid::from_u128(ITEM),
        Uuid::from_u128(PRICE),
        RatingWindowKind::CalendarHourUtc,
        "products.derived/cloudlet@1",
        None,
    );
    let (plan_id, revision_id) = (Uuid::from_u128(1), Uuid::from_u128(2));
    let binding_digest = cell_digest(plan_id, revision_id, &binding).unwrap();
    AcceptedTerm {
        terms_ref: "u1".to_owned(),
        catalog_tenant_id: Uuid::from_u128(3),
        plan_id,
        revision_id,
        binding,
        binding_digest,
    }
}

fn answer(term: &AcceptedTerm, ends_on: Option<Date>, price: u128) -> StoredAnswer {
    let mut binding = term.binding.clone();
    binding.price.ends_on = ends_on;
    binding.price.price_id = Uuid::from_u128(price);
    let binding = seal(binding).unwrap();
    StoredAnswer {
        binding_digest: Sha256Hex::of_bytes(b"answer"),
        plan_id: term.plan_id,
        revision_id: term.revision_id,
        cells: vec![CoreCell {
            selection: CellSelection {
                item_id: binding.item_id,
                dimension_value: None,
            },
            binding: Some(binding),
        }],
        first_fetched_at: at("2026-10-10T00:00:00Z"),
    }
}

fn oct(day: u8) -> Date {
    Date::from_calendar_date(2026, time::Month::October, day).unwrap()
}

#[test]
fn the_same_price_without_an_end_is_fresh() {
    let term = term();
    let fresh = prove(
        &term,
        &answer(&term, None, PRICE),
        at("2026-10-05T01:00:00Z"),
        &[],
    );
    assert_eq!(fresh, Freshness::Fresh);
}

#[test]
fn v78_an_end_after_the_window_keeps_it_fresh() {
    let term = term();
    let answer = answer(&term, Some(oct(20)), PRICE);
    assert_eq!(
        prove(&term, &answer, at("2026-10-12T01:00:00Z"), &[]),
        Freshness::Fresh
    );
}

#[test]
fn v78_an_end_inside_the_window_the_fact_did_not_cut_at_is_stale() {
    let term = term();
    let answer = answer(&term, Some(oct(20)), PRICE);
    // The billing-period child of the line reaches past 20 October.
    assert_eq!(
        prove(&term, &answer, at("2026-11-01T00:00:00Z"), &[]),
        Freshness::Stale(Stale::EndsInsideWindow { ends_on: oct(20) })
    );
    // Once the fact cuts there, the same answer is fresh for the first slice.
    assert_eq!(
        prove(
            &term,
            &answer,
            at("2026-11-01T00:00:00Z"),
            &[at("2026-10-20T00:00:00Z")]
        ),
        Freshness::Fresh
    );
}

#[test]
fn another_price_is_stale_never_priced() {
    let term = term();
    assert_eq!(
        prove(
            &term,
            &answer(&term, None, 0xbb),
            at("2026-10-05T01:00:00Z"),
            &[]
        ),
        Freshness::Stale(Stale::BindingChanged)
    );
}

#[test]
fn an_uncovered_cell_is_stale() {
    let term = term();
    let mut answer = answer(&term, None, PRICE);
    answer.cells[0].binding = None;
    assert_eq!(
        prove(&term, &answer, at("2026-10-05T01:00:00Z"), &[]),
        Freshness::Stale(Stale::Uncovered)
    );
}
