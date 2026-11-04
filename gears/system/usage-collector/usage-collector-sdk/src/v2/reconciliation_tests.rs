//! Unit tests for [`super::ReconciliationMetadata`] and
//! [`super::QuantitySummary`].

use core::num::NonZeroU64;

use bigdecimal::BigDecimal;
use time::OffsetDateTime;

use super::{ObservedQuantity, QuantitySummary, ReconciliationMetadata};
use crate::v2::aggregation::AggregationFold;
use crate::v2::quantity::UsageQuantity;

/// Every fold, listed once so each test below loops over the same five
/// rather than re-enumerating them.
///
/// This array is not itself what forces a fold admitted later to be
/// accounted for: a fixed-size array literal does not have to enumerate
/// every variant of anything, so a sixth `AggregationFold` compiles here
/// unchanged and would simply go untested by this suite. The actual
/// compile-time guarantee — a fold admitted later is a compile error rather
/// than a silently-uncovered branch — lives in the exhaustive `match` inside
/// [`QuantitySummary::accrues`]. Ordered as `AggregationFold` declares them.
const EVERY_FOLD: [AggregationFold; 5] = [
    AggregationFold::Sum,
    AggregationFold::Count,
    AggregationFold::Max,
    AggregationFold::Min,
    AggregationFold::Latest,
];

#[test]
fn sum_is_the_only_fold_that_accrues() {
    for fold in EVERY_FOLD {
        let accrues = QuantitySummary::accrues(fold);
        assert_eq!(
            accrues,
            matches!(fold, AggregationFold::Sum),
            "`usage-collector-v2.yaml`'s `quantity_summary` selects the `accrued_sum` branch \
             for a SUM type and the observation branch for every other fold; {fold:?} took \
             the wrong one"
        );
    }
}

#[test]
fn an_empty_selection_accrues_a_defined_zero_under_sum_and_observes_nothing_otherwise() {
    // The plugin's DESIGN.md:683 in one assertion: "an empty-selection summary
    // (`accrued_sum` = 0, or `observation_count` = 0 with `latest_observation`
    // absent)". `accrued_sum` stays 0 rather than null, because an accrual over
    // an empty set is defined while an observation over one is not.
    assert_eq!(
        QuantitySummary::empty_for(AggregationFold::Sum),
        QuantitySummary::Accrued(BigDecimal::from(0)),
    );
    for fold in EVERY_FOLD {
        if matches!(fold, AggregationFold::Sum) {
            continue;
        }
        assert_eq!(
            QuantitySummary::empty_for(fold),
            QuantitySummary::Observations(None),
            "{fold:?} must report an observation count of zero with the observation absent"
        );
    }
}

#[test]
fn an_accrued_zero_is_not_an_absent_observation() {
    // The distinction the whole branch exists for: a consumer reading a
    // reconciliation figure must be able to tell a meter that summed to zero
    // from one whose fold could not be taken.
    assert_ne!(
        QuantitySummary::empty_for(AggregationFold::Sum),
        QuantitySummary::empty_for(AggregationFold::Max),
    );
}

#[test]
fn the_metadata_empty_for_a_fold_carries_that_folds_empty_summary_and_nothing_else() {
    for fold in EVERY_FOLD {
        let empty = ReconciliationMetadata::empty_for(fold);
        assert_eq!(empty.accepted_count, 0);
        assert_eq!(empty.quantity_summary, QuantitySummary::empty_for(fold));
        assert_eq!(
            empty.max_accepted_at, None,
            "a scope holding no entries reports both watermarks absent"
        );
        assert_eq!(empty.max_window_end, None);
    }
}

#[test]
fn the_observation_branch_keeps_the_scale_the_caller_sent() {
    // `UsageQuantity` equality is textual, and
    // `latest_observation` is one persisted observation rather than a fold
    // result, so it carries `UsageQuantity` and not a bare `Decimal`.
    let trailing = UsageQuantity::parse("42.500").expect("literal is in range");
    let plain = UsageQuantity::parse("42.5").expect("literal is in range");
    let count = NonZeroU64::new(1).unwrap();
    assert_ne!(
        QuantitySummary::Observations(Some(ObservedQuantity {
            count,
            latest: trailing
        })),
        QuantitySummary::Observations(Some(ObservedQuantity {
            count,
            latest: plain
        })),
    );
}

#[test]
fn two_metadata_values_differing_in_any_single_field_are_unequal() {
    // No `..` in the base literal, so a fifth field on `ReconciliationMetadata`
    // is a compile error here rather than a field this test silently stops
    // covering.
    let at = OffsetDateTime::UNIX_EPOCH;
    let base = ReconciliationMetadata {
        accepted_count: 3,
        quantity_summary: QuantitySummary::Accrued(BigDecimal::from(7)),
        max_accepted_at: Some(at),
        max_window_end: Some(at),
    };

    let mut count = base.clone();
    count.accepted_count = 4;
    assert_ne!(base, count);

    let mut summary = base.clone();
    summary.quantity_summary = QuantitySummary::Accrued(BigDecimal::from(8));
    assert_ne!(base, summary);

    let mut accepted = base.clone();
    accepted.max_accepted_at = None;
    assert_ne!(base, accepted);

    let mut window = base.clone();
    window.max_window_end = None;
    assert_ne!(base, window);
}
