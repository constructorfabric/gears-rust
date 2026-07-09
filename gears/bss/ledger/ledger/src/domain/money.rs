//! Money-side domain logic for the gear: the integer minor-unit `Money` value
//! type, the registration headroom check, and the scale-resolution error type.
//! Banker's rounding and proportional allocation live in the sibling `domain`
//! modules `money_math` and `allocate`; the registry-backed resolver lives in
//! `infra::currency_scale` (it depends on a repo).

use serde::{Deserialize, Serialize};
use toolkit_macros::domain_model;

/// A monetary amount in integer minor units at a per-currency scale (decision
/// C — never float, never Decimal).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    pub amount_minor: i64,
    pub currency: String,
    pub scale: u8,
}

impl Money {
    #[must_use]
    pub fn new(amount_minor: i64, currency: impl Into<String>, scale: u8) -> Self {
        Self {
            amount_minor,
            currency: currency.into(),
            scale,
        }
    }
}

/// Default plausible maximum in MAJOR units when a currency does not declare
/// its own (`NUMERIC(38,0)` is deferred). At this bound the maximum acceptable
/// scale is 6; a higher-precision currency (e.g. 8-decimal crypto) registers a
/// smaller per-currency max so its scale still fits the headroom.
pub const PLAUSIBLE_MAX_MAJOR_UNITS: i128 = 1_000_000_000_000; // 10^12

/// `i64` form of [`PLAUSIBLE_MAX_MAJOR_UNITS`] — the per-currency default the
/// gear resolves when `ProvisionCurrencyScale::plausible_max_major` is `None`.
pub const DEFAULT_PLAUSIBLE_MAX_MAJOR: i64 = 1_000_000_000_000; // 10^12

/// True if a currency with `minor_units` scale keeps its per-currency
/// `plausible_max_major` within `i64` minor-unit headroom — i.e.
/// `plausible_max_major * 10^minor_units <= i64::MAX` (A4/I-10). With the
/// default max (10^12) the cutoff is scale 6; a smaller max admits a larger
/// scale.
#[must_use]
pub fn scale_fits_headroom(minor_units: i16, plausible_max_major: i64) -> bool {
    let Ok(exp) = u32::try_from(minor_units) else {
        return false; // negative scale is invalid
    };
    let Some(factor) = 10_i128.checked_pow(exp) else {
        return false;
    };
    i128::from(plausible_max_major)
        .checked_mul(factor)
        .is_some_and(|m| m <= i128::from(i64::MAX))
}

/// Currency-scale resolution failure.
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum ScaleError {
    /// Underlying repository/scope failure.
    #[error("scale resolve repo error: {0}")]
    Repo(String),
    /// Non-ISO currency with no registry row (no implicit scale).
    #[error("no scale for currency: {0}")]
    UnknownCurrencyScale(String),
    /// A registry row exists but its stored `minor_units` is out of the valid
    /// scale range (negative, or larger than a `u8`) — distinct from "no row":
    /// the row must be repaired, not added.
    #[error("corrupt stored scale for currency {currency}: minor_units={minor_units}")]
    CorruptStoredScale { currency: String, minor_units: i16 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_round_trips_through_json() {
        let m = Money::new(12345, "USD", 2);
        let json = serde_json::to_string(&m).unwrap();
        let back: Money = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn headroom_cutoff_is_scale_six_at_default_max() {
        let max = DEFAULT_PLAUSIBLE_MAX_MAJOR;
        assert!(scale_fits_headroom(0, max));
        assert!(scale_fits_headroom(2, max));
        assert!(scale_fits_headroom(6, max));
        // 10^12 * 10^7 = 10^19 > i64::MAX (~9.22e18)
        assert!(!scale_fits_headroom(7, max));
        assert!(!scale_fits_headroom(20, max));
        assert!(!scale_fits_headroom(-1, max));
    }

    #[test]
    fn smaller_max_admits_higher_scale() {
        // BTC: 21_000_000 major units at scale 8 -> 2.1e15 <= i64::MAX.
        assert!(scale_fits_headroom(8, 21_000_000));
        // The same scale 8 overflows under the default 10^12 max.
        assert!(!scale_fits_headroom(8, DEFAULT_PLAUSIBLE_MAX_MAJOR));
    }
}
