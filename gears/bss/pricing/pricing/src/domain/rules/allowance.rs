//! Slice 10's included-allowance gate and its compiled-set check
//! (`inst-ac-gate`, `inst-ac-band`; D-45, D-59, D-130).
//!
//! Row-local for `rules::reservation`'s reasons: every one of these judges **one
//! row against itself** — its charge kind, its declaration, its own kind, its own
//! aggregation function, its own bands, its own reservation pair. None reads
//! another row, the plan, or any tenant state, which is D-21's test for the
//! save-time set, and it puts them where the joint corpus can reach them.
//!
//! # The six refusals and the compile land together, deliberately
//!
//! D-177's rejected option was *building the ten gates now, without the compile*:
//! a `graduated` row with `includedAllowance {100, none}` would pass every gate
//! and publish with no `$0` band prepended and no marker materialized, so Rating
//! bills the first hundred units at the authored rate — an allowance that was
//! accepted, **validated**, and silently ignored. The gate and
//! [`crate::domain::allowance::compile`] therefore read the same conditions from
//! one place ([`crate::domain::allowance::Admissibility::of`]) and land in one
//! change.
//!
//! # What is still refused, and why it is not one of these six
//!
//! `rolloverPolicy = carry` compiles to a `promotional` grant in
//! `pricing_plan_grant` (`inst-ac-carry`, D-52) — a table this gear does not
//! have. It keeps `PRIMITIVE_RULES_UNBUILT`
//! ([`crate::domain::publish::rules::unjudged_primitives`]), which names the
//! **reason** rather than the field precisely so that a half nobody can honour
//! yet needs no seventh `ALLOWANCE_*` code. `tierQualificationWindow` is
//! untouched by this module and refused exactly as before.

use bss_fixtures::ModelKind;
use toolkit_macros::domain_model;

use crate::domain::allowance::{authors_a_free_first_band, compile, offsets_saturate};
use crate::domain::price_row::{PriceRow, model_kind_wire};
use crate::domain::rules::tier_bands::{BandGeometry, BandOrigin, BandTopOpen};
use crate::domain::validation::{Stage, ValidationReport, ValidationRule};

/// `includedAllowance` on a row whose `chargeKind` is not `usage` (`inst-ac-gate`,
/// S10 §5).
///
/// An allowance is *N units of the metered thing included in the subscription*,
/// so on a recurring or one-off row there is no meter, no counter and no
/// quantity for `N` to be a quantity **of** — and the compile has no band ladder
/// to prepend a `$0` band to.
///
/// **Judged at the authoring write** (D-312), unlike its five siblings. Both
/// operands are present at the instant of the request and one of them is frozen:
/// `chargeKind` is a component of the canonical scope key and the key is
/// immutable after create, so the only call that resolves this fault retracts the
/// field the caller just sent. That is the category D-312 names, and
/// `RESERVATION_ON_NON_USAGE` is its sibling on the same axis.
pub const ALLOWANCE_ON_NON_USAGE: &str = "ALLOWANCE_ON_NON_USAGE";

/// An authored `$0` first band beside a declaration (`inst-ac-gate`, S10 §5).
///
/// The compile prepends `[0, N) @ $0`; a row that already authors a free opening
/// band would grant the free quantity twice, and the two quantities need not even
/// agree. The author means one of the two shapes and the gear refuses to guess
/// which.
///
/// **The code recovers its literal meaning under D-130** and it is worth saying
/// why the rule is safe: the compile is a projection, so no `$0` band is ever
/// written back onto the row, and this can no longer fire against the compiler's
/// own output on a re-publish, a supersession, a repricing successor or a clone.
/// An in-place rewrite instead of a projection does exactly that, and blocks the
/// reprice of every allowance-carrying row.
pub const ALLOWANCE_DOUBLE_FREE: &str = "ALLOWANCE_DOUBLE_FREE";

/// `includedAllowance` on a non-`sum` row (`inst-ac-gate`, D-44/D-45 launch
/// boundary, S10 §5).
///
/// On a level row `Q` is a sum of per-granule folds of a level, so "the first N
/// units are free" has no single meaning — free per granule or free per window,
/// and the two bill differently. Level-meter allowance is a named Future gate,
/// the same posture D-53 takes for `consumption` reservations on the same rows.
pub const ALLOWANCE_ON_NON_SUM: &str = "ALLOWANCE_ON_NON_SUM";

/// `quantity <= 0` (`inst-ac-gate`, S10 §5).
///
/// `quantity` is a `u64`, so the reachable half of the design set's condition is
/// `0` — an allowance of nothing, which is not a smaller allowance but an absent
/// one. It is refused rather than normalised away, because the two states are
/// different: an absent declaration carries no marker and a zero one would carry
/// a marker saying "includes 0 units" beside a zero-width `[0, 0)` band.
pub const ALLOWANCE_QUANTITY_INVALID: &str = "ALLOWANCE_QUANTITY_INVALID";

/// The row's `modelKind` has nothing to compile into (`inst-ac-gate`, D-59, S10
/// §5).
///
/// Three kinds, one code, because the operator's next action is the same for all
/// three — change the kind or drop the allowance:
///
/// - **`package`** — package and bands are structurally exclusive (Slice 3), so
///   there is no band set to compile into at all;
/// - **`volume`** — catalog `volume` is Tariffs **Variant A** (the selected
///   band's single rate applies to the **total** `Q`), under which a `$0` first
///   band is a **cliff** and not an allowance: with `N = 100` and a `$1` band,
///   `Q = 99` bills `$0` and `Q = 101` bills `$101`, while the marker
///   simultaneously displays "includes 100 units". Expressing it would need
///   `(Q − N) × rate`, which the band model cannot state, so it is refused rather
///   than approximated (D-59);
/// - **`flat`** — prices no quantity, so it counts none.
///
/// `graduated` and untiered `per_unit` are the whole authorable set.
pub const ALLOWANCE_KIND_UNSUPPORTED: &str = "ALLOWANCE_KIND_UNSUPPORTED";

/// `includedAllowance` on a row carrying reservation attributes (`inst-ac-gate`,
/// L-6, S10 §5).
///
/// `inst-rv-tier-q` excludes the matched reserved quantity from the on-demand
/// tier counter and restarts the remainder's `Q` at 0; the compiled `[0, N)` band
/// would then grant that remainder another `N` free units. Whether the allowance
/// nets the reservation first or stacks on top of it is undecided, so the
/// combination fails publish rather than compiling an unreviewed double benefit —
/// a named Future gate, the same posture as D-53.
pub const ALLOWANCE_WITH_RESERVATION: &str = "ALLOWANCE_WITH_RESERVATION";

/// `inst-ac-gate` — the six conditions between an authored allowance and a
/// compiled one.
///
/// One rule for all six, for `ReservationWellFormed`'s reason: they are six
/// readings of one authored fact, and an author who declared an allowance wrongly
/// should see every way it is wrong in one report rather than one per re-publish.
///
/// The order is the order an author fixes them in and the order
/// [`crate::domain::allowance::Admissibility::of`] reads them: may this row carry an allowance at all, is
/// the declaration a quantity, does the kind have a band set, does the meter's
/// shape admit one, and only then the two content collisions.
///
/// # Every condition is read independently, and that is deliberate
///
/// [`crate::domain::allowance::Admissibility::of`] answers **one** reason because a compile is a yes/no;
/// this re-reads the conditions so the report enumerates all of them. The two
/// cannot drift into disagreement about *whether* a row compiles — that question
/// has one implementation — only about how many sentences the author gets.
#[domain_model]
#[derive(Clone, Copy, Debug)]
pub struct AllowanceAuthorable;

impl ValidationRule<PriceRow> for AllowanceAuthorable {
    fn name(&self) -> &'static str {
        "inst-ac-gate"
    }

    fn evaluate(&self, subject: &PriceRow, report: &mut ValidationReport) {
        let Some(allowance) = subject.included_allowance else {
            return;
        };

        if !subject.is_usage() {
            // D-312: the declaration and a frozen non-usage `chargeKind`, both
            // in the request. See the code's own doc.
            report.violate_at_write(
                ALLOWANCE_ON_NON_USAGE,
                subject.subject(),
                format!(
                    "this row's chargeKind is {} and it declares includedAllowance; an allowance \
                     is N units of a metered thing included in the subscription, so a row with no \
                     meter and no tier counter has nothing for N to be a quantity of",
                    subject.charge_kind
                ),
            );
        }

        if allowance.quantity == 0 {
            report.violate(
                ALLOWANCE_QUANTITY_INVALID,
                subject.subject(),
                "includedAllowance.quantity is 0; an allowance of nothing is an absent \
                 allowance, and the two are different rows: one carries no marker, the other \
                 would carry one reading `includes 0 units` beside a zero-width [0, 0) band"
                    .to_owned(),
            );
        }

        // Reads `model_kind`, so it returns when nothing was authored:
        // `inst-mk-explicit` owns that refusal and re-deriving a per-kind
        // consequence from an absent kind fills the report with consequences of
        // the one fault the author has to fix first. The five conditions around
        // it read no kind and are judged whether or not one has been picked --
        // D-312's line, which the first implementation of `inst-mk-forbidden`
        // got wrong in exactly this place.
        if let Some(kind) = subject.model_kind
            && !matches!(kind, ModelKind::Graduated | ModelKind::PerUnit)
        {
            report.violate(
                ALLOWANCE_KIND_UNSUPPORTED,
                subject.subject(),
                format!(
                    "includedAllowance is authorable on graduated rows and on untiered per_unit \
                     usage rows only; this row is {}. A package row has no band set to compile \
                     into, and on a volume row -- Tariffs Variant A, one rate on the whole Q -- a \
                     $0 first band is a cliff rather than an allowance (D-59)",
                    model_kind_wire(kind)
                ),
            );
        }

        // No `is_usage()` gate, unlike `LevelMaxHold`'s requirement arm: both
        // operands here are fields the request carries, so this is one more way
        // the authored row is wrong rather than a consequence of another rule's
        // refusal. The arm that must not fire on a non-usage key is the one that
        // demands a field the author did not write.
        if subject.is_level() {
            report.violate(
                ALLOWANCE_ON_NON_SUM,
                subject.subject(),
                format!(
                    "this row derives Q with {} and declares an includedAllowance; on a level \
                     meter Q is a sum of per-granule folds, so `the first N units are free` could \
                     mean free per granule or free per window and the two bill differently. \
                     Level-meter allowance is a named Future gate",
                    subject.effective_aggregation_function().as_str()
                ),
            );
        }

        if subject.is_reserved() {
            report.violate(
                ALLOWANCE_WITH_RESERVATION,
                subject.subject(),
                "this row carries reservation attributes and an includedAllowance; the reserved \
                 remainder already restarts Q at 0 (inst-rv-tier-q), so the compiled [0, N) band \
                 would stack a second free quantity on top of it. Whether the allowance nets the \
                 reservation or stacks on it is undecided, so the combination is refused rather \
                 than compiled"
                    .to_owned(),
            );
        }

        if authors_a_free_first_band(subject) {
            report.violate(
                ALLOWANCE_DOUBLE_FREE,
                subject.subject(),
                format!(
                    "this row authors a $0 band at the origin and declares includedAllowance \
                     {{quantity {}}}; the compile prepends its own [0, {}) $0 band, so the free \
                     quantity would be granted twice and the two need not even agree. Author one \
                     of the two shapes",
                    allowance.quantity, allowance.quantity
                ),
            );
        }
    }
}

/// `inst-ac-band` — the **compiled** band set is judged by the Slice-3 band
/// rules.
///
/// *"Publish still validates the compiled set against the standard Slice 3 band
/// rules (ordering/contiguity/open top)"* — this is that sentence. The compiled
/// set is what a consumer rates from, so it is the set that must be a geometry,
/// and compile-equivalence (AC #90a) is the claim that it is byte-identical in
/// evaluation terms to the hand-authored equivalent.
///
/// # It is not a duplicate of the authored-set check, and it is not vacuous
///
/// Offsetting a well-formed ladder by `+N` and prepending `[0, N)` preserves the
/// geometry exactly, so against a clean authored set this rule finds nothing —
/// which is the correct outcome and not evidence that it does nothing. The fault
/// it exists for is the one the **compile** introduces: `from_qty + N` saturates
/// at `u64::MAX`. Nothing else in the gear would notice, because the malformed
/// set is never stored.
///
/// # The saturation is reported as itself, not inferred from the geometry
///
/// **The geometry alone does not surface a saturated bound**, and the premise
/// that a saturated bound *produces* a malformed set is false. `N = u64::MAX` on
/// the ladder
/// `[0, 1000)`, `[1000, null)` collapses both bounds onto `u64::MAX` and the
/// zero-width band between them is caught; `N = u64::MAX - 5` on the same ladder
/// compiles to `[0, N) @ $0`, `[N, u64::MAX)`, `[u64::MAX, null)` — contiguous,
/// origin-anchored, open-topped, and silent under all three rules — while the
/// authored 1000-unit band is materialized **five** units wide. So
/// [`offsets_saturate`](crate::domain::allowance::offsets_saturate) is asked
/// directly, and the code is `ALLOWANCE_QUANTITY_INVALID`: the authored ladder is
/// well-formed and stays so at every smaller `N`, so the remediable operand is
/// the declared quantity.
///
/// That refusal does **not** wait on the authored set being clean. It is a fault
/// of the declaration rather than a consequence of the ladder, so an author whose
/// ladder also has a hole is told about both — two operands, two edits.
///
/// The **geometry** half does wait, so an author whose ladder has a hole is told
/// once rather than twice about one edit. The compiled set is derived from that
/// ladder; reporting its derived faults beside the real one would put the
/// consequence ahead of the cause.
#[domain_model]
#[derive(Clone, Copy, Debug)]
pub struct CompiledAllowanceWellFormed;

impl ValidationRule<PriceRow> for CompiledAllowanceWellFormed {
    fn name(&self) -> &'static str {
        "inst-ac-band"
    }

    fn evaluate(&self, subject: &PriceRow, report: &mut ValidationReport) {
        let Some(compiled) = compile(subject) else {
            return;
        };
        if offsets_saturate(subject) {
            let quantity = subject
                .included_allowance
                .map_or(0, |allowance| allowance.quantity);
            // `violate_at_write` under D-312's amended criterion: both operands
            // -- the declared quantity and the authored band bounds -- are in
            // the request, and no later call can make a saturating offset fit.
            //
            // It is also the **only remediable** operand of this fault, which is why
            // the stage matters: the door keeps `write_stage_only()`, so a
            // publish-stage refusal here is filtered out while the derived band
            // complaint below is not, and the author never sees the one they can act on.
            report.violate_at_write(
                ALLOWANCE_QUANTITY_INVALID,
                subject.subject(),
                format!(
                    "includedAllowance.quantity is {quantity}; offsetting this row's authored \
                     band bounds by it overflows, so the compiled ladder silently narrows the \
                     bands it was supposed to shift. Author a quantity the ladder's top bound \
                     can be raised by"
                ),
            );
        }
        if !band_report(subject).violations.is_empty() {
            return;
        }
        let mut projected = subject.clone();
        projected.model_kind = Some(compiled.presented_kind);
        projected.bands = compiled.bands;
        // Only the violations: the advisory half (`TIER_BAND_PRICE_INCREASE`)
        // is already reported against the authored ladder, and the compiled set
        // rises out of a free opening band by construction -- the exact shape
        // `BandGeometry` deliberately does not warn about.
        let compiled_report = band_report(&projected);
        for mut violation in compiled_report.violations {
            // **Re-stamped to `Publish`, whatever the band rules stamped.**
            // These findings are about the *compiled* ladder, not about anything
            // the author sent -- the authored set was checked clean three lines
            // up -- so none of them can be remediated at the authoring write.
            //
            // `BandGeometry` stamps `TIER_BAND_EMPTY` at `Stage::Write`, and
            // carrying that stamp through makes the write door
            // refuse a row naming a band `[18446744073709551615,
            // 18446744073709551615)` the request never contained. A derived
            // subject must not produce a write-stage refusal. Review M26.
            violation.stage = Stage::Publish;
            report.violations.push(violation);
        }
    }
}

/// The three Slice-3 band-geometry rules over one row.
///
/// Called rather than registered, because this rule's subject is a **derived**
/// row: running `price_row_rules()` here would recurse through this very rule,
/// and every non-band rule in that pipeline would then judge a row nobody
/// authored.
fn band_report(row: &PriceRow) -> ValidationReport {
    let mut report = ValidationReport::default();
    BandGeometry.evaluate(row, &mut report);
    BandOrigin.evaluate(row, &mut report);
    BandTopOpen.evaluate(row, &mut report);
    report
}

#[cfg(test)]
#[path = "allowance_tests.rs"]
mod allowance_tests;
