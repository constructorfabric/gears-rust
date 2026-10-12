//! Currency-qualified reconciliation evidence, separate from invoice counts:
//! the exact grain totals a tie-out folds and compares, and the X4 rounding
//! tolerance that decides whether a divergence blocks period close.
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use bss_ledger_sdk::{CurrencySpec, MoneyError, PostedMoney};
use rust_decimal::Decimal;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;
use crate::domain::exact_money::{
    ExactAmount, ExactError, ScaledSum, map_exact_error, map_money_error, subtract_posted,
};

/// `reconciliation_run.check_type` for the AR-ledger ↔ derived-projection tie-out (AC #7).
pub const CHECK_AR_DERIVED: &str = "AR_DERIVED";
/// `reconciliation_run.check_type` for the Payments ↔ PSP settlement tie.
pub const CHECK_PAYMENTS_PSP: &str = "PAYMENTS_PSP";
/// `reconciliation_run.check_type` for the upstream → ledger invoice-completeness check.
pub const CHECK_INVOICE_COMPLETENESS: &str = "INVOICE_COMPLETENESS";

/// A check result never adds unlike currencies or labels counts as money.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconciliationVariance {
    Money { by_currency: Vec<PostedMoney> },
    MissingInvoices { count: u64 },
}

impl ReconciliationVariance {
    /// Whether every recorded currency (or the diagnostic count) is zero.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        match self {
            Self::Money { by_currency } => by_currency.iter().all(|m| m.amount().is_zero()),
            Self::MissingInvoices { count } => *count == 0,
        }
    }
}

/// An exact signed grain total in major units, with the currency metadata it
/// was folded or read under (as stored, so a corrupt row can still be named).
///
/// A total is either exact or untrusted. An untrusted total (a corrupt stored
/// amount, a currency or scale disagreement, an exact-arithmetic budget breach)
/// has no amount to read, never ties out and always surfaces as a variance.
///
/// `==` is value identity: two exact totals are equal when their metadata and
/// exact values are equal, and two untrusted totals with the same metadata are
/// the same placeholder. Whether two totals *tie out* is the reconciliation
/// verdict [`GrainAmount::differs_from`], where an untrusted side never does.
#[domain_model]
#[derive(Clone, Debug)]
pub struct GrainAmount {
    currency: String,
    currency_scale: u8,
    total: GrainTotal,
}

/// Whether a grain total can be relied on.
#[domain_model]
#[derive(Clone, Debug)]
enum GrainTotal {
    /// An exact total. Every term of a grain has the grain's currency and scale
    /// (a mismatch makes the total untrusted first), so the shared [`ScaledSum`]
    /// adds integer coefficients at that scale, one checked `i128` add per line,
    /// and moves to an exact fraction only on overflow.
    Exact(ScaledSum),
    /// No value can be relied on; there is deliberately no placeholder amount.
    Untrusted,
}

/// Why a grain total has no bounded posting form.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GrainError {
    /// The total cannot be relied on.
    #[error("untrusted grain total for {0}")]
    Untrusted(String),
    /// The stored currency metadata is not a valid currency spec.
    #[error("grain currency metadata: {0}")]
    Metadata(MoneyError),
    /// The exact total is outside the bounded posting contract.
    #[error("grain total out of range: {0}")]
    OutOfRange(ExactError),
}

impl PartialEq for GrainAmount {
    fn eq(&self, other: &Self) -> bool {
        let Self {
            currency,
            currency_scale,
            total,
        } = self;
        let Self {
            currency: other_currency,
            currency_scale: other_currency_scale,
            total: other_total,
        } = other;
        currency == other_currency
            && currency_scale == other_currency_scale
            && match (total, other_total) {
                (GrainTotal::Exact(left), GrainTotal::Exact(right)) => left == right,
                (GrainTotal::Untrusted, GrainTotal::Untrusted) => true,
                _ => false,
            }
    }
}

impl Eq for GrainAmount {}

impl GrainAmount {
    /// An exact zero under the given currency metadata.
    #[must_use]
    pub fn zero(currency: &str, currency_scale: u8) -> Self {
        Self {
            currency: currency.to_owned(),
            currency_scale,
            total: GrainTotal::Exact(ScaledSum::zero(currency_scale)),
        }
    }

    /// The exact value of one validated posting.
    #[must_use]
    pub fn from_posted(money: &PostedMoney) -> Self {
        let mut grain = Self::zero(money.currency().code(), money.currency().scale());
        grain.add_posted(money);
        grain
    }

    /// An untrusted total for a stored amount that failed to decode.
    #[must_use]
    pub fn untrusted(currency: &str, currency_scale: u8) -> Self {
        Self {
            currency: currency.to_owned(),
            currency_scale,
            total: GrainTotal::Untrusted,
        }
    }

    /// An exact total from an exact value (test fixtures beyond the posting contract).
    #[cfg(test)]
    pub(crate) fn from_exact(value: ExactAmount, currency: &str, currency_scale: u8) -> Self {
        Self {
            currency: currency.to_owned(),
            currency_scale,
            total: GrainTotal::Exact(ScaledSum::from_exact(value, currency_scale)),
        }
    }

    /// Mark the total untrusted: a term could not be read or added exactly.
    pub fn mark_untrusted(&mut self) {
        self.total = GrainTotal::Untrusted;
    }

    /// `true` when the total cannot be relied on.
    #[must_use]
    pub fn is_untrusted(&self) -> bool {
        matches!(self.total, GrainTotal::Untrusted)
    }

    /// Currency code of the grain, as stored.
    #[must_use]
    pub fn currency(&self) -> &str {
        &self.currency
    }

    /// Stored posting scale of the grain.
    #[must_use]
    pub fn currency_scale(&self) -> u8 {
        self.currency_scale
    }

    /// Both metadata dimensions, code and stored scale, agree.
    fn same_metadata(&self, currency: &str, currency_scale: u8) -> bool {
        self.currency == currency && self.currency_scale == currency_scale
    }

    /// Add a validated posting exactly. A currency disagreement, a scale
    /// disagreement or an exact-arithmetic budget breach marks the total
    /// untrusted rather than silently mixing or truncating.
    pub fn add_posted(&mut self, delta: &PostedMoney) {
        if !self.same_metadata(delta.currency().code(), delta.currency().scale()) {
            self.mark_untrusted();
            return;
        }
        let GrainTotal::Exact(sum) = &mut self.total else {
            return;
        };
        if sum.add_decimal(delta.amount()).is_err() {
            self.mark_untrusted();
        }
    }

    /// Add another exact total of the same grain (baseline + open fold).
    pub fn add_grain(&mut self, other: &Self) {
        if !self.same_metadata(&other.currency, other.currency_scale) {
            self.mark_untrusted();
            return;
        }
        let GrainTotal::Exact(theirs) = &other.total else {
            self.mark_untrusted();
            return;
        };
        let GrainTotal::Exact(ours) = &mut self.total else {
            return;
        };
        if ours.add_sum(theirs).is_err() {
            self.mark_untrusted();
        }
    }

    /// The exact value, or `None` for an untrusted total (it has no amount).
    #[must_use]
    pub fn exact(&self) -> Option<ExactAmount> {
        match &self.total {
            GrainTotal::Exact(sum) => Some(sum.to_exact()),
            GrainTotal::Untrusted => None,
        }
    }

    /// `true` when this total disagrees with `other` or either side is untrusted.
    #[must_use]
    pub fn differs_from(&self, other: &Self) -> bool {
        match (&self.total, &other.total) {
            (GrainTotal::Exact(left), GrainTotal::Exact(right)) => {
                !self.same_metadata(&other.currency, other.currency_scale) || left != right
            }
            _ => true,
        }
    }

    /// `true` when the total is exactly zero and trusted.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        matches!(&self.total, GrainTotal::Exact(sum) if sum.is_zero())
    }

    /// Canonical decimal text at the grain's scale (diagnostics only).
    #[must_use]
    pub fn text(&self) -> String {
        if self.is_untrusted() {
            return "<untrusted>".to_owned();
        }
        self.exact()
            .and_then(|value| value.canonical_at_scale(self.currency_scale).ok())
            .unwrap_or_else(|| "<unrenderable>".to_owned())
    }

    /// The bounded posting this total denotes, or the documented range error.
    ///
    /// # Errors
    /// An untrusted total, invalid metadata or an out-of-contract magnitude.
    pub fn to_posted(&self) -> Result<PostedMoney, GrainError> {
        let GrainTotal::Exact(sum) = &self.total else {
            return Err(GrainError::Untrusted(self.currency.clone()));
        };
        let spec = CurrencySpec::try_new(self.currency.clone(), self.currency_scale)
            .map_err(GrainError::Metadata)?;
        sum.to_posted(spec).map_err(GrainError::OutOfRange)
    }
}

/// A reconciliation verdict: the per-currency variance to record and whether
/// it is within the rounding tolerance (an out-of-tolerance run blocks close).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToleranceDecision {
    /// The variance to record, one bucket per currency.
    pub variance: ReconciliationVariance,
    /// `true` when the run does not block close.
    pub within_tolerance: bool,
}

/// The X4 rounding budget for one currency: `increments` posting increments
/// (`10^-scale`) per 1,000 items, floored at `increments` so a sub-1,000-item
/// period can still absorb the immaterial-rounding bucket the design grants
/// (statutory floors override; a per-jurisdiction registry remains future).
/// Exact: small integers scaled by a power of ten, no division.
#[must_use]
pub fn tolerance_budget(increments: u32, items: u64, spec: &CurrencySpec) -> Decimal {
    Decimal::from(increments)
        * Decimal::from((items / 1000).max(1))
        * Decimal::new(1, u32::from(spec.scale()))
}

/// `|a − b|` exactly in the shared currency and stored scale; a currency or
/// scale disagreement is the named mismatch error, never an implicit conversion.
fn absolute_difference(a: &PostedMoney, b: &PostedMoney) -> Result<PostedMoney, DomainError> {
    let diff = subtract_posted(a, b)?;
    if diff.amount().is_sign_negative() {
        PostedMoney::try_new(-diff.amount(), diff.currency().clone()).map_err(map_money_error)
    } else {
        Ok(diff)
    }
}

/// The X4 decision for an AR↔derived tie-out.
///
/// `divergences` are the `(computed, cached)` totals of every cache divergence;
/// their **absolute** differences are summed exactly **per currency**, a
/// currency never added to another. The run is within tolerance only when there
/// is no hard defect AND every currency bucket fits its budget of
/// `increments_per_k` posting increments per 1,000 of `items` posted lines (see
/// [`tolerance_budget`]). Hard defects are never rounding: a structural defect
/// the caller found (an imbalanced entry, a negative guarded grain, a PENDING
/// mapping line), an untrusted total, or totals whose currency metadata
/// disagree (including one currency at two scales; the first bucket is kept).
///
/// # Errors
/// The documented range error when a currency's exact total does not fit the
/// bounded decimal contract, or a money-metadata error.
pub fn ar_tolerance_decision<'a>(
    divergences: impl IntoIterator<Item = (&'a GrainAmount, &'a GrainAmount)>,
    structural_defect: bool,
    items: u64,
    increments_per_k: u32,
) -> Result<ToleranceDecision, DomainError> {
    // Deterministic currency order; one bucket per currency with its stored scale.
    let mut buckets: BTreeMap<&str, (u8, ExactAmount)> = BTreeMap::new();
    let mut hard_defect = structural_defect;
    let zero = ExactAmount::from_decimal(Decimal::ZERO);
    for (computed, cached) in divergences {
        let (Some(computed_value), Some(cached_value)) = (computed.exact(), cached.exact()) else {
            hard_defect = true;
            continue;
        };
        if !computed.same_metadata(&cached.currency, cached.currency_scale) {
            hard_defect = true;
            continue;
        }
        let diff = computed_value
            .checked_sub(&cached_value)
            .map_err(map_exact_error)?;
        let magnitude = if diff.is_negative() {
            zero.checked_sub(&diff).map_err(map_exact_error)?
        } else {
            diff
        };
        match buckets.entry(computed.currency()) {
            Entry::Vacant(slot) => {
                slot.insert((computed.currency_scale, magnitude));
            }
            Entry::Occupied(mut slot) => {
                let (scale, sum) = slot.get_mut();
                if *scale != computed.currency_scale {
                    hard_defect = true;
                    continue;
                }
                *sum = sum.checked_add(&magnitude).map_err(map_exact_error)?;
            }
        }
    }
    let mut by_currency = Vec::with_capacity(buckets.len());
    let mut all_within = true;
    for (code, (scale, sum)) in buckets {
        let spec = CurrencySpec::try_new(code.to_owned(), scale).map_err(map_money_error)?;
        let budget = tolerance_budget(increments_per_k, items, &spec);
        let total = sum.into_posted_exact(spec).map_err(map_exact_error)?;
        if total.amount() > budget {
            all_within = false;
        }
        by_currency.push(total);
    }
    Ok(ToleranceDecision {
        variance: ReconciliationVariance::Money { by_currency },
        within_tolerance: !hard_defect && all_within,
    })
}

/// The X4 decision for the Payments↔PSP tie: the variance is the absolute
/// difference of the ledger and PSP settled totals (one currency bucket), and
/// it is within tolerance when that fits the budget of `increments_per_k`
/// posting increments per 1,000 `settlements` (see [`tolerance_budget`]).
///
/// # Errors
/// A currency or scale disagreement between the two totals.
pub fn psp_tolerance_decision(
    ledger_settled: &PostedMoney,
    psp_settled: &PostedMoney,
    settlements: u64,
    increments_per_k: u32,
) -> Result<ToleranceDecision, DomainError> {
    let bucket = absolute_difference(ledger_settled, psp_settled)?;
    let budget = tolerance_budget(increments_per_k, settlements, psp_settled.currency());
    let within_tolerance = bucket.amount() <= budget;
    Ok(ToleranceDecision {
        variance: ReconciliationVariance::Money {
            by_currency: vec![bucket],
        },
        within_tolerance,
    })
}

#[cfg(test)]
#[path = "reconciliation_tests.rs"]
mod reconciliation_tests;
