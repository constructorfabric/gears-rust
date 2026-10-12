//! Registry-backed currency scale resolution errors.

use toolkit_macros::domain_model;

/// Currency-scale resolution failure.
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum ScaleError {
    /// Underlying repository/scope failure.
    #[error("scale resolve repo error: {0}")]
    Repo(#[from] crate::domain::model::RepoError),
    /// Non-ISO currency with no registry row (no implicit scale).
    #[error("no scale for currency: {0}")]
    UnknownCurrencyScale(String),
    /// A registry row exists but its stored `currency_scale` is out of the valid
    /// scale range (outside 0..=28) — distinct from "no row":
    /// the row must be repaired, not added.
    #[error("corrupt stored scale for currency {currency}: currency_scale={currency_scale}")]
    CorruptStoredScale {
        currency: String,
        currency_scale: i16,
    },
}

/// Integer minor units of a validated posted amount, for the parked `v1`
/// event payloads that still carry `*_minor` fields. Exact by construction:
/// a [`PostedMoney`] amount is a multiple of `10^-scale`, so scaling it by
/// `10^scale` leaves no fraction. `None` when the value does not fit `i64`,
/// which a valid posting can reach (scale up to 28: at scale 18 anything above
/// about 9.22 units). Emitters use `infra::v1_payload::v1_minor_units_for`,
/// which applies the one saturating policy instead of failing and logs the
/// field and business identity it saturated.
#[must_use]
pub fn minor_units(money: &bss_ledger_sdk::PostedMoney) -> Option<i64> {
    let amount = money.amount().normalize();
    let scale = u32::from(money.currency().scale());
    // `PostedMoney::try_new` guarantees `amount.scale() <= scale`.
    let shift = scale.checked_sub(amount.scale())?;
    let factor = 10_i128.checked_pow(shift)?;
    let minor = amount.mantissa().checked_mul(factor)?;
    i64::try_from(minor).ok()
}
