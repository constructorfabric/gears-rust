//! The **supersession unit guard** (`inst-tb-supersession-units`; D-82, D-98,
//! D-122, D-127, D-129).
//!
//! The tier counter `Q` is derived per `(subscription, meter, dimensionKey,
//! window)` and belongs to the subscription's usage history, not to a price-row
//! version. Superseding a row therefore does **not** reset an in-window counter:
//! the successor's bands are simply applied to the continued `Q`.
//!
//! That continuity is what this guard protects. A successor landing on an
//! occupied published canonical scope key MUST NOT change the fields the
//! continued `Q` is **denominated in**, **derived from**, or **priced by** —
//! otherwise the same accumulated number is reinterpreted under a different
//! unit, read off a different stream, or re-priced under a different formula
//! mid-window:
//!
//! - `per_hour -> per_day` applies an hours-denominated `Q` to day-denominated
//!   bands: the D-77 factor-of-24 band-edge class, reintroduced through
//!   supersession.
//! - a changed `meter` or `dimensionKey` silently reads a different counter.
//! - a `graduated -> volume` / `package` flip re-prices the **already
//!   accumulated** window total under new math — `volume` applies the selected
//!   band's single rate to the whole window `Q`, including units already rated
//!   marginally under the predecessor (D-98).
//! - a changed `package_size` re-buckets an already-accumulated `used`, because
//!   rating counts blocks by a cumulative ceil-diff that presupposes one block
//!   size per window (D-122).
//! - a changed `carry` allowance would rewrite a **plan-scoped**,
//!   revision-immutable grant row that a supersession cannot open a revision to
//!   touch (D-129).
//!
//! Supersession is a **price** change on one key: new amounts, new bands. What
//! or how the key meters, and which formula prices it, is **structural** and
//! routes through plan revisioning and migration.
//!
//! ## The guard binds the key, not the mechanism (D-127)
//!
//! [`SupersessionUnitGuard`] takes a predecessor/successor **pair** and has no
//! notion of which mechanism produced it. That is the point. Both sanctioned
//! producers of `published -> superseded` are bound: the interactive
//! supersession unit, and the grandfathering cutover's `all_subscriptions`
//! successor, which lands on the predecessor's own scope key and inherits the
//! identical continued counter. Both set `supersedes_price_id` on the successor,
//! which is what pairs the two rows here.
//!
//! Before D-127 the rule was invoked from the supersession unit alone, so a
//! cutover successor could flip `per_hour -> per_day` — the same band-edge class
//! through its fifth door, on the one path that is always material and therefore
//! felt safe. A signature that asked "which mechanism?" would reopen exactly
//! that hole, so this one cannot ask.
//!
//! ## The seven unit axes are shared, not restated
//!
//! **Seven axes, eight labels** (D-317, review Z23-1). The first axis is reported
//! under one of two names — `model_kind` when the authored kind moved, and
//! `includedAllowance (presented modelKind)` when it did not and the *compiled* kind
//! did (D-130's two-band ladder). They are an `if`/`else if` on one axis, so a
//! single answer carries at most seven labels, which is why the arithmetic below
//! still reads seven.
//!
//! Supersession is not the only handover that leaves `Q` running. A **phase
//! conversion** does too — the counter is phase-blind, so the row serving the
//! meter after conversion inherits the continued counter — and
//! `inst-ph-override-units` (D-89, extended by D-122) states the same
//! requirement over the same seven axes for a phase-scoped usage override.
//!
//! So the seven live in one place,
//! [`unit_determining_mismatch`](crate::domain::price_row::unit_determining_mismatch),
//! and both guards call it:
//! [`SupersessionPair::mismatched_unit_fields`] appends the three this guard
//! adds, and
//! [`PhaseOverrideUnits`](crate::domain::plan_rules::phase_graph::PhaseOverrideUnits)
//! uses the seven axes as they stand. A second hand-maintained copy of that list is
//! how the factor-of-24 class re-enters through its next door — the D-127 point
//! restated at field granularity: a guard must not be able to differ by which
//! mechanism asked, and one that can drift already differs.

use toolkit_macros::domain_model;

use crate::domain::price_row::{PriceRow, unit_determining_mismatch};
use crate::domain::rules::SUPERSESSION_UNIT_MISMATCH;
use crate::domain::validation::{ValidationReport, ValidationRule};

/// A predecessor and the successor landing on its canonical scope key.
///
/// The pair is the unit of judgement because the guard is a **comparison**:
/// there is no property of the successor alone that says whether it changed a
/// unit field.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SupersessionPair {
    /// The published row being closed.
    pub predecessor: PriceRow,
    /// The row landing on its key.
    pub successor: PriceRow,
}

impl SupersessionPair {
    /// Pair a predecessor with its successor.
    #[must_use]
    pub const fn new(predecessor: PriceRow, successor: PriceRow) -> Self {
        Self {
            predecessor,
            successor,
        }
    }

    /// The unit/counter-determining fields the successor changed, in the
    /// spelling the design set names them.
    ///
    /// Empty means the successor is a pure **price** change and may publish. The
    /// list is what the violation names: "publish failed" without saying which
    /// field moved is not remediable, and the whole point of the rule is that
    /// the offending field is structural.
    ///
    /// **Twelve fields, seven axes of them shared.** The middle seven are
    /// [`unit_determining_mismatch`] — the same list `inst-ph-override-units`
    /// binds a phase-scoped override to, written once for the reason the module
    /// doc gives. The **five** this guard adds are its own because they are
    /// supersession-specific: `meter` and `dimensionKey` are two of the four
    /// components of the counter's own key, so a successor that moved either
    /// would not inherit the counter at all but silently read a different one;
    /// `reservationFlavor` decides whether the reserved quantity leaves the
    /// on-demand counter (see the comment at its comparison, and D-254);
    /// `maxHoldGranules` decides what a gap in the hold is worth;
    /// and `included_allowance` binds only under the D-129 `carry` condition.
    ///
    /// **Do not re-derive this number by counting the prose.** A census reading "ten
    /// fields … the three this guard adds" is left behind by the next field D-254
    /// adds — and so is the test that spells the count as a literal, which cannot
    /// catch a field's removal either. `mismatched_unit_fields` has five
    /// `changed.push` calls of its own and splices
    /// [`unit_determining_mismatch`]'s seven axes, and that test asserts the total
    /// against those two sources rather than against a literal.
    ///
    /// It has happened a third time: `max_hold_granules` makes it five
    /// and left this sentence saying four. The census now sets every field the
    /// function names, which is the only reader of this count that cannot go
    /// stale silently.
    ///
    /// Deliberately **not** listed, because they are the legitimate price
    /// levers: the band amounts, the band set itself, `amount_minor`,
    /// `package_price_minor`, and the **quantity** of a `none`-policy allowance.
    ///
    /// That last clause was written without a qualifier and it was wrong on one
    /// shape. A `none` allowance compiles to nothing plan-scoped, which is the
    /// argument this method's `carry` condition is about, but on an untiered
    /// `per_unit` row it compiles the row into a band ladder — and *that* moves
    /// what prices the continued counter. So introducing or dropping one on such
    /// a row is caught by [`unit_determining_mismatch`]'s presented-kind clause,
    /// under its own name, while a quantity change on a row that presents a
    /// ladder on both sides stays exactly the free lever the clause claims.
    #[must_use]
    pub fn mismatched_unit_fields(&self) -> Vec<&'static str> {
        let (before, after) = (&self.predecessor, &self.successor);
        let mut changed = Vec::new();
        if before.meter != after.meter {
            changed.push("meter");
        }
        if before.dimension_key != after.dimension_key {
            changed.push("dimensionKey");
        }
        changed.extend(unit_determining_mismatch(before, after));
        // `reservation_flavor` is this guard's own rather than one of the shared
        // seven, and the split is deliberate (D-254): the shared list is
        // `inst-ph-override-units`' too, and widening it would rebind a
        // phase-scoped override on an argument the design set has not made.
        // Here the argument is direct -- the flavor decides whether the reserved
        // quantity leaves the on-demand counter `Q` (`inst-rv-tier-q`) or never
        // enters it (`inst-rv-level`, D-139) -- so a successor that flips it
        // inherits a continued counter under different semantics, which is
        // exactly what this rule refuses for the other unit-determining fields.
        if before.reservation_flavor != after.reservation_flavor {
            changed.push("reservationFlavor");
        }
        // `max_hold_granules` is this guard's own for the same reason and by the
        // same argument. The shared seven are `inst-ph-override-units`' list too,
        // and widening it would rebind a phase-scoped override on an argument the
        // design set has not made. Here the argument is direct: past the bound the
        // level reads 0, so a successor that moves it inherits a **continued**
        // counter under a different rule for what a gap is worth -- the very thing
        // this guard refuses for the other unit-determining fields.
        if before.max_hold_granules != after.max_hold_granules {
            changed.push("maxHoldGranules");
        }
        if self.carry_allowance_changed() {
            changed.push("included_allowance");
        }
        changed
    }

    /// Did a **plan-scoped** allowance move (D-129)?
    ///
    /// Guarded only where a `carry` policy is involved, on either side. A
    /// `carry` declaration compiles into a plan-scoped, revision-immutable grant
    /// row, so changing one — or introducing one, or removing one — is a change
    /// to an artifact a supersession has no revision in which to rewrite. A
    /// `none`-policy allowance compiles to nothing plan-scoped and stays a free
    /// row-local lever, so `none -> none` quantity changes pass.
    fn carry_allowance_changed(&self) -> bool {
        let (before, after) = (
            self.predecessor.included_allowance,
            self.successor.included_allowance,
        );
        if before == after {
            return false;
        }
        before.is_some_and(|allowance| allowance.rollover_policy.is_carry())
            || after.is_some_and(|allowance| allowance.rollover_policy.is_carry())
    }
}

/// `inst-tb-supersession-units` — the guard itself.
///
/// Binds every **usage** row (`per_unit` / `graduated` / `volume` / `package`).
/// A non-usage key has no continued counter to protect, so the guard says
/// nothing about it and a recurring row's supersession is free to change
/// whatever the row-local rules allow.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct SupersessionUnitGuard;

impl ValidationRule<SupersessionPair> for SupersessionUnitGuard {
    fn name(&self) -> &'static str {
        "inst-tb-supersession-units"
    }

    fn evaluate(&self, subject: &SupersessionPair, report: &mut ValidationReport) {
        if !subject.successor.is_usage() {
            return;
        }
        let changed = subject.mismatched_unit_fields();
        if changed.is_empty() {
            return;
        }
        report.violate(
            SUPERSESSION_UNIT_MISMATCH,
            subject.successor.subject(),
            format!(
                "the successor changes {}; the tier counter continues across supersession, \
                 so what it is denominated in, derived from and priced by must not move. \
                 A structural change routes through plan revisioning, not a supersession",
                changed.join(", ")
            ),
        );
    }
}

#[cfg(test)]
#[path = "supersession_tests.rs"]
mod supersession_tests;
