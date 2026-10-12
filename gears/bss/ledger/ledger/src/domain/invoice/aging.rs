//! AR-aging bucket derivation (architecture §5.5). Folds the open per-invoice
//! AR balances into days-past-due buckets per `(payer, currency)`.
//!
//! Days past due = `today − due_date`; an invoice with no `due_date` is treated
//! as not-yet-due (`current`). The bucket boundaries are the tenant's configured
//! [`AgingThresholds`] (VHP-1853); the default `[30, 60, 90]` reproduces the
//! classic `current`, `1-30`, `31-60`, `61-90`, `90+`. Only rows with
//! `balance > 0` age — a settled (`0`) or credit (`< 0`) row carries
//! nothing to chase. Exact summation with bounded final totals; no rounding.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use bss_ledger_sdk::ArInvoiceBalanceView;
use chrono::NaiveDate;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::exact_money::{ExactError, ScaledSum};
use crate::domain::invoice::policy::AgingThresholds;
use bss_ledger_sdk::money::{CurrencySpec, MoneyError, PostedMoney};
use rust_decimal::Decimal;

/// `current` bucket label — not yet due (≤ 0 days past due) or no due date. The
/// one fixed label; the past-due labels are derived from the tenant thresholds.
pub const BUCKET_CURRENT: &str = "current";

/// One aged grain: the outstanding AR for a `(payer, currency, bucket)`. The
/// grain's currency is the one `amount` carries.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgingBucket {
    /// Payer whose receivable this is.
    pub payer_tenant_id: Uuid,
    /// The bucket label, derived from the tenant's [`AgingThresholds`]:
    /// [`BUCKET_CURRENT`], then `"{lo}-{hi}"` per boundary, then `"{last}+"`
    /// (e.g. with `[30,60,90]`: `current` / `1-30` / `31-60` / `61-90` / `90+`).
    pub bucket: String,
    /// Summed outstanding major units in this bucket (always `> 0` — empty
    /// grains are omitted).
    pub amount: PostedMoney,
}

/// Bucket the open AR-invoice balances `rows` as of `today` under `thresholds`,
/// grouped per `(payer, currency)`. Rows with `balance <= 0` are skipped.
/// The result is ordered by `(payer, currency, bucket-age)` for stable output.
/// # Errors
/// Rejects conflicting stored scales (including skipped rows) or final overflow.
pub fn ar_aging(
    rows: &[ArInvoiceBalanceView],
    today: NaiveDate,
    thresholds: &AgingThresholds,
) -> Result<Vec<AgingBucket>, ExactError> {
    let bounds = thresholds.bounds();
    let labels = bucket_labels(bounds);
    // Each payer/currency grain has one stored scale across its age buckets.
    // Scale is validated metadata, never a balance-key axis. Validate every
    // row before filtering, so zero/credit rows cannot hide conflicting metadata.
    // Keys borrow the currency code from `rows`; each output bucket's money
    // carries its own copy of the spec.
    let mut specs: BTreeMap<(Uuid, &str), &CurrencySpec> = BTreeMap::new();
    for row in rows {
        let spec = row.balance.currency();
        match specs.entry((row.payer_tenant_id, spec.code())) {
            Entry::Occupied(stored) => {
                if stored.get().scale() != spec.scale() {
                    return Err(MoneyError::ScaleMismatch.into());
                }
            }
            Entry::Vacant(slot) => {
                slot.insert(spec);
            }
        }
    }
    // With one scale pinned per grain, the shared exact sum adds each balance as
    // an integer coefficient at that scale (one checked `i128` add per row) and
    // moves to an exact fraction only when a coefficient or the running sum
    // overflows `i128`; the bounded money contract is enforced once per output
    // bucket, so every total the old exact sum accepted is still accepted.
    let mut acc: BTreeMap<(Uuid, &str, usize), ScaledSum> = BTreeMap::new();
    for row in rows {
        if row.balance.amount() <= Decimal::ZERO {
            continue;
        }
        let rank = bucket_rank(days_past_due(row.due_date, today), bounds);
        let spec = row.balance.currency();
        acc.entry((row.payer_tenant_id, spec.code(), rank))
            .or_insert_with(|| ScaledSum::zero(spec.scale()))
            .add_decimal(row.balance.amount())?;
    }
    acc.into_iter()
        .map(|((payer_tenant_id, code, rank), sum)| {
            // Every accumulator was seeded by a row whose spec is recorded above.
            let spec = *specs
                .get(&(payer_tenant_id, code))
                .ok_or(MoneyError::CurrencyMismatch)?;
            Ok(AgingBucket {
                payer_tenant_id,
                bucket: labels[rank].clone(),
                amount: sum.to_posted(spec.clone())?,
            })
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

#[cfg(test)]
#[path = "aging_sum_tests.rs"]
mod sum_tests;
