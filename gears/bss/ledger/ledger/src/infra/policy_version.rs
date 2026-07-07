//! [`PolicyVersionGuard`] — the evidence-ref-reuse half of the §4.6 (AC #15)
//! policy-version rule: a correction MUST REUSE the pinned evidence refs of the
//! posting it corrects, never invent fresh ones.
//!
//! ## What "reuse" means here
//!
//! A correction is a post whose header carries `reverses_entry_id` (it points
//! back at a prior posting — a reversal or a `MAPPING_CORRECTION`). Each journal
//! line can carry a tuple of *pinned evidence refs* — the columns that bind the
//! line to immutable upstream evidence:
//! `(pricing_snapshot_ref, po_allocation_group, price_id, sku_or_plan_ref,
//! invoice_item_ref)`.
//!
//! The rule this guard enforces: **every** evidence-ref tuple that appears on
//! the correction's lines MUST also appear on the ORIGINAL's lines. The
//! correction reuses the original's pinned evidence; it may carry fewer tuples
//! (some lines net out with no refs) but never a tuple the original did not
//! have. A correction tuple absent from the original ⇒
//! [`DomainError::PolicyVersionViolation`].
//!
//! All-NULL tuples (a line with no pinned evidence at all) are ignored on both
//! sides — they carry no policy-version evidence to reuse or violate.
//!
//! ## What this guard does NOT own
//!
//! §4.6 also requires (A-4/P-3) that a correction re-applies the **note-time
//! policy** (the rounding / GL-mapping / period rules as they stood at the
//! *note's own* time) and respects the **recognition-state-at-note-time**. Those
//! rules belong to the credit-note / refund / recognition handlers landing in
//! Slices 3/4, which are not present yet. This guard implements only the
//! evidence-ref-reuse half — the part the existing reversal / mapping-correction
//! flows can already satisfy — and is the seam those later slices extend.
//!
//! ## How the existing flows interact with this guard
//!
//! `domain::invoice::reversal::flip_line` builds a reversal's lines from the
//! original's read-back `LineView`, and that DTO does NOT carry the pinned
//! evidence-ref columns — so a reversal's lines all have an all-NULL evidence
//! tuple, which this guard ignores. A `MAPPING_CORRECTION` re-books
//! caller-supplied `corrected_lines`; if those reuse the original's refs the
//! guard passes. So on the real flows the guard never fires — reuse is
//! structural. It exists to reject a hand-crafted / buggy correction that
//! invents a pinned ref the original never had (the integration test's crafted
//! case), and as the seam Slices 3/4 extend.
//!
//! Stateless — every method runs inside the caller's posting transaction
//! (`txn`); tenant isolation runs through the `SecureORM` layer
//! (`.secure().scope_with(scope)`), mirroring
//! [`crate::infra::posting::chain::ChainSealer`].

use std::collections::HashSet;

use sea_orm::{ColumnTrait, Condition, EntityTrait};
use toolkit_db::DbError;
use toolkit_db::secure::{AccessScope, DbTx, SecureEntityExt};

use crate::domain::error::DomainError;
use crate::domain::model::NewLine;
use crate::infra::posting::service::{business, infra};
use crate::infra::storage::entity::journal_line;

/// The pinned evidence-ref tuple of one journal line:
/// `(pricing_snapshot_ref, po_allocation_group, price_id, sku_or_plan_ref,
/// invoice_item_ref)`. A line with no pinned evidence has an all-`None` tuple.
type EvidenceTuple = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Pure set-check: every non-all-NULL evidence tuple on `correction` MUST also
/// appear in `original`. Returns the FIRST correction tuple absent from the
/// original (the violation), or `None` when every correction tuple is reused.
///
/// All-NULL tuples are skipped on both sides — a line with no pinned evidence
/// carries no policy-version evidence to reuse or violate. Factored out of
/// [`PolicyVersionGuard::check`] so the set logic is unit-testable without a DB.
#[must_use]
pub(crate) fn first_unreused_tuple(
    original: &[EvidenceTuple],
    correction: &[EvidenceTuple],
) -> Option<EvidenceTuple> {
    let original_set: HashSet<&EvidenceTuple> =
        original.iter().filter(|t| !is_all_null(t)).collect();
    correction
        .iter()
        .find(|t| !is_all_null(t) && !original_set.contains(*t))
        .cloned()
}

/// `true` when a tuple carries no pinned evidence (all five refs are `None`).
fn is_all_null(t: &EvidenceTuple) -> bool {
    t.0.is_none() && t.1.is_none() && t.2.is_none() && t.3.is_none() && t.4.is_none()
}

/// Project one [`NewLine`] to its pinned evidence tuple.
fn line_tuple(l: &NewLine) -> EvidenceTuple {
    (
        l.pricing_snapshot_ref.clone(),
        l.po_allocation_group.clone(),
        l.price_id.clone(),
        l.sku_or_plan_ref.clone(),
        l.invoice_item_ref.clone(),
    )
}

/// Project one read-back `journal_line` row to its pinned evidence tuple.
fn row_tuple(r: &journal_line::Model) -> EvidenceTuple {
    (
        r.pricing_snapshot_ref.clone(),
        r.po_allocation_group.clone(),
        r.price_id.clone(),
        r.sku_or_plan_ref.clone(),
        r.invoice_item_ref.clone(),
    )
}

/// Posting guard that enforces evidence-ref reuse on a correction (§4.6, AC #15).
/// Runs as a read-only validation step in
/// [`crate::infra::posting::service::PostingService`] AFTER the fiscal-period
/// gate and BEFORE the append-only insert — it only READS the original's lines,
/// so it takes no write lock.
///
/// Stateless (mirrors [`crate::infra::posting::chain::ChainSealer`] /
/// [`crate::infra::posting::freeze::TamperFreezeGuard`]).
#[derive(Clone, Default)]
pub struct PolicyVersionGuard;

impl PolicyVersionGuard {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Enforce evidence-ref reuse for `entry`/`lines` inside `txn`.
    ///
    /// - `entry.reverses_entry_id == None` ⇒ a fresh original posting; nothing to
    ///   reuse, returns `Ok(())`.
    /// - Otherwise (a correction): read the ORIGINAL entry's lines by
    ///   `(tenant_id, reverses_period_id, reverses_entry_id)` under `scope`, then
    ///   require every non-all-NULL evidence tuple on `lines` to appear among the
    ///   original's tuples. A correction tuple absent from the original ⇒
    ///   [`DomainError::PolicyVersionViolation`].
    ///
    /// # Errors
    /// A sentinel [`DbError`] carrying [`DomainError::PolicyVersionViolation`]
    /// when the correction carries a pinned evidence ref the original never had;
    /// an infrastructure [`DbError`] on a storage / scope failure, or when the
    /// referenced original has no lines (an invariant breach — a correction must
    /// point at a real prior posting).
    pub async fn check(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        entry: &crate::domain::model::NewEntry,
        lines: &[NewLine],
    ) -> Result<(), DbError> {
        // A fresh original posting reuses nothing — fast exit.
        let Some(original_entry_id) = entry.reverses_entry_id else {
            return Ok(());
        };
        let tenant = entry.tenant_id;

        // The original's lines are keyed by the entry's `reverses_*` pointers.
        // `reverses_period_id` pins the original's period; combined with
        // `reverses_entry_id` + tenant it selects exactly the original's lines.
        let mut cond = Condition::all()
            .add(journal_line::Column::TenantId.eq(tenant))
            .add(journal_line::Column::EntryId.eq(original_entry_id));
        if let Some(original_period) = entry.reverses_period_id.clone() {
            cond = cond.add(journal_line::Column::PeriodId.eq(original_period));
        }

        let original_rows = journal_line::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(cond)
            .all(txn)
            .await
            .map_err(|e| infra(format!("policy-version read original lines: {e}")))?;

        if original_rows.is_empty() {
            // A correction must point at a real prior posting; no lines means a
            // dangling `reverses_entry_id` — an invariant breach, not a client
            // rejection.
            return Err(infra(format!(
                "policy-version: original entry {original_entry_id} (tenant {tenant}) has no lines"
            )));
        }

        let original_tuples: Vec<EvidenceTuple> = original_rows.iter().map(row_tuple).collect();
        let correction_tuples: Vec<EvidenceTuple> = lines.iter().map(line_tuple).collect();

        if let Some(bad) = first_unreused_tuple(&original_tuples, &correction_tuples) {
            return Err(business(DomainError::PolicyVersionViolation(format!(
                "correction reuses no original evidence ref for tuple \
                 (pricing_snapshot_ref={:?}, po_allocation_group={:?}, price_id={:?}, \
                 sku_or_plan_ref={:?}, invoice_item_ref={:?}); a correction must reuse the \
                 original posting's pinned evidence",
                bad.0, bad.1, bad.2, bad.3, bad.4
            ))));
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "policy_version_tests.rs"]
mod tests;
