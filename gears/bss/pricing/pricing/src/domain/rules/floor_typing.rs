//! Slice 10's minimum-quantity floor typing (`inst-ft-typed`,
//! `inst-ft-fallback`, `inst-ft-both`, `inst-ft-warn`).
//!
//! Row-local, for `rules::reservation`'s reasons: each rule judges one row
//! against itself, which is D-21's test for the save-time set.
//!
//! # `FLOOR_TYPE_MISSING` has no rule here, and that is the shape rather than a
//! gap
//!
//! `inst-ft-typed` says an untyped `minQtyThreshold` fails publish. In this
//! gear's shape there is no untyped threshold to fail: `inst-ft-both` settles
//! that the two floors are *"distinct fields"* — a row may carry both — so
//! `pricing_plan_addon_rule` models them as `min_qty_purchase` and `min_qty_usage`, and
//! **the column is the type**. A quantity that did not say which floor it was
//! could not be stored.
//!
//! That is the same argument `publish::rules` already makes for the money rules:
//! a [`PriceRow`] cannot hold a violation, "so a rule here would be a rule with
//! nothing to reject — and a rule that always passes is indistinguishable from a
//! rule that holds". The absence is recorded so it reads as a decision.
//!
//! **The code is still owed a home**, because a *wire* payload can carry an
//! untyped threshold even though a row cannot: the day the authoring surface
//! accepts a `minQtyThreshold` object with a `type` member rather than two
//! fields, `FLOOR_TYPE_MISSING` is that surface's refusal. Nothing in this gear
//! offers such a payload today.

use toolkit_macros::domain_model;

use crate::domain::allowance::presented_bands;
use crate::domain::price_row::PriceRow;
use crate::domain::validation::{ValidationReport, ValidationRule};

/// A `usage` floor was authored with no fallback (`inst-ft-fallback`, S10 §5).
///
/// "The fallback is authored, not implied": a below-floor usage line must fail
/// closed into the rating exception path, and the author says so rather than
/// inheriting it. A row that declared the floor and not the behaviour beneath it
/// would freeze a quantity threshold whose consequence no document states — and
/// the two wrong readings, silent zero-rating and silent charging, are the exact
/// pair `inst-ft-typed` refuses.
///
/// The reverse pairing is deliberately **not** a violation: a fallback with no
/// floor is unrepresentable in a useful sense — it describes nothing and bills
/// nothing — and refusing it would be a rule about a field combination no author
/// can act on. It is reported as a warning instead
/// ([`FLOOR_FALLBACK_WITHOUT_FLOOR`]).
pub const FLOOR_FALLBACK_MISSING: &str = "FLOOR_FALLBACK_MISSING";

/// A fallback was authored with no `usage` floor beneath it to apply to.
///
/// A **warning**, not a violation. Nothing is mis-billed: with no floor there is
/// no below-floor case, so the field is inert. But it is almost always a
/// half-finished edit — the author set the behaviour and not the threshold — and
/// silence would freeze an inert field into an immutable version.
///
/// It is not in S10 §5's warning list (`FLOOR_INSIDE_PRICED_BAND`,
/// `GRANT_PROMO_NO_EXPIRY`); the Slice-10 handback carries it as a register item
/// rather than leaving the mint silent.
pub const FLOOR_FALLBACK_WITHOUT_FLOOR: &str = "FLOOR_FALLBACK_WITHOUT_FLOOR";

/// A floor falls inside a band that prices quantity (`inst-ft-warn`, S10 §5).
///
/// A **warning**: the plan publishes. The floor hides paid quantity — an author
/// who set a floor of 500 on a row whose first band prices `[0, 1000)` has made
/// half that band unreachable — which is legal and is almost always a mistake.
///
/// `inst-ft-warn` extends this to the `$0` allowance band, "where the floor
/// silently voids part of the granted allowance". **That half is built now**: the
/// rule reads the row's *presented* band set
/// ([`presented_bands`](crate::domain::allowance::presented_bands)), so a floor
/// inside the compiled `[0, N)` band warns exactly as one inside a hand-authored
/// `$0` first band does. Reading the authored set instead would have missed the
/// compiled band entirely on a `per_unit` row, which has no bands at all until
/// the compile runs.
pub const FLOOR_INSIDE_PRICED_BAND: &str = "FLOOR_INSIDE_PRICED_BAND";

/// A `usage` floor declares what happens beneath it, and a fallback has a floor.
#[domain_model]
#[derive(Clone, Copy, Debug)]
pub struct FloorFallbackDeclared;

impl ValidationRule<PriceRow> for FloorFallbackDeclared {
    fn name(&self) -> &'static str {
        "inst-ft-fallback"
    }

    fn evaluate(&self, subject: &PriceRow, report: &mut ValidationReport) {
        match (subject.min_qty_usage, subject.min_qty_usage_fallback) {
            (Some(floor), None) => report.violate(
                FLOOR_FALLBACK_MISSING,
                subject.subject(),
                format!(
                    "this row sets a usage floor of {floor} and declares no fallback; \
                     below-floor usage would then be either silently zero-rated or silently \
                     charged, and inst-ft-typed refuses both. Declare the fallback (launch: \
                     exception)"
                ),
            ),
            (None, Some(fallback)) => report.warn(
                FLOOR_FALLBACK_WITHOUT_FLOOR,
                subject.subject(),
                format!(
                    "this row declares the fallback {fallback} and no usage floor for it to \
                     apply beneath; nothing is mis-billed, because with no floor there is no \
                     below-floor case, but the field is inert and is usually a half-finished \
                     edit"
                ),
            ),
            _ => {}
        }
    }
}

/// A floor inside a band hides quantity the band prices (`inst-ft-warn`).
#[domain_model]
#[derive(Clone, Copy, Debug)]
pub struct FloorOutsideBands;

impl ValidationRule<PriceRow> for FloorOutsideBands {
    fn name(&self) -> &'static str {
        "inst-ft-warn"
    }

    fn evaluate(&self, subject: &PriceRow, report: &mut ValidationReport) {
        // Both floors are checked. `inst-ft-warn`'s wording names the hazard
        // generically ("the floor hides paid quantity") and the fix
        // extends it to a `$0` band; neither is specific to one floor type, and a
        // purchase floor inside a priced band hides exactly as much quantity as a
        // usage one.
        // The **presented** set (D-45): what a consumer rates from, which is the
        // compiled ladder on an allowance row and the authored one everywhere
        // else. `inst-ft-warn`'s allowance half is exactly the difference.
        let bands = presented_bands(subject);
        for (label, floor) in [
            ("purchase", subject.min_qty_purchase),
            ("usage", subject.min_qty_usage),
        ] {
            let Some(floor) = floor else { continue };
            // Strictly inside: a floor **at** a band's lower bound hides nothing,
            // which is the authoring an operator who noticed the overlap would
            // produce. Only a floor above the bound and below the top does.
            for band in &bands {
                let inside =
                    floor > band.from_qty && band.to_qty.closed_at().is_none_or(|top| floor < top);
                if inside {
                    report.warn(
                        FLOOR_INSIDE_PRICED_BAND,
                        subject.subject(),
                        format!(
                            // `nano_minor()` is a **nano**-minor count (D-311's
                            // stored scale) and this sentence called it `minor`,
                            // which read a band at $0.023 as $230,000.00. The
                            // number is right and the unit was not: `RateMinor`
                            // cannot render a decimal without the currency it
                            // deliberately does not carry, so the unit is named
                            // rather than the number converted.
                            "the {label} floor of {floor} falls inside the band starting at {} \
                             priced at {} nano-minor; quantity between the band's start and the \
                             floor is unreachable. Legal, and usually an authoring error",
                            band.from_qty,
                            band.unit_price_rate.nano_minor()
                        ),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "floor_typing_tests.rs"]
mod floor_typing_tests;
