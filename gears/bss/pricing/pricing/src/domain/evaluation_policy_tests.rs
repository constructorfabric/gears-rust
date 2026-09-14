//! The guard that makes the generation worth stamping.
//!
//! Every assertion here is anchored in `design/01-foundation.md` §4.4 rather
//! than in this crate. That is the whole design: a guard whose expectation lives
//! beside the code it guards is satisfied by the same edit that breaks it, and a
//! generation satisfied that way is a constant asserting a stability nobody
//! checked — on posted money, where the segment's only job is to tell one
//! evaluation semantics from another.
//!
//! What an author adding a field to `PriceRow` meets, in order:
//!
//! 1. **A compile error** in `partition_row_fields` (E0027) — no rest pattern,
//!    so the field must be named and classified.
//! 2. **`the_roster_is_the_documents_roster`** or
//!    **`the_out_of_roster_set_is_the_documents`**, depending on which side they
//!    put it — the document has not heard of the field either way.
//! 3. If the field is rostered, making that green needs a **new log line**, and
//!    `the_declared_generation_is_the_last_one_in_the_log` plus
//!    `the_crate_stamps_the_generation_the_document_declares` then require the
//!    document's generation and this crate's constant to move with it.
//!
//! The one evasion left open is editing an existing log line, and it is left
//! open on purpose: backdating a field into a past generation falsifies what a
//! numbered decision admitted, in a normative document. That is not a shortcut
//! through a guard, it is a different act.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;

use super::{EVALUATION_POLICY_GENERATION, partition_plan_fields, partition_row_fields};
use crate::domain::contracts::{PlanChangeContract, UsageCounterOnPlanChange};
use crate::domain::price_row::PriceRow;
use crate::domain::scope_key::ChargeKind;

/// The document that declares the roster, the log and the generation.
///
/// Embedded rather than read at run time, for two reasons and not one. The
/// domain layer may not import infrastructure — `std::fs` in this module is a
/// DE0301 error, and a guard that made the domain read files to prove a rule
/// would be buying the rule with the layering. And `include_str!` makes the
/// document a **build input**: a wave that edits §4.4 recompiles this test, and
/// a repository that lost the file would not build at all rather than skip a
/// check.
const FOUNDATION: &str = include_str!("../../../docs/design/01-foundation.md");

/// The fenced block's opening line, and the whole reason this file can find it.
const BLOCK_ANCHOR: &str = "evaluation-policy-generation:";

/// A row whose only purpose is to give the exhaustive pattern something to
/// match — the partition is a statement about the *shape* of a price row and
/// never about a value in one.
/// Any plan-change contract: the partition reads the **shape**, never the values.
fn any_plan_contract() -> PlanChangeContract {
    PlanChangeContract {
        allowed_change_targets: None,
        comparability_rank: None,
        usage_counter_on_plan_change: UsageCounterOnPlanChange::Reset,
    }
}

fn any_row() -> PriceRow {
    PriceRow {
        charge_kind: ChargeKind::Recurring,
        model_kind: None,
        amount_minor: None,
        unit_rate: None,
        bands: Vec::new(),
        package_size: None,
        package_price_minor: None,
        quantity_source: None,
        manual_quantity: None,
        meter: None,
        dimension_key: String::new(),
        billing_granularity: None,
        tier_aggregation_window: None,
        tier_qualification_window: None,
        aggregation_function: None,
        aggregation_granularity: None,
        max_hold_granules: None,
        included_allowance: None,
        reserved_rate: None,
        reservation_flavor: None,
        min_qty_purchase: None,
        min_qty_usage: None,
        min_qty_usage_fallback: None,
        discount_ref: None,
    }
}

/// The §4.4 block, as the guard reads it.
struct DeclaredBlock {
    generation: String,
    /// `(generation, decision, op, field)`, in document order.
    log: Vec<(String, String, char, String)>,
    outside: BTreeSet<String>,
}

/// Read the one fenced block of `01-foundation.md` §4.4 that opens with
/// [`BLOCK_ANCHOR`].
///
/// Deliberately a fenced block rather than a prose list or a markdown table: the
/// surrounding section is edited by every wave, and a guard that parsed prose
/// would fail on a reflow and pass on a lie.
fn declared_block() -> DeclaredBlock {
    let mut blocks = FOUNDATION.split("```").filter(|b| b.contains(BLOCK_ANCHOR));
    let block = blocks
        .next()
        .expect("01-foundation.md declares the evaluation-policy block (D-162, section 4.4)");
    assert!(
        blocks.next().is_none(),
        "exactly one block declares the roster; two would be two guards that can differ"
    );

    // A split on the fence leaves the info string as the block's first line.
    let (info, body) = block
        .split_once('\n')
        .expect("a fenced block has a body under its info string");
    assert_eq!(
        info.trim(),
        "text",
        "the block is fenced as `text`; a language tag would invite a renderer to reflow it"
    );

    let mut generation = String::new();
    let mut log = Vec::new();
    let mut outside = BTreeSet::new();
    let mut section = "";

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix(BLOCK_ANCHOR) {
            generation = value.trim().to_owned();
            continue;
        }
        if trimmed == "log:" || trimmed == "outside:" {
            section = if trimmed == "log:" { "log" } else { "outside" };
            continue;
        }
        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        match section {
            "log" => {
                assert_eq!(
                    fields.len(),
                    4,
                    "a log line is `ep-<n>  <decision>  <+|->  <field>`, not {trimmed:?}"
                );
                let op = fields[2]
                    .chars()
                    .next()
                    .expect("a log line names an operation");
                assert!(op == '+' || op == '-', "unknown log operation {op:?}");
                log.push((
                    fields[0].to_owned(),
                    fields[1].to_owned(),
                    op,
                    fields[3].to_owned(),
                ));
            }
            "outside" => {
                // `<field>  <reason>` — the reason is prose for a reader and is
                // not matched, but its presence is: a field filed outside the
                // roster with no stated reason is the silent classification this
                // guard exists to prevent.
                assert!(
                    fields.len() >= 2,
                    "an out-of-roster field states why it is out: {trimmed:?}"
                );
                assert!(
                    outside.insert(fields[0].to_owned()),
                    "{} is listed twice",
                    fields[0]
                );
            }
            _ => panic!("a line outside both sections of the block: {trimmed:?}"),
        }
    }

    assert!(!generation.is_empty(), "the block declares a generation");
    assert!(!log.is_empty(), "the block declares a log");
    DeclaredBlock {
        generation,
        log,
        outside,
    }
}

/// The roster the document declares, obtained by replaying its log.
///
/// Replayed rather than read from a second list on purpose: the log is the
/// append-only record of *which decision* moved the set, and deriving the roster
/// from it is what makes a new field impossible to add without a new generation.
fn replayed_roster(block: &DeclaredBlock) -> BTreeSet<String> {
    let mut roster = BTreeSet::new();
    for (generation, decision, op, field) in &block.log {
        assert!(
            decision.starts_with("D-"),
            "every log entry names the decision that made it, not {decision:?}"
        );
        match op {
            '+' => assert!(
                roster.insert(field.clone()),
                "{field} is added twice, at {generation}"
            ),
            _ => assert!(
                roster.remove(field),
                "{field} is removed at {generation} without ever having been added"
            ),
        }
    }
    roster
}

#[test]
fn the_roster_is_the_documents_roster() {
    let block = declared_block();
    // The **union** of both partitions since `ep-2`: the roster outgrew `PriceRow`
    // when Slice 6 landed `usage_counter_on_plan_change`, which a snapshot freezes
    // and no row carries. Each struct keeps its own exhaustive destructure, because
    // that is the part that fails to compile when a field is added.
    let (row_roster, _) = partition_row_fields(&any_row());
    let (plan_roster, _) = partition_plan_fields(&any_plan_contract());

    assert_eq!(
        row_roster
            .iter()
            .chain(plan_roster.iter())
            .map(|f| (*f).to_owned())
            .collect::<BTreeSet<_>>(),
        replayed_roster(&block),
        "the fields this crate calls evaluation policy are not the ones \
         01-foundation.md section 4.4 declares -- if a field was added to PriceRow, \
         append a log line for it and bump the generation"
    );
}

#[test]
fn the_out_of_roster_set_is_the_documents() {
    let block = declared_block();
    let (_, row_outside) = partition_row_fields(&any_row());
    let (_, plan_outside) = partition_plan_fields(&any_plan_contract());

    assert_eq!(
        row_outside
            .iter()
            .chain(plan_outside.iter())
            .map(|f| (*f).to_owned())
            .collect::<BTreeSet<_>>(),
        block.outside,
        "a PriceRow field was filed outside the roster without the document saying so -- \
         both halves are pinned, so that a new field cannot be classified silently"
    );
}

#[test]
fn every_field_of_the_row_is_classified_exactly_once() {
    let (roster, outside) = partition_row_fields(&any_row());

    // No stated total, and none is available: `partition_row_fields` generates its
    // destructure from the two lists, so a field the row grows and neither list
    // names is a non-exhaustive pattern rather than anything a number here could
    // catch. A literal total is the opposite check -- it fails when the lists grow
    // and the literal does not, which is bookkeeping, and it passes over the field
    // that was never classified at all.
    //
    // What is left is the half a pattern cannot see: one field written into both
    // lists, or one name written twice.
    let union: BTreeSet<&str> = roster.iter().chain(outside.iter()).copied().collect();
    assert_eq!(
        union.len(),
        roster.len() + outside.len(),
        "a field is classified twice or named twice"
    );
}

#[test]
fn every_field_of_the_plan_contract_is_classified_exactly_once() {
    let (roster, outside) = partition_plan_fields(&any_plan_contract());

    // Derived, for `every_field_of_the_row_is_classified_exactly_once`'s reason:
    // the destructure comes from these lists, so an unclassified field does not
    // compile and a total here would only restate what the pattern already refuses.
    let union: BTreeSet<&str> = roster.iter().chain(outside.iter()).copied().collect();
    assert_eq!(
        union.len(),
        roster.len() + outside.len(),
        "a field is classified twice or named twice"
    );
}

#[test]
fn the_declared_generation_is_the_last_one_in_the_log() {
    let block = declared_block();

    let mut seen: Vec<&str> = Vec::new();
    for (generation, _, _, _) in &block.log {
        if seen.last() != Some(&generation.as_str()) {
            assert!(
                !seen.contains(&generation.as_str()),
                "{generation} is written in two places; the log is append-only"
            );
            seen.push(generation);
        }
    }
    for (index, generation) in seen.iter().enumerate() {
        assert_eq!(
            *generation,
            format!("ep-{}", index + 1),
            "generations run ep-1, ep-2, ... without gaps"
        );
    }
    assert_eq!(
        seen.last().copied(),
        Some(block.generation.as_str()),
        "the declared generation is the last one the log reached -- a field set that \
         moved without a bump is exactly what this refuses"
    );
}

#[test]
fn the_crate_stamps_the_generation_the_document_declares() {
    assert_eq!(EVALUATION_POLICY_GENERATION, declared_block().generation);
}

#[test]
fn the_generation_is_shaped_the_way_the_decision_declares_it() {
    // `ep-<n>`, n a positive integer: consumers compare it for equality and read
    // nothing out of it, so the only thing the format has to guarantee is that
    // two field sets never share a string.
    let ordinal = EVALUATION_POLICY_GENERATION
        .strip_prefix("ep-")
        .expect("the generation is `ep-<n>`");
    let ordinal: u32 = ordinal.parse().expect("`<n>` is an integer");
    assert!(ordinal >= 1);
}
