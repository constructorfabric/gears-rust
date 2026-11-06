//! What one `(tenant, GTS type)` scope holds, for reconciliation.
//!
//! Reconciliation is the fourth read path, and an operator surface: it reports
//! ingestion activity and two watermarks so a consumer's gap can be detected
//! without folding the meter.

use core::num::NonZeroU64;

use bigdecimal::BigDecimal;
use time::OffsetDateTime;

use crate::v2::aggregation::AggregationFold;
use crate::v2::quantity::UsageQuantity;

/// The fold-appropriate quantity summary, in one of two branches.
///
/// Which branch a scope reports is decided by the **declared fold** the SPI
/// call carries, never by the caller and never by the plugin: a summing meter
/// reports an accrued sum, and every other meter reports how many
/// observations the range selected together with the latest of them
/// (`usage-collector-v2.yaml`'s `ReconciliationMetadata.quantity_summary`,
/// this gear's `dod-reconciliation-quantity-summary`, and the `TimescaleDB`
/// plugin's `DESIGN.md` §3.6 `cpt-cf-uc-plugin-seq-reconciliation`).
///
/// **Quantities under a non-accruing fold are not summable**, which is why
/// the second branch reports a count and one observation rather than a fold
/// result.
///
/// Not `#[non_exhaustive]`, deliberately: the branch set is closed by
/// [`Self::accrues`], which is total over the fold set, so there is no third
/// branch to evolve into. (The attribute would not forbid out-of-crate
/// construction — on an enum it bars exhaustive matching, not building a
/// variant — it would only cost every consumer a dead wildcard arm.)
///
/// Reach [`Self::Accrued`]'s `BigDecimal` through [`crate::bigdecimal`]
/// rather than a `bigdecimal` dependency of your own — see that re-export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuantitySummary {
    /// An accruing fold's total. Zero over an empty selection, never absent:
    /// an accrual over an empty set is defined.
    Accrued(BigDecimal),
    /// Every non-accruing fold. Absent when the range selected no entry,
    /// because an observation over an empty set is not defined.
    Observations(Option<ObservedQuantity>),
}

/// A non-empty [`QuantitySummary::Observations`]: the count and the latest
/// of the records a range selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedQuantity {
    /// Non-withdrawn records the requested range selected. Never zero: an
    /// empty selection is the outer `Option`'s `None`, not a zero here.
    pub count: NonZeroU64,
    /// The latest of the selected records by the `LATEST` total order of
    /// DESIGN §3.1 — greatest `window_end`, then greatest `accepted_at`,
    /// then greatest `id`.
    pub latest: UsageQuantity,
}

impl QuantitySummary {
    /// Whether `fold` takes the [`Self::Accrued`] branch.
    #[must_use]
    pub const fn accrues(fold: AggregationFold) -> bool {
        match fold {
            AggregationFold::Sum => true,
            AggregationFold::Count
            | AggregationFold::Max
            | AggregationFold::Min
            | AggregationFold::Latest => false,
        }
    }

    /// The summary for a selection holding nothing, under a known fold.
    #[must_use]
    pub fn empty_for(fold: AggregationFold) -> Self {
        if Self::accrues(fold) {
            Self::Accrued(BigDecimal::from(0))
        } else {
            Self::Observations(None)
        }
    }
}

/// Accepted count, a fold-appropriate summary, and two watermarks, for one
/// `(tenant, GTS type)` scope.
///
/// The first two figures answer different questions and deliberately
/// disagree. `accepted_count` counts every accepted entry the range selects,
/// invalidations included, because it reports **ingestion activity**.
/// `quantity_summary` excludes both halves of every withdrawn pair, because
/// it reports the **meter**.
///
/// Both watermarks are the scope's `max(accepted_at)` and `max(window_end)`,
/// **unbounded by the request's range**, and absent when the scope holds no
/// entries. A range selecting nothing therefore leaves them populated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconciliationMetadata {
    /// Every accepted entry the range selects, invalidations included.
    pub accepted_count: u64,
    /// The summary, in the branch the declared fold selects.
    pub quantity_summary: QuantitySummary,
    /// The scope's greatest acceptance instant, unbounded by the range.
    pub max_accepted_at: Option<OffsetDateTime>,
    /// The scope's greatest covered-period end, unbounded by the range.
    pub max_window_end: Option<OffsetDateTime>,
}

impl ReconciliationMetadata {
    /// The answer for a scope holding no entries at all, under a known fold.
    ///
    /// A tenant the compiled scope excludes answers exactly this, per the SPI
    /// contract — never an error.
    #[must_use]
    pub fn empty_for(fold: AggregationFold) -> Self {
        Self {
            accepted_count: 0,
            quantity_summary: QuantitySummary::empty_for(fold),
            max_accepted_at: None,
            max_window_end: None,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "reconciliation_tests.rs"]
mod reconciliation_tests;
