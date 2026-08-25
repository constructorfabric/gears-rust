//! Slice 10's reserved-capacity attributes, as row-local rules
//! (`inst-rv-attrs`, `inst-rv-usage`, `inst-rv-level`; A1/A2, D-53, D-139).
//!
//! # Why these are row rules and not plan rules
//!
//! Every one of the three judges **one row against itself**: whether a
//! reservation may sit on this `chargeKind` at all, whether both halves of the
//! pair are present, and whether this row's `aggregationFunction` admits the
//! flavor. None reads another row, the plan, or any tenant state — which is D-21's
//! test for the save-time set, and it earns the author the refusal at authoring
//! rather than at publish.
//!
//! It also puts them where the **joint corpus** can reach them.
//! `examples/regen_registry/validator.rs` answers the corpus's publish cases by
//! running [`price_row_rules`](super::price_row_rules) and then the supersession
//! guard; it never assembles a `PlanShape`. A reservation rule registered into
//! the plan set would therefore have been unreachable from
//! `corpus/reserved/consumption-on-level-rejected.toml` — the case whose whole
//! purpose is to pin `LEVEL_RESERVATION_CONSUMPTION_FORBIDDEN`, and which had
//! carried `declined_until = "slice-10-advanced-primitives"` precisely because
//! no subject could reach the rule.

use toolkit_macros::domain_model;

use crate::domain::price_row::{PriceRow, ReservationFlavor};
use crate::domain::validation::{ValidationReport, ValidationRule};

/// A reservation was authored on a row that is not a **usage** row
/// (`inst-rv-attrs` / `inst-rv-usage`, S10 §5).
///
/// A1 makes a reservation an attribute *of the metered line it reserves*, so a
/// reservation on a recurring or one-off row names no meter, no quantity and no
/// counter — there is nothing for `reservedRate` to be a rate **of**. Rating
/// sources the reserved rate at step 6 against a usage line; a non-usage row
/// carrying one would freeze a rate no evaluation path can reach.
pub const RESERVATION_ON_NON_USAGE: &str = "RESERVATION_ON_NON_USAGE";

/// Exactly one half of the reservation pair was authored (`inst-rv-attrs`, A1).
///
/// §6 states this as a schema constraint —
/// `CHECK (reservation_flavor IS NULL) = (reserved_rate_minor IS NULL)` — and it
/// is a rule here rather than a column `CHECK` because `SQLite` has no
/// incremental form for a table-level constraint and a Postgres-only one would
/// leave the two engines' `EXPECTED_CHECKS` censuses stating different schemas
/// (``pricing_price``'s note).
///
/// A rate with no flavor cannot be evaluated at all — `inst-rv-tier-q` and
/// `inst-rv-level` bill the same rate by two different rules — and a flavor with
/// no rate reserves at an unstated price. Neither is a value a snapshot may
/// freeze, so publish refuses rather than defaulting either half.
///
/// **This code is not in S10 §5's list.** The AC requires the rule ("flavor
/// without rate rejected") and the design set never minted a code for it; the
/// Slice-10 handback carries it as a documentation register item rather than
/// leaving the mint silent.
pub const RESERVATION_PAIR_INCOMPLETE: &str = "RESERVATION_PAIR_INCOMPLETE";

/// `reservationFlavor = consumption` was authored on a non-`sum` row (D-53,
/// `inst-rv-level`, S10 §5).
///
/// On a level row `Q` is a sum of per-granule folds, so "subtract the reserved
/// quantity" has no single meaning: it could net per granule or per window, and
/// the two bill differently. D-53 therefore makes `capacity` the **only** flavor
/// authorable on a non-`sum` row at launch — its charge never touches `Q`, which
/// is exactly the reserved-cloudlets-with-peak-metering launch product — and
/// defers per-granule netting to a named Future gate.
///
/// Fail-closed rather than defaulted to `capacity`: the two flavors are
/// different commercial offers, and silently converting one into the other would
/// sell something nobody authored.
pub const LEVEL_RESERVATION_CONSUMPTION_FORBIDDEN: &str = "LEVEL_RESERVATION_CONSUMPTION_FORBIDDEN";

/// A reserved row is a **usage** row, carries both halves of the pair, and does
/// not reserve consumption on a level meter.
///
/// Three refusals in one rule because they are three readings of one authored
/// fact — the reservation — and an author who set it wrongly should see every
/// way it is wrong in one report rather than one per re-publish.
///
/// The order matters and is the order an author fixes them in: whether the row
/// may reserve at all (`inst-rv-usage`), whether the reservation is complete
/// (`inst-rv-attrs`), and only then whether the completed flavor is legal on
/// this meter's shape (`inst-rv-level`, D-53).
///
/// # The corpus reads the **first** violation, so the order is load-bearing
///
/// `CatalogPublishValidator` reports `shape.violations.first()`, so a case
/// pinning one of these three codes is pinning whichever of them this rule
/// reports first. `consumption-on-level-rejected` authors both rows complete and
/// on `usage` for exactly that reason — its own comment says the case would
/// otherwise have gone red on `TIER_BANDS_GAP`, "a verdict about a malformed row,
/// and about nothing D-53 says".
#[domain_model]
#[derive(Clone, Copy, Debug)]
pub struct ReservationWellFormed;

impl ValidationRule<PriceRow> for ReservationWellFormed {
    fn name(&self) -> &'static str {
        "inst-rv-attrs"
    }

    fn evaluate(&self, subject: &PriceRow, report: &mut ValidationReport) {
        if !subject.is_reserved() {
            return;
        }

        if !subject.is_usage() {
            // D-312: a reservation and a frozen non-usage `chargeKind`, both in
            // the request. The both-or-neither pairing stays publish-stage.
            report.violate_at_write(
                RESERVATION_ON_NON_USAGE,
                subject.subject(),
                format!(
                    "this row's chargeKind is {} and it carries a reservation; a reservation is \
                     an attribute of the metered line it reserves (A1), so there is no meter, no \
                     quantity and no counter for the reserved rate to apply to",
                    subject.charge_kind
                ),
            );
        }

        // The pairing, from either side. Reported with the half that is present,
        // because that is what the author has to reconcile.
        match (subject.reserved_rate, subject.reservation_flavor) {
            (Some(_), None) => report.violate(
                RESERVATION_PAIR_INCOMPLETE,
                subject.subject(),
                "this row authors reservedRate with no reservationFlavor; the same rate bills by \
                 two different rules (inst-rv-tier-q excludes the reserved quantity from Q, \
                 inst-rv-level never enters it), so the rate alone does not determine a charge"
                    .to_owned(),
            ),
            (None, Some(flavor)) => report.violate(
                RESERVATION_PAIR_INCOMPLETE,
                subject.subject(),
                format!(
                    "this row authors reservationFlavor = {flavor} with no reservedRate; it \
                     reserves at an unstated price, which is not a value the snapshot may freeze"
                ),
            ),
            _ => {}
        }

        // D-53. Read through `effective_aggregation_function` -- via `is_level`
        // -- so an unauthored `aggregationFunction` and an authored `sum` are one
        // row here, as they are everywhere else.
        if subject.is_level() && subject.reservation_flavor == Some(ReservationFlavor::Consumption)
        {
            report.violate(
                LEVEL_RESERVATION_CONSUMPTION_FORBIDDEN,
                subject.subject(),
                format!(
                    "this row derives Q with {} and reserves consumption; on a level meter Q is a \
                     sum of per-granule folds, so subtracting a reserved quantity could net per \
                     granule or per window and the two bill differently (D-53). Only \
                     reservationFlavor = capacity is authorable here at launch",
                    subject.effective_aggregation_function().as_str()
                ),
            );
        }
    }
}

#[cfg(test)]
#[path = "reservation_tests.rs"]
mod reservation_tests;
