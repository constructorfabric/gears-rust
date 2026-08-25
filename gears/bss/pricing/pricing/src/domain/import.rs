//! Bulk import, Phase 1 — the checks that need no store
//! (`design/12-operator-efficiency.md` §3 `algo-bulk-import` `inst-bk-phase1`,
//! §4 `inst-bi-validate`; D-118, D-148).
//!
//! # Why a Phase 1 at all, and why part of it lives here
//!
//! `inst-bk-phase1` is **validate all-or-nothing**: one invalid row blocks the
//! whole batch pre-commit, with a per-row report naming every violation. That
//! posture only pays for itself if the report is complete — an operator fixing a
//! batch one refusal at a time is doing what the all-or-nothing rule exists to
//! spare them — so a rule that can answer for a row **must** answer for every row
//! rather than stopping at the first.
//!
//! Phase 1's rules split by what they need. Some need the store: whether the row
//! addresses a **published** scope key (`IMPORT_TARGETS_PUBLISHED`, D-118),
//! whether its key holds a pending approval unit (D-35), whether its `If-Match`
//! is current. Others need only the batch, and those are here. Keeping them apart
//! is not tidiness: the batch-only rules are the ones a caller can run before it
//! has opened anything, and they are the ones whose behaviour a test can state
//! without building a world.
//!
//! Three arms are batch-only: the in-batch duplicate below, the unbuilt Slice-10
//! primitive (D-177), and the **key contradiction** (D-312). The third needs no
//! store for the same reason it needs no later call — both its operands are in the
//! row, and one of them is the frozen scope key. It is a *row-local* rule, which
//! Phase 1 had none of before and whose absence the D-312 entry asserted away; see
//! [`key_contradictions`].
//!
//! # The in-batch duplicate, and the key it is judged on (D-148)
//!
//! Two rows on one canonical scope key are a duplicate **inside** the batch and
//! fail Phase 1 per-row. The key is the **whole** [`ScopeKey`], usage line
//! included, which is the column list of `uq_pricing_price_scope_key_draft` as
//! `uq_pricing_price_scope_key_current` **widened** it: `COALESCE(meter, '')` and `dimension_key`
//! are in the index, so two rows differing only in their usage line are two
//! keys and both author.
//!
//! The index also carries `tenant_id`, which [`ScopeKey`] does not, so this rule
//! is **stricter than the index by one column** — harmless because a bulk
//! operation carries one tenant (`pricing_bulk_operation` has a `tenant_id` and
//! no `plan_id`, so a run is single-tenant and may span plans). That premise is
//! written here rather than left implicit, an unstated premise about the schema
//! being exactly how the first version of this rule went wrong.
//!
//! **This was got wrong once and the wrong answer is worth keeping visible**
//! (D-283). The first build read `pricing_price`, which created the index in
//! its original eight-axis form, and judged the duplicate on those eight — so it
//! refused a batch the store admits, and the batch it refused was D-103's
//! confirmed worked example, a `PaaS` plan pricing cloudlets, storage and egress.
//! D-196 made the usage pair normative axes of the canonical key precisely to
//! make that plan expressible. **Read the schema, not a migration**: a `CREATE
//! INDEX` is a statement about the day it ran.
//!
//! # Both sides of a duplicate are named
//!
//! A collision has two rows in it and neither is more at fault than the other:
//! the operator has to see both to decide which one they meant. Reporting only
//! the second would also make the report depend on the order the rows arrived in,
//! which is not a property of the batch.

use std::collections::HashMap;

use toolkit_macros::domain_model;

use crate::domain::concurrency::RowVersion;
use crate::domain::price_record::{PriceContent, authored_content};
use crate::domain::publish::rules::{PRIMITIVE_RULES_UNBUILT, unjudged_primitives};
use crate::domain::rules::price_row_rules;
use crate::domain::scope_key::ScopeKey;

/// The wire code for two rows on one canonical scope key.
///
/// The same code the store raises when a row loses that race to a concurrent
/// author (`infra::error_mapping`); one collision, one code, whichever end of the
/// pipeline notices it. Phase 1 catching it earlier is a better *report*, not a
/// different fault.
pub const DUPLICATE_SCOPE_KEY: &str = "DUPLICATE_SCOPE_KEY";

/// The wire code for a row aimed at a **published** row's scope key.
///
/// D-118 pins the import to the draft plane: an import row lands as a draft, on
/// a new key or over an existing draft. Published rows are append-only and change
/// only through D-88's supersession units at a bounded changeover instant —
/// machinery this path has none of, having no instant in its API and no window
/// operations. The remediation is named in the refusal because it exists: a
/// **repricing run**, the sibling that was explicitly built on those units.
pub const IMPORT_TARGETS_PUBLISHED: &str = "IMPORT_TARGETS_PUBLISHED";

/// One row an operator submitted.
///
/// `if_match` is `None` for a row claiming a **new** scope key and `Some` for a
/// row editing an existing draft under its version (`inst-bk-phase2`: the token
/// is the price row's own column, D-141). Phase 1 does not resolve it — that
/// needs the store — but it is carried here because the duplicate rule below has
/// to treat "a new row on this key" and "an edit of the row on this key" as the
/// same occupancy: two rows aimed at one draft row collide whether or not either
/// claims to own it already.
#[domain_model]
#[derive(Clone, Debug)]
pub struct ImportRow {
    /// The canonical key the row would occupy.
    pub scope_key: ScopeKey,
    /// The authored content.
    pub content: PriceContent,
    /// The version the row asserts, when it edits an existing draft.
    pub if_match: Option<RowVersion>,
}

/// One violation against one row.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RowViolation {
    /// The machine-readable code, which is the discriminator a client acts on.
    ///
    /// `Serialize` on these three because `inst-bk-idem` **replays
    /// this report**: it is stored on `pricing_bulk_operation.report` and handed
    /// back verbatim to a retry. Deriving them is what makes the stored shape
    /// *this* type rather than a hand-built JSON object beside it — the second
    /// spelling D-285 named and did not remove.
    ///
    /// **`Serialize` only.** `Deserialize` would build `rows` from arbitrary JSON —
    /// two entries for one row, or out of batch order — which is exactly the
    /// invariant the private field exists to make structural. Nothing in the crate
    /// reads a report back into these types; the stored column leaves as an opaque
    /// value, so the round trip `inst-bk-idem` needs is served by one direction.
    ///
    /// A `String` rather than the `&'static str` the constants are, because
    /// `inst-bk-idem` **replays this report**: it is stored on
    /// `pricing_bulk_operation.report` and handed back verbatim to the retry, and
    /// a borrowed-static field cannot come back out of a JSON column. Changing it
    /// after the route freezes the shape would be a migration; changing it now is
    /// an allocation per violation, on a path that has just done a database read.
    pub code: String,
    /// The sentence an operator reads.
    pub detail: String,
}

/// What Phase 1 found against one row, by its position in the submitted batch.
///
/// The **index** identifies the row rather than its key, because a row whose key
/// is the problem cannot be named by that key without ambiguity — that is the
/// duplicate case exactly, where two rows share one.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RowOutcome {
    /// Zero-based position in the submitted batch.
    pub row: usize,
    /// Every violation found against it, never only the first.
    pub violations: Vec<RowViolation>,
}

/// Phase 1's per-row report over a batch.
#[domain_model]
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct BatchReport {
    /// One entry per row that failed, in batch order. A row that passed every
    /// rule has **no** entry — the report is of violations, and a clean row's
    /// absence is what "nothing to fix here" looks like.
    ///
    /// **Private, and that is the invariant made structural rather than hoped
    /// for.** [`BatchReport::add`] binary-searches this vector, so an out-of-order
    /// or appended-to `rows` silently produces the two-entries-per-row state
    /// `add` exists to prevent. D-283's defect was an invariant nobody could see;
    /// this one is enforced by the module boundary instead of by a doc asking the
    /// next author nicely.
    rows: Vec<RowOutcome>,
}

impl BatchReport {
    /// Record one violation against one row, keeping the report's invariants.
    ///
    /// **The merge API D-283 said was owed.** The store-dependent rules run in
    /// `infra::import`, after `classify`, and a caller appending to `rows`
    /// directly would produce two entries for one row — breaking the
    /// one-entry-per-row invariant this type's doc states and
    /// [`RowOutcome`]'s leans on. Both halves go through here, so there is one
    /// place that establishes it.
    ///
    /// Violations sort by code and rows by index, so a report is a function of
    /// the batch rather than of the order the rules happened to run in.
    pub fn add(&mut self, row: usize, violation: RowViolation) {
        match self.rows.binary_search_by_key(&row, |outcome| outcome.row) {
            Ok(at) => {
                let found = &mut self.rows[at];
                found.violations.push(violation);
                found.violations.sort_by(|a, b| a.code.cmp(&b.code));
            }
            Err(at) => self.rows.insert(
                at,
                RowOutcome {
                    row,
                    violations: vec![violation],
                },
            ),
        }
    }

    /// The outcomes, in batch order.
    #[must_use]
    pub fn rows(&self) -> &[RowOutcome] {
        &self.rows
    }

    /// Whether Phase 1 blocks the batch (`inst-bk-phase1`, O2).
    ///
    /// Any violation at all blocks it: all-or-nothing is the whole posture, and a
    /// report that let some rows through would make "nothing partially validated
    /// sneaks through" untrue by construction.
    #[must_use]
    pub fn blocks_the_batch(&self) -> bool {
        self.rows
            .iter()
            .any(|outcome| !outcome.violations.is_empty())
    }
}

/// Run Phase 1's **batch-only** rules over the submitted rows.
///
/// Every rule answers for every row; nothing short-circuits.
///
/// The store-dependent rules join the same report through
/// [`BatchReport::add`], which is what keeps the one-entry-per-row invariant
/// true of a report both halves have written to.
#[must_use]
pub fn classify(rows: &[ImportRow]) -> BatchReport {
    let mut report = BatchReport::default();
    for (row, found) in duplicate_scope_keys(rows) {
        report.add(row, found);
    }
    for (row, found) in unbuilt_primitives(rows) {
        report.add(row, found);
    }
    for (row, found) in key_contradictions(rows) {
        report.add(row, found);
    }
    report
}

/// Rows whose authored content contradicts their own frozen scope key — D-312's
/// third door.
///
/// # Why this belongs to the import at all
///
/// D-312 refuses a key contradiction at the price `POST` and `PATCH`, on the
/// ground that such a request is complete and knowably unpublishable the instant
/// it arrives. `bulk_imports` is the **third** door a price row enters through, and
/// it needs the refusal too. Reading the batch as "already validated whole in Phase
/// 1" is the premise that leaves it out, and it is false: Phase 1 judges duplicate
/// keys and unbuilt primitives and no row-local rule at all, so a batch carrying
/// `flat` on a `usage` key lands and is refused only at publish — through the one
/// door of the three that lands rows *in bulk*.
///
/// The refusal therefore **moves earlier, it does not move** — the same sentence
/// `unbuilt_primitives` above is built on (D-177). Publish keeps the whole rule set
/// on its own authority; what changes is that an operator hears it while the batch
/// can still be fixed rather than after the rows are committed.
///
/// # The stage subset, not the whole rule set
///
/// Only [`Stage::Write`](crate::domain::validation::Stage) violations are taken.
/// Running the full `price_row_rules()` here would refuse a batch of legitimately
/// incomplete drafts — no `model_kind` yet, no bands yet — which is exactly what
/// §4.2 puts the rule set at publish to permit, and an import lands **drafts**. So
/// this arm inherits D-312's line rather than restating it: the same
/// `write_stage_only()` filter the authoring plane applies, over the same pipeline.
///
/// # The projection is not optional
///
/// The row is judged through [`authored_content`], which fills `charge_kind` **from
/// the key**. Judging `row.content.row` directly would read whatever the caller
/// put there — and the analogous shortcut has already produced two Criticals, both
/// recorded on that function. A usage batch judged un-normalized reads as recurring
/// and is refused outright.
///
/// The codes are **inherited** from the violations, not minted here: one reason,
/// one code, however many surfaces notice it.
fn key_contradictions(rows: &[ImportRow]) -> Vec<(usize, RowViolation)> {
    // Once, not per row: the pipeline is a fresh allocation of every registered
    // rule and a batch is the case where that multiplies.
    let rules = price_row_rules();
    let mut found = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let subject = authored_content(&row.scope_key, row.content.clone()).row;
        let Some(write_stage) = rules.run(&subject).write_stage_only() else {
            continue;
        };
        // `Violation::subject` is dropped rather than carried: it names the row by
        // its key, and [`RowOutcome`] identifies a row by its **index** precisely
        // because a row whose key is at fault cannot be named by it unambiguously.
        for violation in write_stage.violations {
            found.push((
                index,
                RowViolation {
                    code: violation.code,
                    detail: violation.detail,
                },
            ));
        }
    }
    found
}

/// Rows carrying a Slice-10 primitive whose rules or artifacts are unbuilt
/// (D-177).
///
/// **The refusal moves earlier, it does not move.** `domain::publish::rules`
/// already refuses these fields at publish on its own authority (D-179), whatever
/// put the value there, and that backstop stays — its removal has an order, and
/// this is not it. What building this arm changes is *where the operator hears*:
/// in the per-row Phase 1 report, where the batch can still be fixed, instead of
/// at a publish that rejects work already committed.
///
/// It **inherits** `PRIMITIVE_RULES_UNBUILT` rather than minting a second code,
/// which the design set states outright: the code names the reason, not the
/// field, and one reason wants one code however many surfaces notice it.
///
/// **This arm narrowed rather than moving when D-45 landed**, and the narrowing
/// cost nothing here: what it refuses is
/// [`unjudged_primitives`](crate::domain::publish::rules::unjudged_primitives)'
/// list, so the allowance's judged-and-compiled `none` half left all three
/// surfaces in one edit and its `carry` half — whose grant has no
/// `pricing_plan_grant` table — stayed on all three. A second spelling of the
/// list here would have been a second thing to get right.
fn unbuilt_primitives(rows: &[ImportRow]) -> Vec<(usize, RowViolation)> {
    let mut found = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        for (field, missing) in unjudged_primitives(&row.content.row) {
            found.push((
                index,
                RowViolation {
                    code: PRIMITIVE_RULES_UNBUILT.to_owned(),
                    detail: format!(
                        "this row carries `{field}`, and the Slice-10 rules that would judge it \
                         are unbuilt, as is {missing}; the batch stops here, where the row can \
                         still be fixed, rather than at the publish that would otherwise catch it"
                    ),
                },
            ));
        }
    }
    found
}

/// Rows sharing one canonical scope key, each told which other rows it collides
/// with.
fn duplicate_scope_keys(rows: &[ImportRow]) -> Vec<(usize, RowViolation)> {
    let mut occupants: HashMap<_, Vec<usize>> = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        occupants
            .entry(row.scope_key.clone())
            .or_default()
            .push(index);
    }

    let mut found = Vec::new();
    for (_, group) in occupants {
        if group.len() < 2 {
            continue;
        }
        for &index in &group {
            let others: Vec<String> = group
                .iter()
                .filter(|&&other| other != index)
                .map(ToString::to_string)
                .collect();
            found.push((
                index,
                RowViolation {
                    code: DUPLICATE_SCOPE_KEY.to_owned(),
                    detail: format!(
                        "this row's canonical scope key is the same as row(s) {}'s — {}; the \
                         draft plane admits one row per key, so one of them has to change or \
                         go. Two rows differing in their meter or dimension key are **not** \
                         this case: those are two keys and both author (D-196, D-283).",
                        others.join(", "),
                        rows[index].scope_key
                    ),
                },
            ));
        }
    }
    found
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod import_tests;
