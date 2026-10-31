#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not `expect`, because not every item unwraps or expects"
)]
use bss_orders::domain::gate::compose::{ComposeError, compose, local};
use bss_orders::domain::gate::predicates::PredicateId;
use bss_orders::domain::gate::vector::*;
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::gate_report::CatalogSelection;
use uuid::Uuid;

fn key() -> OutcomeKey {
    OutcomeKey {
        predicate: PredicateId::CatalogSlotCoverage,
        line_id: Some(Uuid::nil()),
        item_id: Some(Uuid::nil()),
        selection: CatalogSelection::SelectedNull,
    }
}
fn sub(pred: &str, v: Verdict, reason: Option<&str>) -> ProducerResult {
    ProducerResult {
        producer: "pricing".into(),
        profile: "pricing-read/1".into(),
        predicate: pred.into(),
        verdict: v,
        reason: reason.map(str::to_owned),
        detail: None,
    }
}
fn map(r: &ProducerResult) -> Option<Reason> {
    match r.reason.as_deref() {
        Some("REVISION_NOT_PUBLISHED") => Some(Reason::CatalogPredicateFailed),
        Some("BELOW_FLOOR") => Some(Reason::QuantityBelowFloor),
        Some("TIMEOUT") => Some(Reason::CatalogPredicatesUnavailable),
        Some("NOT_EVALUABLE") => Some(Reason::CatalogPredicateUnevaluable),
        _ => None,
    }
}

#[test]
fn any_unevaluable_wins_then_any_failed_then_passed_with_first_winning_reason() {
    let o = compose(
        key(),
        Applicability::Required,
        map,
        vec![
            sub("a", Verdict::Passed, None),
            sub("b", Verdict::Failed, Some("REVISION_NOT_PUBLISHED")),
            sub("c", Verdict::Unevaluable, Some("TIMEOUT")),
        ],
    )
    .unwrap();
    assert_eq!(o.verdict, Verdict::Unevaluable);
    assert_eq!(o.reason, Some(Reason::CatalogPredicatesUnavailable));
    assert_eq!(
        o.producer_results.len(),
        3,
        "every subresult is kept in order"
    );
    let o = compose(
        key(),
        Applicability::Required,
        map,
        vec![
            sub("a", Verdict::Passed, None),
            sub("b", Verdict::Failed, Some("REVISION_NOT_PUBLISHED")),
        ],
    )
    .unwrap();
    assert_eq!(
        (o.verdict, o.reason),
        (Verdict::Failed, Some(Reason::CatalogPredicateFailed))
    );
    let o = compose(
        key(),
        Applicability::Required,
        map,
        vec![sub("a", Verdict::Passed, None)],
    )
    .unwrap();
    assert_eq!((o.verdict, o.reason), (Verdict::Passed, None));
}

#[test]
fn duplicate_subresult_identity_and_unmapped_reasons_are_invalid() {
    let dup = compose(
        key(),
        Applicability::Required,
        map,
        vec![
            sub("a", Verdict::Passed, None),
            sub("a", Verdict::Passed, None),
        ],
    );
    assert!(matches!(dup, Err(ComposeError::DuplicateSubresult(_))));
    let unmapped = compose(
        key(),
        Applicability::Required,
        map,
        vec![sub("a", Verdict::Failed, Some("NEW_CODE"))],
    );
    assert!(matches!(unmapped, Err(ComposeError::ReasonUnmapped(_))));
}

fn composed(subs: Vec<ProducerResult>) -> (Verdict, Option<Reason>) {
    let o = compose(key(), Applicability::Required, map, subs).unwrap();
    (o.verdict, o.reason)
}

#[test]
fn first_failed_and_first_unevaluable_reasons_win() {
    let failed = |c| sub("a", Verdict::Failed, Some(c));
    let failed_b = |c| sub("b", Verdict::Failed, Some(c));
    assert_eq!(
        composed(vec![
            failed("REVISION_NOT_PUBLISHED"),
            failed_b("BELOW_FLOOR")
        ]),
        (Verdict::Failed, Some(Reason::CatalogPredicateFailed))
    );
    assert_eq!(
        composed(vec![
            failed("BELOW_FLOOR"),
            failed_b("REVISION_NOT_PUBLISHED")
        ]),
        (Verdict::Failed, Some(Reason::QuantityBelowFloor)),
        "reversed order: order, not value, decides"
    );
    let unev = |p, c| sub(p, Verdict::Unevaluable, Some(c));
    assert_eq!(
        composed(vec![unev("a", "TIMEOUT"), unev("b", "NOT_EVALUABLE")]),
        (
            Verdict::Unevaluable,
            Some(Reason::CatalogPredicatesUnavailable)
        )
    );
    assert_eq!(
        composed(vec![unev("a", "NOT_EVALUABLE"), unev("b", "TIMEOUT")]),
        (
            Verdict::Unevaluable,
            Some(Reason::CatalogPredicateUnevaluable)
        ),
        "reversed order: order, not value, decides"
    );
}

#[test]
fn same_predicate_from_different_producers_or_profiles_is_not_a_duplicate() {
    let mut other_producer = sub("a", Verdict::Passed, None);
    other_producer.producer = "rating".into();
    let mut other_profile = sub("a", Verdict::Passed, None);
    other_profile.profile = "pricing-read/2".into();
    let o = compose(
        key(),
        Applicability::Required,
        map,
        vec![
            sub("a", Verdict::Passed, None),
            other_producer,
            other_profile,
        ],
    )
    .unwrap();
    assert_eq!(o.producer_results.len(), 3);
    assert_eq!(o.verdict, Verdict::Passed);
}

#[test]
fn empty_subresults() {
    let r = compose(key(), Applicability::Required, map, vec![]);
    assert!(matches!(r, Err(ComposeError::NoSubresults)), "{r:?}");
}

#[test]
fn a_local_result_has_no_producer_results() {
    let o = local(
        key(),
        Applicability::Required,
        Verdict::Failed,
        Some(Reason::QuantityBelowFloor),
        None,
    );
    assert!(o.producer_results.is_empty());
}
