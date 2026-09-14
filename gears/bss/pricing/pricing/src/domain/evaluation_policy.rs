//! The evaluation-policy generation — `pricingSnapshotRef`'s third segment.
//!
//! A snapshot ref names the resolved rows (the price ids), the version they were
//! resolved at (the version ref), and **which evaluation-policy field set the
//! frozen row content is to be read under**. That third segment is this module:
//! a declared constant, `ep-<n>`, that moves when a decision moves the field
//! set (`design/01-foundation.md` §4.4, D-162).
//!
//! # Why a constant needs a guard more than it needs a value
//!
//! The segment exists so that a period rated under one evaluation semantics is
//! tellable from a period rated under another. A generation nobody remembers to
//! bump is therefore **worse than no generation**: it asserts a stability that
//! is not there, immutably, on posted money. So the deliverable here is not
//! [`EVALUATION_POLICY_GENERATION`] — it is the chain that fails when the field
//! set moves and the constant does not, and the chain is deliberately anchored
//! outside this file:
//!
//! 1. [`partition_row_fields`] destructures [`PriceRow`] with a pattern that has
//!    **no `..` arm**. Adding a field to the row does not compile here (E0027)
//!    until whoever added it says which side of the roster boundary it falls on.
//!    That is the step an author cannot route around, and it is why the function
//!    takes a row it never reads: Rust has no way to match a struct's fields
//!    exhaustively without a value of it.
//! 2. The **document** is the statement, not this file. `01-foundation.md` §4.4
//!    declares the roster as an append-only log of `ep-<n>  <decision>  ± field`
//!    lines, plus the out-of-roster set. `evaluation_policy_tests.rs` reads that
//!    block, replays the log, and asserts both halves of the partition against
//!    it. A test that merely restated this crate's fields would prove nothing —
//!    the same edit that adds a field would update it.
//! 3. The bump is therefore a **mechanism rather than a request**: a field
//!    cannot enter the roster without a log line, a log line *is* a generation,
//!    the last generation is the declared one, and the declared one is
//!    [`EVALUATION_POLICY_GENERATION`]. There is no edit that adds a rostered
//!    field and leaves the constant where it was.
//!
//! Backdating a field into an existing log line is still possible, and is
//! deliberately not defended against: it falsifies what a numbered decision
//! admitted, in a normative document, which is a different act with a different
//! reviewer.
//!
//! # What the generation does not claim
//!
//! It tracks the field **set**, not the meaning of a field. D-58 made
//! `tier_aggregation_window` mandatory on `package` rows and would not move it;
//! a decision redefining `peak` would not move it either. That is the coverage a
//! content digest would have bought and this does not, and it is stated in §4.4
//! for the same reason it is stated here — a reader is entitled to know which
//! risk they are carrying.
//!
//! # One rostered field still has no rule, and one half of a second has no store
//!
//! `tier_qualification_window` and `included_allowance` are both in the roster
//! below, so the generation stamp tells a consumer both are part of the set an
//! evaluator reads. As of D-45's landing that is **true of the allowance**:
//! `inst-ac-gate`'s six refusals judge it and
//! [`crate::domain::allowance::compile`] honours it, materializing the `$0` band,
//! the offset ladder and the marker into the read model. Two things are still
//! unhonoured and the roster promises them anyway:
//!
//! - `tier_qualification_window` — none of `inst-tt-forbidden` /
//!   `inst-tt-window-pair` / `inst-tt-zero-band` / `inst-tt-fixture` exists, and
//!   neither does the rate lock (`inst-tt-lock`);
//! - `included_allowance` with `rolloverPolicy = carry` — `inst-ac-carry`'s
//!   promotional grant needs `pricing_plan_grant` (D-52) and this gear has no
//!   such table.
//!
//! What holds the line is **three refusals, and the load-bearing one is a rule**:
//! [`crate::domain::publish::rules::NoUnjudgedPrimitive`] is registered in the
//! publish set and refuses both at precheck and again inside the commit
//! transaction; `api::rest::prices::refuse_unlanded_primitives` rejects them on
//! `POST …/plans/{planId}/prices` and `PATCH …/prices/{priceId}`; and
//! `domain::import` inherits the same code on the bulk plane. None of the three is
//! a property of the type, the column or the roster — the domain model, the
//! storage round trip and the D-129 supersession guard all carry both fields quite
//! happily.
//!
//! **The roster does not move for any of this.** D-162 makes it an append-only
//! log: removing a field nobody can author would bump a generation for a
//! fabrication, and adding the allowance's `none` half changed no field name, so
//! `ep-1` is unchanged by a slice landing. That is the property to keep in mind
//! before widening it.
//!
//! **What holds the freeze off is the publish rule, not an unmounted route**
//! (D-298): `POST …/plans/{planId}/publish` is merged in `module.rs` and declared in
//! `module_test`'s path census. The danger is real and the reason it is closed
//! should be readable by whoever next widens this roster — a group that adds a
//! writer of `PriceRow` owes the remaining refusals or a second refusal at its own
//! boundary. No DTO in this gear sets `deny_unknown_fields`, so
//! the surface refusal is also contingent on both members remaining modelled
//! fields rather than silently ignored ones (D-174 clause 1).

use crate::domain::contracts::PlanChangeContract;
use crate::domain::price_row::PriceRow;

/// The evaluation-policy generation this gear stamps.
///
/// Format `ep-<n>`, `n` a positive integer, monotone, and **opaque to consumers
/// except for equality**: two snapshots carrying this string were frozen under
/// the same evaluation-policy field set, and two carrying different strings were
/// not. Nothing else is promised.
///
/// It is the gear's constant rather than a per-publish value — every publish of
/// one generation stamps the same string — and it moves only with the log in
/// `design/01-foundation.md` §4.4, whose last entry it must equal.
pub const EVALUATION_POLICY_GENERATION: &str = "ep-4";

/// Sorts a struct's fields into two named lists **and** destructures the struct
/// from the same two lists.
///
/// One source for both halves, which is what makes the guard unevadable: the `let`
/// pattern is exhaustive and has no rest arm, so a field the struct grows and
/// neither list names does not compile — and because the pattern is generated,
/// there is no `_` an author can reach for to silence it. A hand-written pattern
/// costs exactly that: `new_field: _` satisfies it while the field is classified
/// nowhere, and the generation string goes on claiming coverage it does not have.
///
/// Each returned name is `stringify!` of the field's own identifier, so a name
/// cannot drift into the storage vocabulary or survive a rename.
///
/// The diagnostic a missing field earns here reads *"pattern requires `..` due to
/// inaccessible fields"* and does not name it, because the pattern is expanded
/// rather than written. The field's name comes from the same field addition's other
/// errors — `content_pin` and `projection` destructure the row too and report
/// `E0027` with the name — and the fix is always the same: write the identifier
/// into `roster` or `outside` below, and append its line to `01-foundation.md`
/// section 4.4.
///
/// The compiler's own suggestion on that error — *"ignore the inaccessible and
/// unused fields"* — offers a `..` inside this pattern. Taking it removes the
/// guard entirely and leaves `EVALUATION_POLICY_GENERATION` claiming a field set
/// nothing enumerates.
macro_rules! partition_fields {
    (
        $ty:ident,
        $value:expr,
        roster: [$($rostered:ident),* $(,)?],
        outside: [$($outside:ident),* $(,)?]
    ) => {{
        let $ty {
            $($rostered: _,)*
            $($outside: _,)*
        } = $value;

        (
            vec![$(stringify!($rostered)),*],
            vec![$(stringify!($outside)),*],
        )
    }};
}

/// Every field of [`PriceRow`], sorted into the evaluation-policy roster and out
/// of it.
///
/// The roster is the fields that tell an evaluator **how to derive the billable
/// quantity and select the rate**. Outside it sit the row's identity (the
/// scope-key axis and the metered line it prices), its money, and the fields
/// saying where a quantity *comes from* rather than how it is *derived* — the
/// distinction `EVAL_POLICY_MISPLACED` already draws by naming "an
/// evaluation-policy **or** quantity field".
///
/// Returns `(roster, outside)`, each in this row's declaration order. Both
/// halves are returned because both are pinned: a field added to the row and
/// quietly filed outside the roster would otherwise change nothing anyone has to
/// record, and the guard's job is to make every field of this row a stated
/// classification rather than a silent one.
///
/// # The pattern is the point, and the lists are the pattern
///
/// [`partition_fields`] generates the exhaustive `let` destructure **from these
/// two lists**, so a field added to [`PriceRow`] is a non-exhaustive pattern until
/// its identifier is written into one of them. There is nowhere to put a `_`:
/// hand-writing the pattern with real bindings leaves `new_field: _` satisfying it
/// with the field classified in neither list, which is the one shape an author
/// reaching for the cheapest fix would write.
///
/// Each entry is the field's own name by construction (`stringify!`), so an entry
/// cannot drift into the storage vocabulary or outlive a rename.
#[must_use]
pub fn partition_row_fields(row: &PriceRow) -> (Vec<&'static str>, Vec<&'static str>) {
    partition_fields!(
        PriceRow,
        row,
        roster: [
            model_kind,
            package_size,
            billing_granularity,
            tier_aggregation_window,
            tier_qualification_window,
            aggregation_function,
            aggregation_granularity,
            max_hold_granules,
            included_allowance,
            reservation_flavor,
            min_qty_usage,
            min_qty_usage_fallback,
        ],
        outside: [
            charge_kind,
            amount_minor,
            // The `per_unit` rate (D-311). Outside for `amount_minor`'s reason and
            // the same one: it is the row's price, not a knob an evaluator reads to
            // decide *how* to price.
            unit_rate,
            bands,
            package_price_minor,
            quantity_source,
            manual_quantity,
            meter,
            dimension_key,
            // Outside for `unit_rate`'s reason: a rate is the row's price, not a
            // knob an evaluator reads to decide *how* to price.
            reserved_rate,
            min_qty_purchase,
            discount_ref,
        ]
    )
}

/// The same partition over the **plan-scoped** contract, added by D-113's field.
///
/// # Why a second function rather than a wider first one
///
/// D-162 built the guard around [`PriceRow`] because every rostered field was a
/// row field, and it warned in the same breath that *"a slice that lands \[a
/// plan-scoped member\] and leaves the roster alone leaves the generation claiming
/// more than it covers."* Slice 6 landed exactly that: `usage_counter_on_plan_change`
/// is frozen with the snapshot — D-113 exists because two gears were already reading
/// it *"from the pinned snapshot"* while no gear defined it — so it is evaluation
/// policy by the roster's own test, and it is not on a row.
///
/// The exhaustive destructure is what makes either function worth having, and it
/// cannot span two structs: a pattern that misses a new field must fail to compile.
/// So the roster is the **union** of the two partitions, and each struct keeps its
/// own arm.
///
/// # Why only one of the three is rostered
///
/// `allowed_change_targets` is a permission — whether a self-service change may
/// happen at all — and `comparability_rank` drives the proration sign Subscriptions
/// computes. Neither changes how a rated line is evaluated, which is the roster's
/// subject; both are consumer-contract inputs that a snapshot carries for other
/// reasons.
#[must_use]
pub fn partition_plan_fields(
    contract: &PlanChangeContract,
) -> (Vec<&'static str>, Vec<&'static str>) {
    partition_fields!(
        PlanChangeContract,
        contract,
        roster: [usage_counter_on_plan_change],
        outside: [allowed_change_targets, comparability_rank]
    )
}

#[cfg(test)]
#[path = "evaluation_policy_tests.rs"]
mod evaluation_policy_tests;
