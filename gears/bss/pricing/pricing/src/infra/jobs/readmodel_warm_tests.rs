//! What the sweep decides without a database.
//!
//! One thing, and it is the one the design set names by string: the two alarm
//! constants. §3.6 and §4.4 spell them, an operator's runbook greps for them,
//! and nothing else in this crate would notice a typo. They are on the
//! `PricingAlarm` roster since D-238, so a misspelling here would now disagree
//! with the enum rather than merely with the document — which is why the case
//! below asserts against **literals** rather than against the enum's own
//! rendering. Everything else the pass does is a statement about rows
//! in four tables and is proved in `tests/sqlite_read_model.rs` — including
//! D-237's, which is why the plan-only degraded mark is not asserted here.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::{ALARM_COMMIT_OVERDUE, ALARM_PIN_ELIGIBILITY_OVERDUE};

#[test]
fn the_two_alarm_names_are_the_ones_the_design_set_spells() {
    // Asserted against literals rather than derived, for `CatalogEvent::as_str`'s
    // reason: these are the strings an operator's runbook matches on, and
    // asserting them against `PricingAlarm::as_str` would be asserting the enum
    // against itself. D-238 put both names on that enum, so this case and the
    // roster now have to agree — which is the point of transcribing rather than
    // deriving.
    assert_eq!(
        ALARM_COMMIT_OVERDUE,
        "pricing.catalogversion.commit_overdue"
    );
    assert_eq!(
        ALARM_PIN_ELIGIBILITY_OVERDUE,
        "pricing.readmodel.pin_eligibility_overdue"
    );
}

// A second case used to be here: that a pass which did nothing reports nothing.
// It constructed no job and never called `run`, so what it asserted was
// `#[derive(Default)]` — and of eleven fields it named three, under a name that
// claimed a property of the pass. `jobs/window_activation_tests.rs:16-21` had
// already recorded deleting exactly this shape one file over; the correction had
// been applied to one of the two siblings (review Z4-7). The property it was
// named for belongs to `run` and is asserted of `run` in
// `tests/sqlite_read_model.rs`, which drives a pass over a store with no pending
// refs. Deleted rather than repointed, for that reason.

// A third case used to be here, and it is the second of this shape to go:
// `the_three_undeliverable_signal_flags_are_independent_of_one_another` (review
// RUST-TEST-001, 2026-08-20). It built a `SweepReport::default()`, assigned
// `degraded_emit_failed` and `frontier_block_probe_failed` by hand, and asserted
// the sibling fields were still `false` and the counters still `0` — it
// constructed no job, called no function of `readmodel_warm`, and what it
// asserted was that writing one struct field does not write another. That is a
// compiler guarantee, so the case stayed green against any behavioural change to
// the sweep, **including deleting every `report.… = true` assignment in the
// production code** — which is exactly the swallow the three fields were added to
// make testable. Same defect as the case deleted above, one paragraph later.
//
// **Where the three arms actually are, measured rather than asserted**, because
// the deleted case's own doc claimed all three were driven end to end and that is
// true of one:
//
// * `frontier_scan_failed` — driven in `tests/sqlite_read_model.rs`, which drops
//   the frontier table under a running sweep and asserts the flag rises with the
//   rest of the report byte-identical to a healthy pass's;
// * `degraded_emit_failed` and `frontier_block_probe_failed` — **not driven by
//   any pass**. `module_tests::sweep_is_noteworthy` proves each reaches an
//   operator once raised, which is a real property of a real function, but
//   nothing forces the enqueue or the committed-version read to fail and watches
//   the pass set the flag. Both want a store case that makes the read fail, and
//   that case belongs beside `frontier_scan_failed`'s in
//   `tests/sqlite_read_model.rs` rather than here: this file has no database.
