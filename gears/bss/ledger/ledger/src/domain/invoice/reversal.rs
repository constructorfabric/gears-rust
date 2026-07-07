//! Strict line-negation reversal + the `MAPPING_CORRECTION` flow
//! (architecture §5.3–5.4).
//!
//! A reversal posts the **same accounts** as the original with each line's
//! **side flipped** and its **amount kept positive** (the foundation's amount
//! guard rejects negatives, so a reversal is a positive entry on the opposite
//! side, not a negative one). `source_doc_type = REVERSAL`, the `reverses_*`
//! header points back at the original, and `source_business_id = "reverses=<id>"`
//! keys the `REVERSAL` idempotency. A reversal of a reversal is rejected
//! ([`ReversalError::CannotReverseReversal`]) — you correct by re-posting, not
//! by stacking reversals.
//!
//! A `MAPPING_CORRECTION` is a reversal of the mis-mapped original immediately
//! followed by a corrected re-post; its idempotency business id is
//! `"<invoice_id>:<correction_id>"`, where `correction_id` is a stable hash of
//! `(original_entry_id, reversal_entry_id)` so the same correction always keys
//! identically (retries replay, never double-post).
//!
//! `correction_id` reuses the foundation's hashing primitive — the FIPS-
//! validated `aws-lc-rs` SHA-256 that already fingerprints the idempotency
//! payload — rather than adding a `sha2` dependency.

use aws_lc_rs::digest::{SHA256, digest as sha256};
use bss_ledger_sdk::{AccountClass, EntryView, PostEntry, PostLine, Side, SourceDocType};
use toolkit_macros::domain_model;
use uuid::Uuid;

/// Why a reversal could not be built.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReversalError {
    /// The supplied original is itself a `REVERSAL` — reversing a reversal is
    /// forbidden (correct forward by re-posting, never stack reversals).
    #[error("cannot reverse an entry that is itself a reversal")]
    CannotReverseReversal,
    /// The original carries a `REUSABLE_CREDIT` line whose
    /// `credit_grant_event_type` the read-back `LineView` does not expose, so a
    /// faithful reversal cannot be reconstructed (it would violate the DB
    /// `chk_journal_line_credit_grant`). Slice 1 never emits this class; the
    /// payments slice that does will carry the field on `LineView`. Fail fast
    /// here rather than abort opaquely at the DB CHECK.
    #[error(
        "cannot reverse an entry with a REUSABLE_CREDIT line (credit-grant dim not reconstructible)"
    )]
    CreditGrantNotReconstructible,
}

/// The `source_business_id` of a reversal: `"reverses=<original_entry_id>"`.
/// Keys the `(tenant, REVERSAL, business_id)` idempotency so a re-issued
/// reversal of the same entry replays.
#[must_use]
pub fn reversal_business_id(original_entry_id: Uuid) -> String {
    format!("reverses={original_entry_id}")
}

/// Build the reversing entry for `original`: same accounts, every line's side
/// flipped, amounts unchanged (positive), header pointing back at the original.
///
/// The reversal posts into `into_period_id` (a reversal may land in a later,
/// still-OPEN period than the original) with `effective_at = effective_on`.
///
/// # Errors
/// [`ReversalError::CannotReverseReversal`] when `original.source_doc_type` is
/// already [`SourceDocType::Reversal`]; [`ReversalError::CreditGrantNotReconstructible`]
/// when the original carries a `REUSABLE_CREDIT` line.
pub fn build_reversal(
    original: &EntryView,
    into_period_id: String,
    effective_on: chrono::NaiveDate,
    posted_by_actor_id: Uuid,
    correlation_id: Uuid,
) -> Result<PostEntry, ReversalError> {
    if original.source_doc_type == SourceDocType::Reversal {
        return Err(ReversalError::CannotReverseReversal);
    }
    // A `REUSABLE_CREDIT` line requires `credit_grant_event_type`
    // (`chk_journal_line_credit_grant`), which the read-back `LineView` does not
    // expose — `flip_line` cannot reconstruct it. Reject before posting rather
    // than abort opaquely at the DB CHECK. (Slice 1 never emits this class.)
    if original
        .lines
        .iter()
        .any(|l| l.account_class == AccountClass::ReusableCredit)
    {
        return Err(ReversalError::CreditGrantNotReconstructible);
    }
    let entry_id = Uuid::now_v7();
    let lines = original.lines.iter().map(flip_line).collect();

    Ok(PostEntry {
        entry_id,
        tenant_id: original.tenant_id,
        period_id: into_period_id,
        entry_currency: original.entry_currency.clone(),
        source_doc_type: SourceDocType::Reversal,
        source_business_id: reversal_business_id(original.entry_id),
        effective_at: effective_on,
        posted_by_actor_id,
        correlation_id,
        reverses_entry_id: Some(original.entry_id),
        reverses_period_id: Some(original.period_id.clone()),
        lines,
    })
}

/// Build the corrected re-post half of a `MAPPING_CORRECTION`: the
/// caller-supplied `corrected_lines` (already re-mapped to the right accounts)
/// posted under `source_doc_type = MAPPING_CORRECTION`, keyed
/// `"<invoice_id>:<correction_id>"`, with the header still pointing back at the
/// original (the reversal cleared it; this re-books it correctly).
///
/// `correction_id` is [`correction_id`]`(original.entry_id, reversal_entry_id)`.
#[must_use]
#[allow(clippy::too_many_arguments)] // each is a distinct, non-confusable field
pub fn build_mapping_correction(
    original: &EntryView,
    reversal_entry_id: Uuid,
    invoice_id: &str,
    into_period_id: String,
    effective_on: chrono::NaiveDate,
    posted_by_actor_id: Uuid,
    correlation_id: Uuid,
    corrected_lines: Vec<PostLine>,
) -> PostEntry {
    let correction = correction_id(original.entry_id, reversal_entry_id);
    PostEntry {
        entry_id: Uuid::now_v7(),
        tenant_id: original.tenant_id,
        period_id: into_period_id,
        entry_currency: original.entry_currency.clone(),
        source_doc_type: SourceDocType::MappingCorrection,
        source_business_id: format!("{invoice_id}:{correction}"),
        effective_at: effective_on,
        posted_by_actor_id,
        correlation_id,
        // The correction re-books the original invoice; it points back at the
        // entry it corrects (the reversal that cleared the bad mapping).
        reverses_entry_id: Some(reversal_entry_id),
        reverses_period_id: Some(original.period_id.clone()),
        lines: corrected_lines,
    }
}

/// Stable correction id for a `(original, reversal)` entry pair: the hex SHA-256
/// of `"<original_entry_id>:<reversal_entry_id>"`. Deterministic (the same pair
/// always yields the same id — retries replay) and order-sensitive (swapping the
/// two ids yields a different id). Uses the foundation's FIPS SHA-256.
#[must_use]
pub fn correction_id(original_entry_id: Uuid, reversal_entry_id: Uuid) -> String {
    let canon = format!("{original_entry_id}:{reversal_entry_id}");
    let digest = sha256(&SHA256, canon.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest.as_ref() {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Negate one read-back line into its reversing [`PostLine`]: flip the side,
/// keep the (positive) amount, preserve the account + invoice/tax dims so the
/// reversal lands on exactly the grains the original moved. The read DTO does
/// not carry seller/resource/source-ref columns, so those are left `None` on the
/// reversal (it nets the original; lineage lives on the original line).
fn flip_line(line: &bss_ledger_sdk::LineView) -> PostLine {
    PostLine {
        line_id: Uuid::now_v7(),
        payer_tenant_id: line.payer_tenant_id,
        seller_tenant_id: None,
        resource_tenant_id: None,
        account_id: line.account_id,
        account_class: line.account_class,
        gl_code: line.gl_code.clone(),
        side: flip(line.side),
        amount_minor: line.amount_minor,
        currency: line.currency.clone(),
        invoice_id: line.invoice_id.clone(),
        due_date: line.due_date,
        revenue_stream: line.revenue_stream.clone(),
        mapping_status: line.mapping_status,
        // Reverse at the ORIGINAL locked rate: copy the original line's functional
        // translation (kept positive, like `amount_minor`); the flipped DR/CR side
        // makes the functional delta net the original to zero. NO re-lock (spec
        // §4.2 F-8c) — a reversal must not synthesize FX gain/loss. Cross-currency
        // lines carry a functional value; single-currency lines carry `None`.
        functional_amount_minor: line.functional_amount_minor,
        functional_currency: line.functional_currency.clone(),
        tax_jurisdiction: line.tax_jurisdiction.clone(),
        tax_filing_period: line.tax_filing_period.clone(),
        tax_rate_ref: None,
        invoice_item_ref: None,
        sku_or_plan_ref: None,
        price_id: None,
        pricing_snapshot_ref: None,
        po_allocation_group: None,
        credit_grant_event_type: None,
        // Preserve the AR sub-class so a reversal nets the original disputed
        // delta on the same `ar_invoice_balance` sub-grain (`disputed_minor`).
        ar_status: line.ar_status.clone(),
    }
}

/// The opposite posting side.
const fn flip(side: Side) -> Side {
    match side {
        Side::Debit => Side::Credit,
        Side::Credit => Side::Debit,
    }
}

#[cfg(test)]
#[path = "reversal_tests.rs"]
mod tests;
