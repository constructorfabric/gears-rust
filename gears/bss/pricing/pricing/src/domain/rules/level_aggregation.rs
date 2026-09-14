//! Level-aggregation authoring rules (D-44,
//! `cpt-cf-bss-pricing-algo-level-aggregation`).
//!
//! A usage row may declare how `Q` is *derived* rather than merely summed:
//! `peak` takes the maximum sample in each granule, `time_weighted` takes the
//! step-integral over it, and `Q` is the sum of the granule folds. That is
//! **additive**, so band math, supersession continuity and the allowance offset
//! are untouched — which is exactly why the authoring rules have to be tight:
//! the additivity only holds while every field agrees on what a granule is.
//!
//! The catalog authors the policy and **never computes a fold**; Rating owns the
//! fold (T-D-17).
//!
//! `None` means the default throughout: an unauthored `aggregationFunction` is
//! `sum` and an unauthored `aggregationGranularity` on a non-`sum` row is `hour`.
//! Every rule here reads the row through
//! [`PriceRow::effective_aggregation_function`] /
//! [`PriceRow::effective_aggregation_granularity`], because "authored nothing"
//! and "authored the default" are the same row and must not validate
//! differently.

use toolkit_macros::domain_model;

use crate::domain::price_row::PriceRow;
use crate::domain::rules::{LEVEL_FIELDS_INVALID, LEVEL_GRANULARITY_MISMATCH};
use crate::domain::validation::{ValidationReport, ValidationRule};

/// `inst-la-fields` — where the level fields may appear at all.
///
/// Level aggregation is a property of a metered stream, so neither field may
/// appear on a non-usage row. And `aggregationGranularity` is **forbidden, not
/// ignored, on a `sum` row**: there is no granule fold to parameterize, so the
/// field would be a frozen value in `pricingSnapshotRef` that changes nothing —
/// an author would reasonably read it as having taken effect.
///
/// "An unknown value" needs no check: the two fields are enums, so a value
/// outside `{sum, peak, time_weighted}` / `{hour, day}` cannot be constructed.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct LevelFields;

impl ValidationRule<PriceRow> for LevelFields {
    fn name(&self) -> &'static str {
        "inst-la-fields"
    }

    fn evaluate(&self, subject: &PriceRow, report: &mut ValidationReport) {
        if !subject.is_usage() {
            let mut misplaced = Vec::new();
            if subject.aggregation_function.is_some() {
                misplaced.push("aggregationFunction");
            }
            if subject.aggregation_granularity.is_some() {
                misplaced.push("aggregationGranularity");
            }
            if !misplaced.is_empty() {
                // D-312: placement against the frozen `chargeKind`. The
                // sibling fault in this file -- `aggregationGranularity` on a
                // usage `sum` row -- is content-against-content and stays
                // publish-stage. **`maxHold` off a level row is both**: on a
                // usage row it is content-against-content, and on a non-usage row
                // its second operand is this same frozen key, so `LevelMaxHold`
                // splits it by `is_usage()`.
                report.violate_at_write(
                    LEVEL_FIELDS_INVALID,
                    subject.subject(),
                    format!(
                        "{} are usage-row only; this row's chargeKind is {}",
                        misplaced.join(" and "),
                        subject.charge_kind
                    ),
                );
            }
            return;
        }
        if !subject.is_level() && subject.aggregation_granularity.is_some() {
            report.violate(
                LEVEL_FIELDS_INVALID,
                subject.subject(),
                "aggregationGranularity is forbidden on a sum row: there is no granule fold \
                 for it to parameterize, so freezing it would state a policy nothing applies",
            );
        }
    }
}

/// `inst-la-granularity` — the D-77 pairing: `hour => per_hour`,
/// `day => per_day`.
///
/// Without it two rules of this slice name **different** units for the same
/// band. `inst-tb-units` derives the band unit from `billingGranularity`;
/// `inst-la-units` derives it from `level unit x aggregationGranularity`. A
/// `time_weighted` / `hour` row with `billingGranularity = per_day` therefore
/// has bands in GB-hours under one reading and GB-days under the other: a
/// factor-of-24 error at the band edge that every other stated check passes.
/// With the pairing pinned the two rules name one unit by construction.
///
/// A row with **no** `billingGranularity` is not judged here — that is a missing
/// field, reported once by `inst-tb-window` under `EVAL_POLICY_MISSING`. This
/// rule judges pairings, and there is no pairing to judge.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct LevelGranularityPairing;

impl ValidationRule<PriceRow> for LevelGranularityPairing {
    fn name(&self) -> &'static str {
        "inst-la-granularity"
    }

    fn evaluate(&self, subject: &PriceRow, report: &mut ValidationReport) {
        if !subject.is_usage() || !subject.is_level() {
            return;
        }
        let granularity = subject.effective_aggregation_granularity();
        let expected = granularity.billing_counterpart();
        let Some(authored) = subject.billing_granularity else {
            return;
        };
        if authored != expected {
            report.violate(
                LEVEL_GRANULARITY_MISMATCH,
                subject.subject(),
                format!(
                    "a {} row folding into {granularity} granules must set billingGranularity \
                     to {expected}, not {authored}: otherwise the band bounds and the fold \
                     output are denominated in different units",
                    subject.effective_aggregation_function()
                ),
            );
        }
    }
}

/// `inst-la-maxhold` — the `hold_last` bound, required on non-`sum` **usage**
/// rows and forbidden everywhere else: on a `sum` usage row, which has no gap
/// fold to bound, and on any non-usage row, whose key cannot carry the fold at
/// all. The two refusals differ in stage and in sentence; see `evaluate`.
///
/// A gauge stream has gaps, and `hold_last` carries the last observed level
/// across one for at most `max_hold_granules` granules; beyond that the level
/// reads **0** and an operator signal raises. How long a stale level may stand
/// in for a live one is a commercial statement, so there is **no default**: it
/// is authored explicitly or the row does not publish.
///
/// Its presence on a `sum` row fails for the same reason
/// `aggregationGranularity` does — a `sum` row has no gap-fold to bound.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct LevelMaxHold;

impl ValidationRule<PriceRow> for LevelMaxHold {
    fn name(&self) -> &'static str {
        "inst-la-maxhold"
    }

    fn evaluate(&self, subject: &PriceRow, report: &mut ValidationReport) {
        // `is_level()` reads the aggregation function and no charge kind, so the
        // charge kind is asked for here: the requirement arm below is owed by a
        // usage row only. A non-usage row authoring `aggregationFunction: peak`
        // has a fault `LevelFields` refuses at the write on that key, and
        // demanding `maxHoldGranules` beside it reports a consequence of the one
        // thing the author has to fix -- on a row with no gap fold to bound.
        if !subject.is_usage() || !subject.is_level() {
            if subject.max_hold_granules.is_some() {
                // **Two stages, one code, and the key decides which.** On a
                // non-usage row the field can never become legal: `LevelFields`
                // above refuses `aggregationFunction` at the write on such a key,
                // so nothing a later call adds can carry the fold this bound is
                // about, and the fault's second operand is the frozen
                // `chargeKind`. That is D-312's criterion. Judged at publish
                // only, such a row is answered 201 and reads back as authorable,
                // dying later -- no DB constraint ties the column to the charge
                // kind (`chk_pricing_price_max_hold_granules` only enforces
                // `>= 1`).
                //
                // On a *usage* `sum` row both operands are content and the
                // publish stage stays right: a later `aggregationFunction: peak`
                // resolves it by adding intent.
                let detail = "max_hold_granules is forbidden on a sum row: it bounds a gap \
                              fold, and a sum row has none";
                if subject.is_usage() {
                    report.violate(LEVEL_FIELDS_INVALID, subject.subject(), detail);
                } else {
                    report.violate_at_write(
                        LEVEL_FIELDS_INVALID,
                        subject.subject(),
                        format!(
                            "max_hold_granules is usage-row only; this row's chargeKind is {}",
                            subject.charge_kind
                        ),
                    );
                }
            }
            return;
        }
        match subject.max_hold_granules {
            None => report.violate(
                LEVEL_FIELDS_INVALID,
                subject.subject(),
                format!(
                    "a {} row must declare max_hold_granules (>= 1): how long a stale level \
                     may stand in across a sampling gap is authored, never assumed",
                    subject.effective_aggregation_function()
                ),
            ),
            Some(0) => report.violate(
                LEVEL_FIELDS_INVALID,
                subject.subject(),
                "max_hold_granules must be at least 1: a bound of zero granules holds nothing \
                 and is not the way to say `never hold`",
            ),
            Some(_) => {}
        }
    }
}

#[cfg(test)]
#[path = "level_aggregation_tests.rs"]
mod level_aggregation_tests;
