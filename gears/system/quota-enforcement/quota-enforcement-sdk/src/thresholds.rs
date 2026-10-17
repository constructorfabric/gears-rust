//! Threshold crossing detection, shared by every counter mutation.
//!
//! The notifications feature owns the specification
//! (`algo-threshold-emission`); this is the arithmetic every caller of it
//! needs. Emission is upward-only and each threshold fires once per period: a
//! credit that drops the counter below a threshold does not re-arm it, because
//! the marker only moves up, and a period rollover resets the marker instead.
//!
//! The caller advances the marker and builds the event; keeping both out of
//! here lets the storage plugin do them inside its transaction.

use std::collections::BTreeSet;

use crate::models::{QuotaId, ThresholdCrossing};

/// Which of `thresholds` the move from `pre` to `post` crossed.
///
/// A threshold `t` is crossed when the counter moves from below `t` percent of
/// `cap` to at or above it. The comparison is exact: `pre * 100 < t * cap` and
/// `t * cap <= post * 100`, evaluated in `u128` so that neither the
/// multiplication nor a near-`u64::MAX` counter can overflow, and so that no
/// division introduces a rounding error. Landing exactly on a threshold counts
/// as crossing it; leaving from exactly on it does not.
///
/// Returns `None` when nothing was crossed: a downward or flat move, an
/// unbounded or zero cap, no configured thresholds, or every candidate already
/// at or below `marker`.
// @cpt-algo:cpt-cf-quota-enforcement-algo-threshold-emission:p1
#[must_use]
pub fn threshold_crossings(
    quota_id: QuotaId,
    pre: u64,
    post: u64,
    cap: Option<u64>,
    thresholds: &[u8],
    marker: Option<u8>,
) -> Option<ThresholdCrossing> {
    // @cpt-begin:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-denied-if
    // @cpt-begin:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-none
    let cap = cap?;
    if cap == 0 || post <= pre || thresholds.is_empty() {
        return None;
    }
    // @cpt-end:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-none
    // @cpt-end:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-denied-if
    let pre_scaled = u128::from(pre) * 100;
    let post_scaled = u128::from(post) * 100;
    let cap = u128::from(cap);
    // @cpt-begin:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-compute
    let crossed: Vec<u8> = thresholds
        .iter()
        .copied()
        .collect::<BTreeSet<u8>>()
        .into_iter()
        .filter(|threshold| marker.is_none_or(|seen| *threshold > seen))
        .filter(|threshold| {
            let level = u128::from(*threshold) * cap;
            pre_scaled < level && level <= post_scaled
        })
        .collect();
    // @cpt-end:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-compute
    // @cpt-begin:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-empty-if
    // @cpt-begin:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-skip
    let highest = crossed.last().copied()?;
    // @cpt-end:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-skip
    // @cpt-end:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-empty-if
    // @cpt-begin:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-return
    Some(ThresholdCrossing {
        quota_id,
        crossed_thresholds: crossed,
        highest_crossed_threshold: highest,
    })
    // @cpt-end:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-return
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "thresholds_tests.rs"]
mod tests;
