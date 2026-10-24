//! Exact money and quantities of generation 1 (rating DESIGN §3.3, T-D-66, T-D-80; ADR-0005).
//!
//! [`ExactMoney`] is a reduced fraction in **major** units of one currency at the posting scale
//! stored with its price; [`ExactQuantity`] is a finite decimal of at most 60 integer and 28
//! fractional digits. Both compute with the vendored `bss_money::exact::ExactAmount` and never
//! round: the only rounding inside an evaluation is declared business rounding of a quantity
//! (Products' per-granule rounding, the package block `ceil`), and Billing rounds each invoice
//! line once.
//!
//! - **Conversion.** A Pricing decimal enters as the reduced fraction of its digits over
//!   `10^scale`, tagged with the price's currency and stored scale; nothing is multiplied by
//!   `10^currency_scale`.
//! - **Currency.** Two amounts combine only when code and scale both match, else
//!   `binding_currency_mismatch`; nothing is converted (money ADR rule 9).
//! - **Budget.** Every multiply and divide checks its operands against the generation's
//!   [`ArithmeticBudget`] first (`precision_overflow`).
//! - **Bounds.** A delivered amount (a line, a price total, a minimum fee) leaves the core only
//!   through [`ExactMoney::to_wire`], which refuses `abs(numerator) >= 10^84` or
//!   `denominator >= 10^64` with `precision_overflow`; a quantity computed inside evaluation beyond
//!   60 integer or 28 fractional digits is `quantity_out_of_range`. Both are raised before anything
//!   is persisted or delivered. An intermediate amount is bounded by the budget only, so lineage
//!   records it with [`ExactMoney::to_lineage`], never through the delivery bound.
//! - **Canonical text.** `-` only for a negative value, no `+`, no leading zeros except the `0`
//!   before a point, no trailing fractional zeros, no exponent, every zero `0`; a fraction is
//!   reduced with the sign on the numerator, zero `0/1`. Every Rating digest hashes this text.

// @cpt-algo:cpt-cf-bss-rating-algo-core-exact-arithmetic:p1

use std::fmt;

use rating_gen1_money::exact::ExactAmount;
use rust_decimal::Decimal;
use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};

use crate::budget::ArithmeticBudget;
use crate::error::EvaluationError;
use crate::fraction;
use crate::input::CoreCurrency;
use crate::outcome::MoneyText;

/// The most digits of a delivered numerator: `abs(numerator) < 10^84`.
pub const MAX_NUMERATOR_DIGITS: u32 = 84;
/// The most digits of a delivered denominator: `denominator < 10^64`.
pub const MAX_DENOMINATOR_DIGITS: u32 = 64;
/// The most integer digits of a quantity (`NUMERIC(88,28)`).
pub const MAX_QUANTITY_INTEGER_DIGITS: usize = 60;
/// The most fractional digits of a quantity.
pub const MAX_QUANTITY_FRACTION_DIGITS: u8 = 28;
/// The longest quantity text a parser looks at.
const MAX_QUANTITY_TEXT: usize = 128;

/// The message a JSON number in a quantity field is refused with.
pub const REQUIRES_STRING: &str = "the field requires a string";

/// Why a text is not a canonical quantity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QuantityTextError {
    /// A sign, leading zero, exponent, separator or `-0` the canonical form refuses.
    #[error("not a canonical decimal string")]
    NotCanonical,
    /// More than 60 integer or 28 fractional digits.
    #[error("outside the quantity bound (60 integer and 28 fractional digits)")]
    OutOfRange,
}

/// A finite exact decimal quantity in canonical text.
///
/// Fractional trailing zeros are accepted on input and dropped (`10.000` reads as `10`); two equal
/// quantities therefore have one text, and equality of the text is equality of the value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExactQuantity(String);

impl ExactQuantity {
    /// Zero.
    #[must_use]
    pub fn zero() -> Self {
        Self("0".to_owned())
    }

    /// Parses a decimal quantity into its canonical form.
    ///
    /// # Errors
    ///
    /// [`QuantityTextError`] for malformed text or a value beyond the bound.
    pub fn parse(text: &str) -> Result<Self, QuantityTextError> {
        if text.is_empty() || text.len() > MAX_QUANTITY_TEXT {
            return Err(QuantityTextError::NotCanonical);
        }
        let (negative, unsigned) = text
            .strip_prefix('-')
            .map_or((false, text), |rest| (true, rest));
        let (whole, fractional) = unsigned
            .split_once('.')
            .map_or((unsigned, None), |(w, f)| (w, Some(f)));
        let canonical_whole = !whole.is_empty()
            && whole.bytes().all(|byte| byte.is_ascii_digit())
            && (whole == "0" || !whole.starts_with('0'));
        if !canonical_whole
            || fractional.is_some_and(|f| f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(QuantityTextError::NotCanonical);
        }
        let fractional = fractional.map_or("", |f| f.trim_end_matches('0'));
        if whole.len() > MAX_QUANTITY_INTEGER_DIGITS
            || fractional.len() > usize::from(MAX_QUANTITY_FRACTION_DIGITS)
        {
            return Err(QuantityTextError::OutOfRange);
        }
        if negative && whole == "0" && fractional.is_empty() {
            return Err(QuantityTextError::NotCanonical);
        }
        let mut canonical = String::with_capacity(text.len());
        if negative {
            canonical.push('-');
        }
        canonical.push_str(whole);
        if !fractional.is_empty() {
            canonical.push('.');
            canonical.push_str(fractional);
        }
        Ok(Self(canonical))
    }

    /// The canonical quantity of an exact value computed at `site`.
    ///
    /// # Errors
    ///
    /// `quantity_out_of_range` when the value needs more than 28 fractional or 60 integer digits.
    pub fn from_exact(value: &ExactAmount, site: &str) -> Result<Self, EvaluationError> {
        let out_of_range = || EvaluationError::QuantityOutOfRange {
            detail: format!(
                "{site}: the quantity {value} exceeds 60 integer or 28 fractional digits"
            ),
        };
        // The first scale the value terminates at is its canonical text (trailing zeros trimmed).
        let text = (0..=MAX_QUANTITY_FRACTION_DIGITS)
            .find_map(|scale| value.canonical_at_scale(scale).ok())
            .ok_or_else(out_of_range)?;
        Self::parse(&text).map_err(|_| out_of_range())
    }

    /// The exact value.
    ///
    /// # Errors
    ///
    /// `precision_overflow` if the money crate's budget refuses it (never, within the bound).
    pub fn to_exact(&self) -> Result<ExactAmount, EvaluationError> {
        let unsigned = self.0.strip_prefix('-').unwrap_or(&self.0);
        let (whole, fractional) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        let digits = format!(
            "{}{whole}{fractional}",
            if self.is_negative() { "-" } else { "" }
        );
        let digits = bigdecimal::num_bigint::BigInt::parse_bytes(digits.as_bytes(), 10)
            .ok_or_else(|| EvaluationError::MoneyContractViolation {
                detail: format!("quantity text {:?} is not canonical", self.0),
            })?;
        let scale = u8::try_from(fractional.len()).unwrap_or(u8::MAX);
        fraction::from_scaled(&digits, scale)
    }

    /// Whether the quantity is below zero.
    #[must_use]
    pub fn is_negative(&self) -> bool {
        self.0.starts_with('-')
    }

    /// The canonical text, which is also what every Rating digest hashes.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The exact sum.
    ///
    /// # Errors
    ///
    /// `quantity_out_of_range` beyond the bound, `precision_overflow` beyond the budget.
    pub fn checked_add(&self, rhs: &Self, site: &str) -> Result<Self, EvaluationError> {
        let sum = self
            .to_exact()?
            .checked_add(&rhs.to_exact()?)
            .map_err(|error| EvaluationError::from_money(site, &error))?;
        Self::from_exact(&sum, site)
    }
}

impl fmt::Display for ExactQuantity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ExactQuantity {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ExactQuantity {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = deserializer.deserialize_any(TextOnly)?;
        Self::parse(&text).map_err(de::Error::custom)
    }
}

/// Accepts a string and refuses every other JSON type, a number in particular.
struct TextOnly;

impl Visitor<'_> for TextOnly {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(REQUIRES_STRING)
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<String, E> {
        Ok(value.to_owned())
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<String, E> {
        Ok(value)
    }
}

/// An exact amount in major units of one currency at the posting scale stored with its price.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactMoney {
    amount: ExactAmount,
    currency: CoreCurrency,
}

impl ExactMoney {
    /// Zero in `currency`.
    #[must_use]
    pub fn zero(currency: &CoreCurrency) -> Self {
        Self {
            amount: ExactAmount::from_coefficient(0, 0),
            currency: currency.clone(),
        }
    }

    // @cpt-begin:cpt-cf-bss-rating-algo-core-exact-arithmetic:p1:inst-ex-convert
    /// A Pricing decimal as the reduced fraction of its digits over `10^scale`, in major units of
    /// the price's currency (T-D-74, T-D-80); never rescaled or rounded.
    #[must_use]
    pub fn from_price(value: Decimal, currency: &CoreCurrency) -> Self {
        Self {
            amount: ExactAmount::from_decimal(value),
            currency: currency.clone(),
        }
    }

    /// The exact amount.
    #[must_use]
    pub fn amount(&self) -> &ExactAmount {
        &self.amount
    }

    /// The currency code and stored posting scale.
    #[must_use]
    pub fn currency(&self) -> &CoreCurrency {
        &self.currency
    }

    /// Whether the amount is below zero.
    #[must_use]
    pub fn is_negative(&self) -> bool {
        self.amount.is_negative()
    }

    /// The exact sum of two amounts of one currency and scale.
    ///
    /// # Errors
    ///
    /// `binding_currency_mismatch` for another code or scale, `precision_overflow` beyond the
    /// budget.
    pub fn checked_add(&self, rhs: &Self, site: &str) -> Result<Self, EvaluationError> {
        self.same_currency(rhs, site)?;
        let amount = self
            .amount
            .checked_add(&rhs.amount)
            .map_err(|error| EvaluationError::from_money(site, &error))?;
        Ok(self.with(amount))
    }

    /// The exact difference of two amounts of one currency and scale.
    ///
    /// # Errors
    ///
    /// As [`Self::checked_add`].
    pub fn checked_sub(&self, rhs: &Self, site: &str) -> Result<Self, EvaluationError> {
        self.same_currency(rhs, site)?;
        let amount = self
            .amount
            .checked_sub(&rhs.amount)
            .map_err(|error| EvaluationError::from_money(site, &error))?;
        Ok(self.with(amount))
    }

    // @cpt-end:cpt-cf-bss-rating-algo-core-exact-arithmetic:p1:inst-ex-convert

    /// The amount times an exact quantity (a per-unit rate times units).
    ///
    /// # Errors
    ///
    /// `precision_overflow` when an operand leaves the budget or the product the crate's limit.
    pub fn times_quantity(
        &self,
        quantity: &ExactQuantity,
        budget: ArithmeticBudget,
        site: &str,
    ) -> Result<Self, EvaluationError> {
        self.times(&quantity.to_exact()?, budget, site)
    }

    /// The amount times `numerator / denominator` (a covered fraction of a period).
    ///
    /// # Errors
    ///
    /// `period_geometry_mismatch` for a zero denominator, `precision_overflow` beyond the budget.
    pub fn times_ratio(
        &self,
        numerator: u64,
        denominator: u64,
        budget: ArithmeticBudget,
        site: &str,
    ) -> Result<Self, EvaluationError> {
        if denominator == 0 {
            return Err(EvaluationError::PeriodGeometryMismatch {
                detail: format!("{site}: an empty period has no covered fraction"),
            });
        }
        let ratio = ExactAmount::from_coefficient(i128::from(numerator), 0)
            .checked_div(&ExactAmount::from_coefficient(i128::from(denominator), 0))
            .map_err(|error| EvaluationError::from_money(site, &error))?;
        self.times(&ratio, budget, site)
    }

    // @cpt-begin:cpt-cf-bss-rating-algo-core-exact-arithmetic:p1:inst-ex-budget
    /// The amount times an exact factor, budget-checked first.
    ///
    /// # Errors
    ///
    /// `precision_overflow` when an operand leaves the budget or the product the crate's limit.
    pub fn times(
        &self,
        factor: &ExactAmount,
        budget: ArithmeticBudget,
        site: &str,
    ) -> Result<Self, EvaluationError> {
        budget.check_operands(site, &self.amount, factor)?;
        let amount = self
            .amount
            .checked_mul(factor)
            .map_err(|error| EvaluationError::from_money(site, &error))?;
        Ok(self.with(amount))
    }

    // @cpt-end:cpt-cf-bss-rating-algo-core-exact-arithmetic:p1:inst-ex-budget

    // @cpt-begin:cpt-cf-bss-rating-algo-core-exact-arithmetic:p1:inst-ex-bounds
    /// The delivered form: reduced numerator and denominator as canonical integer text, with
    /// `currency` and `currency_scale`.
    ///
    /// # Errors
    ///
    /// `precision_overflow` when `abs(numerator) >= 10^84` or `denominator >= 10^64`.
    pub fn to_wire(&self, site: &str) -> Result<MoneyText, EvaluationError> {
        let text = self.to_lineage()?;
        let (numerator_digits, denominator_digits) = (
            digits(text.numerator.trim_start_matches('-')),
            digits(&text.denominator),
        );
        if numerator_digits > MAX_NUMERATOR_DIGITS || denominator_digits > MAX_DENOMINATOR_DIGITS {
            return Err(EvaluationError::PrecisionOverflow {
                detail: format!(
                    "{site}: the amount has a numerator of {numerator_digits} digits and a \
                     denominator of {denominator_digits} (bounds: abs(numerator) < 10^84, \
                     denominator < 10^64)"
                ),
            });
        }
        Ok(text)
    }

    /// The same text for an intermediate amount in lineage (a model amount before proration, a
    /// pre-clamp amount): bounded by the arithmetic budget only, never by the delivery bound
    /// (DESIGN §3.3 keeps the two apart).
    ///
    /// # Errors
    ///
    /// `money_contract_violation` if the money crate's text cannot be read back.
    pub fn to_lineage(&self) -> Result<MoneyText, EvaluationError> {
        let (numerator, denominator) = fraction::parts(&self.amount)?;
        Ok(MoneyText {
            numerator: numerator.to_str_radix(10),
            denominator: denominator.to_str_radix(10),
            currency: self.currency.code.clone(),
            currency_scale: self.currency.scale,
        })
    }

    // @cpt-end:cpt-cf-bss-rating-algo-core-exact-arithmetic:p1:inst-ex-bounds

    fn same_currency(&self, rhs: &Self, site: &str) -> Result<(), EvaluationError> {
        if self.currency == rhs.currency {
            return Ok(());
        }
        Err(EvaluationError::BindingCurrencyMismatch {
            detail: format!(
                "{site}: {} at scale {} cannot meet {} at scale {}; nothing is converted",
                self.currency.code, self.currency.scale, rhs.currency.code, rhs.currency.scale
            ),
        })
    }

    fn with(&self, amount: ExactAmount) -> Self {
        Self {
            amount,
            currency: self.currency.clone(),
        }
    }
}

/// The number of digits of canonical integer text without its sign.
fn digits(text: &str) -> u32 {
    u32::try_from(text.len()).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[path = "exact_tests.rs"]
mod tests;
