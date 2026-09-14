//! Package (block) pricing rules (`cpt-cf-bss-pricing-algo-package`).
//!
//! A `package` row prices in whole blocks: `blocks = ceil(used / package_size)`,
//! `charge = blocks * package_price_minor`. The catalog authors the three fields
//! and computes none of that — but the fields have to be structurally sound and
//! the accumulation window has to exist, because block math is **non-linear in
//! the window**.

use bss_fixtures::ModelKind;
use toolkit_macros::domain_model;

use crate::domain::price_row::{PriceRow, TierAggregationWindow};
use crate::domain::rules::{EVAL_POLICY_MISSING, PACKAGE_FIELDS_INVALID};
use crate::domain::validation::{ValidationReport, ValidationRule};

/// `inst-pk-fields` — block fields in range, and structurally exclusive with
/// tier bands.
///
/// `package_size > 0` because a block of zero units is a division by zero
/// downstream, and a row that carries both bands and block fields states two
/// incompatible formulas for one price — Tariffs would have to choose.
///
/// `package_price_minor >= 0` is guaranteed by construction: the amount type
/// refuses a negative, so there is nothing left for this rule to check. Presence
/// of the two fields on a `package` row is `inst-mk-required`'s (it owns the
/// per-kind required set); this rule judges the values and the exclusivity.
///
/// # The frozen-key arm runs before the kind is required (D-312)
///
/// This rule was the one member of that five-rule family without one, and `let
/// Some(kind) = subject.model_kind else { return };` was the first statement of
/// `evaluate`. `ModelKind::Package` is legal only on a usage row
/// (`inst-mk-chargekind`), so `package_size` / `package_price_minor` beside a
/// `recurring`, `one_time` or `one_time_setup` `chargeKind` can never be
/// legalised by a later call — `chargeKind` is frozen by the scope key and the
/// only resolution retracts the field just sent. That is `inst-mk-forbidden`'s
/// criterion exactly, and it is why the four siblings hoist theirs.
///
/// The cost of not having it was the cost D-312 exists to remove. The two fields
/// have exactly two readers in the whole rule set — `inst-mk-required` and this
/// rule — and both sat behind a kind guard, while `inst-pk-window` early-returns
/// on `!is_usage()`. So a `recurring` row carrying `package_size` was answered
/// `MODEL_KIND_MISSING` and nothing else at the write **and** nothing about the
/// package fields at publish; once the author picked any kind it surfaced at
/// publish only, under a message naming the *kind* rather than the frozen key —
/// which points them at `model_kind: package`, the one value
/// `inst-mk-chargekind` then refuses.
///
/// Nothing unpublishable ever published: the row was blocked throughout, by
/// `MODEL_KIND_MISSING` and then by this code. What the arm buys is that the
/// author learns it at the save, in terms of the operand that is frozen.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct PackageFields;

impl ValidationRule<PriceRow> for PackageFields {
    fn name(&self) -> &'static str {
        "inst-pk-fields"
    }

    fn evaluate(&self, subject: &PriceRow, report: &mut ValidationReport) {
        // The frozen-key half, ahead of the kind guard: it reads `chargeKind`
        // and the two fields, and needs no kind at all. See the doc above.
        if !subject.is_usage() {
            check_usage_only_package_fields(subject, report);
        }

        let Some(kind) = subject.model_kind else {
            return;
        };
        if !matches!(kind, ModelKind::Package) {
            // `is_usage()` guards the re-report rather than the fault: on a
            // non-usage row the arm above has already named these two fields, at
            // the write and in terms of the frozen operand, and one fault owes
            // one violation. A **usage** row of the wrong kind is the case this
            // arm is left holding, and it stays publish-stage because
            // `model_kind: package` resolves it by adding intent rather than by
            // retracting the field — `inst-mk-forbidden`'s split of one code
            // across two stages, for the same reason.
            if subject.is_usage()
                && (subject.package_size.is_some() || subject.package_price_minor.is_some())
            {
                report.violate(
                    PACKAGE_FIELDS_INVALID,
                    subject.subject(),
                    "package_size / package_price_minor are authorable only on a package row",
                );
            }
            return;
        }
        if subject.package_size == Some(0) {
            // `violate_at_write`, not `violate`. A block size of zero is read
            // from the field alone: no operand lies outside the request, and no
            // later call can give a block of no units something to price --
            // D-312's criterion, met the way the misplaced-fields arm above
            // meets it.
            //
            // The store already refuses it -- `chk_pricing_price_package_size`
            // is `package_size IS NULL OR package_size > 0` -- so deferring to
            // publish left that CHECK as the first thing to read the field and
            // the caller got a 500 rather than a violation. Third instance of
            // this shape found by the W1 e2e wave, after the zero-width band and
            // the flat row carrying bands.
            report.violate_at_write(
                PACKAGE_FIELDS_INVALID,
                subject.subject(),
                "package_size must be greater than 0: a block of no units prices nothing",
            );
        }
        if !subject.bands.is_empty() {
            report.violate(
                PACKAGE_FIELDS_INVALID,
                subject.subject(),
                format!(
                    "a package row carries {} tier band(s): block pricing and tier bands are \
                     structurally exclusive, and a row holding both states two formulas",
                    subject.bands.len()
                ),
            );
        }
    }
}

/// The block fields are usage-row only: a non-usage row has no metered quantity
/// for `used` to accumulate, so there is nothing for a block to be a block of.
///
/// `check_usage_only_policy`'s shape one file over, and deliberately its wording
/// too — an author who trips both reads one sentence twice rather than two
/// sentences about one frozen key.
fn check_usage_only_package_fields(row: &PriceRow, report: &mut ValidationReport) {
    let mut misplaced = Vec::new();
    if row.package_size.is_some() {
        misplaced.push("package_size");
    }
    if row.package_price_minor.is_some() {
        misplaced.push("package_price_minor");
    }
    if misplaced.is_empty() {
        return;
    }
    // D-312: the misplaced field and the frozen `chargeKind` are both in the
    // request, and `ModelKind::Package` — the only kind that legalises either —
    // is itself illegal on this key. The two sibling faults this rule holds,
    // `package_size = 0` and bands beside blocks, are content against content and
    // stay publish-stage.
    report.violate_at_write(
        PACKAGE_FIELDS_INVALID,
        row.subject(),
        format!(
            "{} are usage-row only; this row's chargeKind is {}",
            misplaced.join(" and "),
            row.charge_kind
        ),
    );
}

/// `inst-pk-window` — a `package` row must carry `tierAggregationWindow` (D-58).
///
/// Not tier bookkeeping borrowed for a kind that has no tiers: the window is
/// what `used` accumulates over **before** the block round-up. 150 units in a
/// month folds to `ceil(150/100) = 2` blocks under `invoice_period` and to 30
/// blocks under a daily fold — a fifteen-fold spread on the same published row.
///
/// `billingGranularity` does not supply it. It quantizes the quantity; it does
/// not bound a period — the same asymmetry the PRD already resolves for
/// `volume`, whose single rate applies to the total `Q` *within* the window.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct PackageWindow;

impl ValidationRule<PriceRow> for PackageWindow {
    fn name(&self) -> &'static str {
        "inst-pk-window"
    }

    fn evaluate(&self, subject: &PriceRow, report: &mut ValidationReport) {
        if subject.model_kind != Some(ModelKind::Package) || !subject.is_usage() {
            return;
        }
        if subject.tier_aggregation_window.is_none() {
            report.violate(
                EVAL_POLICY_MISSING,
                subject.subject(),
                format!(
                    "a package row must carry tierAggregationWindow ({}): it is the window \
                     `used` accumulates over before the block round-up, and \
                     billingGranularity does not bound a period",
                    TierAggregationWindow::offered()
                ),
            );
        }
    }
}

#[cfg(test)]
#[path = "package_tests.rs"]
mod package_tests;
