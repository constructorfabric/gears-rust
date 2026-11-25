//! The reference amount function: one price over one quantity (D-549).
//!
//! [`amount_for`] prices one [`PriceModel`] at one quantity, exactly, in major currency units. It
//! is a reference function for tests and arithmetic oracles, such as a consumer that checks its
//! own model arithmetic against Pricing's. It is not a quote, and nothing prices with it outside
//! tests and oracles. It applies no allowance, proration, minimum-fee floor, promotion or
//! rounding: D-415 keeps those with Rating, and an invoice line rounds downstream (D-535).
//!
//! The Pricing gear evaluates a stored price through this function and judges a price's content
//! with [`price_violations`] and [`tier_violations`], so there is one implementation of each.

use bss_money::exact::{ExactAmount, ExactError};
use rust_decimal::Decimal;

use crate::numeric::{self, NumericError};
use crate::read::{PriceModel, Tier};

/// A content rule of a price that the price breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PriceViolation {
    /// A decimal of the price breaks its field's limit ([`crate::numeric`], D-527): a rate
    /// (`amount`, `unit_amount`, `package_price`, a band's `rate`), a band's `up_to`, or a
    /// `package_size`, which must be above zero.
    #[error("a decimal of the price breaks its limit: {0}")]
    Numeric(NumericError),
    /// A volume or graduated price has no band.
    #[error("a tiered price has no band")]
    NoBands,
    /// The last band has an upper bound: the top band must be open.
    #[error("the top band of a tiered price has an upper bound")]
    TopBandClosed,
    /// A band before the last has no upper bound, or a bound that is not above the bound of the
    /// band before it.
    #[error("the bands of a tiered price do not rise")]
    BandsOutOfOrder,
}

/// Why [`amount_for`] refused a price or a quantity.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AmountError {
    /// The price breaks a content rule; the first one found ([`price_violations`]).
    #[error("the price breaks a content rule: {0}")]
    Price(#[from] PriceViolation),
    /// The quantity is below zero.
    #[error("the quantity is below zero")]
    NegativeQuantity,
    /// An exact step failed. A price that passes [`price_violations`] keeps every step inside the
    /// exact budget, so a valid price never reaches this.
    #[error("exact arithmetic failed: {0}")]
    Arithmetic(#[from] ExactError),
}

/// Every content rule the bands of a volume or graduated price break, in band order (D-387).
///
/// No band at all is [`PriceViolation::NoBands`] alone. Each band then gives its rate's limit,
/// then its `up_to`'s limit, then its position: the last band must have no `up_to`
/// ([`PriceViolation::TopBandClosed`]); every other band needs an `up_to` above the bound of the
/// band before it, the first one above zero ([`PriceViolation::BandsOutOfOrder`]).
#[must_use]
pub fn tier_violations(tiers: &[Tier]) -> Vec<PriceViolation> {
    if tiers.is_empty() {
        return vec![PriceViolation::NoBands];
    }
    let mut violations = Vec::new();
    let mut previous = Decimal::ZERO;
    for (index, tier) in tiers.iter().enumerate() {
        violations.extend(limit(numeric::validate_rate(tier.rate)));
        violations.extend(
            tier.up_to
                .and_then(|top| limit(numeric::validate_threshold(top))),
        );
        if index + 1 == tiers.len() {
            if tier.up_to.is_some() {
                violations.push(PriceViolation::TopBandClosed);
            }
        } else if let Some(top) = tier.up_to.filter(|top| *top > previous) {
            previous = top;
        } else {
            violations.push(PriceViolation::BandsOutOfOrder);
        }
    }
    violations
}

/// Every content rule `price` breaks. A flat amount, a per-unit amount and a package price are
/// rates; a package size is a positive quantity, judged before the package price, and only the
/// first of the two is reported. A volume or graduated price is judged by [`tier_violations`].
#[must_use]
pub fn price_violations(price: &PriceModel) -> Vec<PriceViolation> {
    match price {
        PriceModel::Flat { amount } => limit(numeric::validate_rate(*amount)).into_iter().collect(),
        PriceModel::PerUnit { unit_amount } => limit(numeric::validate_rate(*unit_amount))
            .into_iter()
            .collect(),
        PriceModel::Package {
            package_size,
            package_price,
        } => limit(numeric::validate_positive_quantity(*package_size))
            .or_else(|| limit(numeric::validate_rate(*package_price)))
            .into_iter()
            .collect(),
        PriceModel::Volume { tiers } | PriceModel::Graduated { tiers } => tier_violations(tiers),
    }
}

/// The refusal of a checked value, if any.
fn limit(checked: Result<Decimal, NumericError>) -> Option<PriceViolation> {
    checked.err().map(PriceViolation::Numeric)
}

/// The exact value of a decimal.
fn exact(value: Decimal) -> ExactAmount {
    ExactAmount::from_decimal(value)
}

/// Price one quantity of one price, exactly, in major currency units (D-549).
///
/// This is a reference function: it prices one price over one quantity, for tests and arithmetic
/// oracles. It applies no allowance, proration, minimum-fee floor, promotion or rounding (D-415
/// keeps those with Rating), it is not a quote, and nothing prices with it outside tests and
/// oracles.
///
/// The price's content is judged first ([`price_violations`]), then the quantity, which must not
/// be below zero. Then:
/// - a flat price is its amount, whatever the quantity;
/// - a per-unit price is the quantity times the unit amount;
/// - a package price counts whole packages, `ceil(quantity / package_size)`, then multiplies that
///   count by the package price; the count is a number of packages, not money;
/// - a volume price takes the band the quantity falls in, `[previous up_to, up_to)` (D-387), and
///   applies its rate to the whole quantity;
/// - a graduated price applies each band's rate to the part of the quantity inside that band.
///
/// Nothing is rounded and nothing is narrowed to a `Decimal`. The bounds all fit the 256-digit
/// budget of [`ExactAmount`]:
/// - an input (a quantity, a threshold or a package size) is below 10^28 and has at most 28
///   places;
/// - a product is below 10^56 and has at most 56 places;
/// - a package count is at most 10^56;
/// - a package amount is below 10^84.
///
/// A rate has at most 12 places, so an amount has at most 40. A `quantity` is any `Decimal` that
/// is not negative; one above 10^28 also fits.
///
/// # Errors
/// [`AmountError::Price`] with the first content rule the price breaks;
/// [`AmountError::NegativeQuantity`] for a quantity below zero; [`AmountError::Arithmetic`] for a
/// failed exact step, which a valid price never reaches.
pub fn amount_for(price: &PriceModel, quantity: Decimal) -> Result<ExactAmount, AmountError> {
    if let Some(violation) = price_violations(price).first() {
        return Err(AmountError::Price(*violation));
    }
    if quantity < Decimal::ZERO {
        return Err(AmountError::NegativeQuantity);
    }
    let units = exact(quantity);
    match price {
        PriceModel::Flat { amount } => Ok(exact(*amount)),
        PriceModel::PerUnit { unit_amount } => Ok(units.checked_mul(&exact(*unit_amount))?),
        PriceModel::Package {
            package_size,
            package_price,
        } => {
            let packages = units.checked_div(&exact(*package_size))?.ceil()?;
            Ok(packages.checked_mul(&exact(*package_price))?)
        }
        PriceModel::Volume { tiers } => {
            let band = tiers
                .iter()
                .find(|tier| tier.up_to.is_none_or(|top| quantity < top))
                .ok_or(PriceViolation::TopBandClosed)?;
            Ok(units.checked_mul(&exact(band.rate))?)
        }
        PriceModel::Graduated { tiers } => {
            let mut total = exact(Decimal::ZERO);
            let mut lower = Decimal::ZERO;
            for tier in tiers {
                let upper = tier.up_to.unwrap_or(quantity).min(quantity);
                if upper > lower {
                    let width = exact(upper).checked_sub(&exact(lower))?;
                    total = total.checked_add(&width.checked_mul(&exact(tier.rate))?)?;
                }
                if upper == quantity {
                    break;
                }
                lower = upper;
            }
            Ok(total)
        }
    }
}

#[cfg(test)]
#[path = "amount_tests.rs"]
mod tests;
