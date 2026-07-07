//! AR-aging bucket derivation (architecture §5.5). Folds the open per-invoice
//! AR balances into days-past-due buckets per `(payer, currency)`.
//!
//! Days past due = `today − due_date`; an invoice with no `due_date` is treated
//! as not-yet-due (`current`). The bucket boundaries are the tenant's configured
//! [`AgingThresholds`] (VHP-1853); the default `[30, 60, 90]` reproduces the
//! classic `current`, `1-30`, `31-60`, `61-90`, `90+`. Only rows with
//! `balance_minor > 0` age — a settled (`0`) or credit (`< 0`) row carries
//! nothing to chase. Pure `i64` summation; no rounding.

use std::collections::BTreeMap;

use bss_ledger_sdk::ArInvoiceBalanceView;
use chrono::NaiveDate;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::invoice::policy::AgingThresholds;

/// `current` bucket label — not yet due (≤ 0 days past due) or no due date. The
/// one fixed label; the past-due labels are derived from the tenant thresholds.
pub const BUCKET_CURRENT: &str = "current";

/// One aged grain: the outstanding AR for a `(payer, currency, bucket)`.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgingBucket {
    /// Payer whose receivable this is.
    pub payer_tenant_id: Uuid,
    /// Currency of the grain.
    pub currency: String,
    /// The bucket label, derived from the tenant's [`AgingThresholds`]:
    /// [`BUCKET_CURRENT`], then `"{lo}-{hi}"` per boundary, then `"{last}+"`
    /// (e.g. with `[30,60,90]`: `current` / `1-30` / `31-60` / `61-90` / `90+`).
    pub bucket: String,
    /// Summed outstanding minor units in this bucket (always `> 0` — empty
    /// grains are omitted).
    pub amount_minor: i64,
}

/// Bucket the open AR-invoice balances `rows` as of `today` under `thresholds`,
/// grouped per `(payer, currency)`. Rows with `balance_minor <= 0` are skipped.
/// The result is ordered by `(payer, currency, bucket-age)` for stable output.
#[must_use]
pub fn ar_aging(
    rows: &[ArInvoiceBalanceView],
    today: NaiveDate,
    thresholds: &AgingThresholds,
) -> Vec<AgingBucket> {
    let bounds = thresholds.bounds();
    let labels = bucket_labels(bounds);
    // (payer, currency, bucket-rank) -> summed outstanding (i128 while folding);
    // the numeric rank sorts buckets oldest-first within a payer/currency.
    let mut acc: BTreeMap<(Uuid, String, usize), i128> = BTreeMap::new();
    for row in rows {
        if row.balance_minor <= 0 {
            continue;
        }
        let rank = bucket_rank(days_past_due(row.due_date, today), bounds);
        *acc.entry((row.payer_tenant_id, row.currency.clone(), rank))
            .or_insert(0) += i128::from(row.balance_minor);
    }
    acc.into_iter()
        .map(|((payer_tenant_id, currency, rank), amount)| AgingBucket {
            payer_tenant_id,
            currency,
            bucket: labels[rank].clone(),
            amount_minor: i64::try_from(amount).unwrap_or(i64::MAX),
        })
        .collect()
}

/// Days past due: `today − due_date`, or `0` (not yet due) when there is no due
/// date. A future due date yields a negative count (→ `current`).
fn days_past_due(due_date: Option<NaiveDate>, today: NaiveDate) -> i64 {
    match due_date {
        Some(due) => (today - due).num_days(),
        None => 0,
    }
}

/// Map a days-past-due count to its bucket rank: `0` (current) for `≤ 0`, then
/// `i + 1` for the first boundary with `days <= bounds[i]`, else the open-ended
/// overflow rank `bounds.len() + 1`.
fn bucket_rank(days: i64, bounds: &[i64]) -> usize {
    if days <= 0 {
        return 0;
    }
    for (i, &b) in bounds.iter().enumerate() {
        if days <= b {
            return i + 1;
        }
    }
    bounds.len() + 1
}

/// Derive the labels for `bounds` (strictly increasing, all `> 0`, non-empty):
/// `["current", "1-{b0}", "{b0+1}-{b1}", …, "{last}+"]` — length `bounds.len() +
/// 2`, indexed by [`bucket_rank`].
fn bucket_labels(bounds: &[i64]) -> Vec<String> {
    let mut labels = Vec::with_capacity(bounds.len() + 2);
    labels.push(BUCKET_CURRENT.to_owned());
    let mut lower = 1_i64;
    for &b in bounds {
        labels.push(format!("{lower}-{b}"));
        lower = b + 1;
    }
    // The open-ended last bucket, labelled by the final boundary (e.g. "90+").
    labels.push(format!("{}+", bounds.last().copied().unwrap_or(0)));
    labels
}

#[cfg(test)]
#[path = "aging_tests.rs"]
mod tests;
