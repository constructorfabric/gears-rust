//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-contract:p1
//! Exact quota unit arithmetic (P-D-267). An amount is a canonical decimal string of at most 39
//! digits read as `u128`; a unit's factor into its canonical unit is a positive, reduced fraction;
//! a conversion multiplies with checks and refuses a result that is not whole or does not fit.
//! No float anywhere.
use std::fmt;

/// The most digits a canonical amount has: `u128::MAX` is 39 digits long.
const MAX_DIGITS: usize = 39;

/// A non-negative quota amount, read from a canonical decimal string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Amount(u128);

impl Amount {
    /// Read a canonical decimal: `0`, or digits without a leading zero, at most 39 of them, with
    /// no sign, point, exponent or space.
    ///
    /// # Errors
    /// [`UnitError::NotDecimal`] for any other text; [`UnitError::Overflow`] for 39 digits whose
    /// value does not fit in 128 bits.
    pub fn parse(text: &str) -> Result<Self, UnitError> {
        let canonical = !text.is_empty()
            && text.len() <= MAX_DIGITS
            && text.bytes().all(|byte| byte.is_ascii_digit())
            && (text == "0" || !text.starts_with('0'));
        if !canonical {
            return Err(UnitError::NotDecimal);
        }
        // The text is digits only, so the one way `parse` can still fail is a value over 128 bits.
        text.parse::<u128>()
            .map(Self)
            .map_err(|_| UnitError::Overflow)
    }

    /// The amount as an integer.
    #[must_use]
    pub const fn get(self) -> u128 {
        self.0
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A unit's exact factor into its canonical unit: `numerator / denominator`, both positive and
/// without a common divisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Factor {
    numerator: u128,
    denominator: u128,
}

impl Factor {
    /// The canonical unit's factor into itself.
    pub const ONE: Self = Self {
        numerator: 1,
        denominator: 1,
    };

    /// Read a factor from the two canonical decimal strings a unit instance carries.
    ///
    /// # Errors
    /// [`UnitError::NotDecimal`] or [`UnitError::Overflow`] when either text is not an amount
    /// ([`Amount::parse`]); [`UnitError::NotPositive`] when either is zero;
    /// [`UnitError::NotReduced`] when they share a divisor.
    pub fn parse(numerator: &str, denominator: &str) -> Result<Self, UnitError> {
        let numerator = Amount::parse(numerator)?.get();
        let denominator = Amount::parse(denominator)?.get();
        if numerator == 0 || denominator == 0 {
            return Err(UnitError::NotPositive);
        }
        if gcd(numerator, denominator) != 1 {
            return Err(UnitError::NotReduced);
        }
        Ok(Self {
            numerator,
            denominator,
        })
    }

    /// Whether this is the factor 1/1 a canonical unit carries.
    #[must_use]
    pub const fn is_one(&self) -> bool {
        self.numerator == 1 && self.denominator == 1
    }

    /// The factor's numerator.
    #[must_use]
    pub const fn numerator(&self) -> u128 {
        self.numerator
    }

    /// The factor's denominator.
    #[must_use]
    pub const fn denominator(&self) -> u128 {
        self.denominator
    }
}

/// Convert `amount` from a unit with factor `from` into a unit with factor `to`, both into the
/// same canonical unit: `amount × from.numerator × to.denominator / (from.denominator ×
/// to.numerator)`.
///
/// # Errors
/// [`UnitError::Overflow`] when a product does not fit in 128 bits; [`UnitError::Inexact`] when
/// the result is not a whole amount of the target unit.
pub fn convert(amount: Amount, from: Factor, to: Factor) -> Result<Amount, UnitError> {
    let numerator = amount
        .0
        .checked_mul(from.numerator)
        .and_then(|product| product.checked_mul(to.denominator))
        .ok_or(UnitError::Overflow)?;
    let denominator = from
        .denominator
        .checked_mul(to.numerator)
        .ok_or(UnitError::Overflow)?;
    if numerator % denominator != 0 {
        return Err(UnitError::Inexact);
    }
    // The denominator is a product of positive factors, so the division cannot fail.
    numerator
        .checked_div(denominator)
        .map(Amount)
        .ok_or(UnitError::Overflow)
}

/// Euclid's greatest common divisor.
const fn gcd(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let rest = left % right;
        left = right;
        right = rest;
    }
    left
}

/// Why an amount, a factor or a conversion is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitError {
    /// The text is not a canonical decimal integer of at most 39 digits.
    NotDecimal,
    /// A factor's numerator or denominator is zero.
    NotPositive,
    /// A factor's numerator and denominator share a divisor.
    NotReduced,
    /// The converted amount is not whole in the target unit.
    Inexact,
    /// A value or a product does not fit in 128 bits.
    Overflow,
}

impl UnitError {
    /// The stable refusal code the catalog answers for this error.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotDecimal | Self::NotPositive | Self::NotReduced | Self::Overflow => {
                "QUOTA_UNIT_INVALID"
            }
            Self::Inexact => "QUOTA_UNIT_INEXACT",
        }
    }
}

impl fmt::Display for UnitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotDecimal => "not a canonical decimal integer of at most 39 digits",
            Self::NotPositive => "a unit factor must be positive",
            Self::NotReduced => "a unit factor must be a reduced fraction",
            Self::Inexact => "the amount does not convert exactly into the unit",
            Self::Overflow => "the amount does not fit in 128 bits",
        })
    }
}

impl std::error::Error for UnitError {}

#[cfg(test)]
#[path = "units_tests.rs"]
mod tests;
