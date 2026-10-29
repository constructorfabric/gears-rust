//! Bounded exact fractions in major units, narrowed only at an explicit posting boundary.

use std::borrow::Borrow;
use std::cmp::Ordering;
use std::sync::LazyLock;

/// The integer type of [`ExactAmount::numer`], [`ExactAmount::denom`] and
/// [`ExactAmount::from_parts`], re-exported so callers use the crate's version.
pub use bigdecimal::num_bigint::BigInt;
use bigdecimal::num_bigint::BigUint;
use bigdecimal::num_traits::{One, Signed, ToPrimitive, Zero};
use rust_decimal::Decimal;

use crate::money::split_decimal;
use crate::{CurrencySpec, MoneyError, PostedMoney};

const FINAL_DIGITS: u32 = 256;
const PRODUCT_DIGITS: u32 = 512;
const SUM_DIGITS: u32 = 513;

/// The most digits of a canonical integer string: the reduced budget, `FINAL_DIGITS`.
const INTEGER_TEXT_DIGITS: usize = 256;
/// The most integer digits [`ExactAmount::parse_decimal`] reads.
const DECIMAL_INTEGER_DIGITS: usize = 60;
/// The most fractional digits [`ExactAmount::parse_decimal`] reads, after trailing zeros.
const DECIMAL_FRACTION_DIGITS: u32 = 28;
/// The longest text [`ExactAmount::parse_decimal`] reads: a sign, 60 + 28 digits, the
/// point and room for fractional trailing zeros.
const DECIMAL_TEXT_BYTES: usize = 128;

/// Exclusive magnitude bounds: an integer has more than `N` decimal digits
/// exactly when its magnitude is at least `10^N`. Computed once, so a digit
/// check is one comparison instead of a clone and a base-10 rendering.
static FINAL_BOUND: LazyLock<BigUint> = LazyLock::new(|| digit_bound(FINAL_DIGITS));
static PRODUCT_BOUND: LazyLock<BigUint> = LazyLock::new(|| digit_bound(PRODUCT_DIGITS));
static SUM_BOUND: LazyLock<BigUint> = LazyLock::new(|| digit_bound(SUM_DIGITS));

/// An exact intermediate amount, with a positive, reduced denominator.
///
/// Equality and order are both defined by exact value ([`Ord::cmp`]), so they
/// agree even for a fraction that is not reduced.
#[derive(Clone, Debug)]
pub struct ExactAmount {
    numerator: BigInt,
    denominator: BigInt,
}

/// A validation or resource-limit failure in exact monetary arithmetic.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExactError {
    /// The final posting or its metadata violates the money contract.
    #[error(transparent)]
    Money(#[from] MoneyError),
    /// A reduced integer exceeds 256 digits or scratch exceeds its derived bound.
    #[error("exact arithmetic limit exceeded")]
    ArithmeticLimit,
    /// The divisor is zero.
    #[error("division by zero")]
    DivisionByZero,
    /// Allocation requires at least one weight.
    #[error("allocation weights must be non-empty")]
    EmptyWeights,
    /// Allocation weights cannot be negative.
    #[error("allocation weights must be non-negative")]
    NegativeWeight,
    /// The operation has more terms than its business limit allows.
    #[error("too many terms")]
    TooManyTerms,
}

impl ExactAmount {
    /// Preserve all decimal digits as a reduced fraction; every Decimal fits the budget.
    #[must_use]
    pub fn from_decimal(value: Decimal) -> Self {
        Self::normalize(BigInt::from(value.mantissa()), ten_pow(value.scale()))
    }

    /// The exact value `coefficient × 10^-scale` (`1230` at scale 2 is `12.3`).
    /// Every `i128` coefficient at any `u8` scale fits the budget.
    #[must_use]
    pub fn from_coefficient(coefficient: i128, scale: u8) -> Self {
        Self::normalize(BigInt::from(coefficient), ten_pow(u32::from(scale)))
    }

    /// The reduced fraction `numerator / denominator`, with the sign moved to the
    /// numerator: `94 / -800` is `-47/400`, and any zero is `0/1`.
    ///
    /// # Errors
    /// Returns [`ExactError::DivisionByZero`] for a zero denominator, or
    /// [`ExactError::ArithmeticLimit`] when an input is too large to reduce (more than 513
    /// numerator or 512 denominator digits) or the reduced fraction exceeds 256 digits.
    pub fn from_parts(numerator: BigInt, denominator: BigInt) -> Result<Self, ExactError> {
        Self::reduce(numerator, denominator)
    }

    /// The exact value of plain decimal text with at most 60 integer and 28 fractional
    /// digits, past the 28 significant digits of [`Self::from_decimal`]: the widest
    /// quantity a BSS contract publishes (`NUMERIC(88,28)`). The syntax is
    /// [`crate::parse_decimal`]'s: fractional trailing zeros are accepted, while exponents,
    /// whitespace, `+`, leading integer zeros and negative zero are rejected.
    ///
    /// # Errors
    /// Returns [`MoneyError::InvalidDecimal`] for any other text, text over 128 bytes or
    /// more than 28 fractional digits, and [`MoneyError::AmountOutOfRange`] for more than
    /// 60 integer digits.
    pub fn parse_decimal(text: &str) -> Result<Self, ExactError> {
        if text.len() > DECIMAL_TEXT_BYTES {
            return Err(MoneyError::InvalidDecimal.into());
        }
        let parts = split_decimal(text)?;
        let scale = u32::try_from(parts.fraction.len())
            .ok()
            .filter(|scale| *scale <= DECIMAL_FRACTION_DIGITS)
            .ok_or(MoneyError::InvalidDecimal)?;
        if parts.integer.len() > DECIMAL_INTEGER_DIGITS {
            return Err(MoneyError::AmountOutOfRange.into());
        }
        let magnitude = parse_digits(&[parts.integer, parts.fraction].concat())?;
        let numerator = if parts.negative {
            -magnitude
        } else {
            magnitude
        };
        // At most 88 digits over 10^28 always fits the budget.
        Ok(Self::normalize(numerator, ten_pow(scale)))
    }

    /// Read back the text of [`Self::canonical_fraction`], and nothing else, so a stored or
    /// received fraction has one text and one digest: integer strings with `-` only on a
    /// negative numerator, no `+`, whitespace, point or leading zeros, a positive
    /// denominator and no common factor (`1`/`2` and `0`/`1` are accepted, `2`/`4` and
    /// `0`/`2` are not). Use [`Self::from_parts`] to reduce other integers.
    ///
    /// Canonical means reduced and well-formed, not inside the caller's contract: each
    /// integer is bounded only by the crate's 256-digit budget, so a caller with narrower
    /// wire or storage bounds still checks them on the parsed value.
    ///
    /// # Errors
    /// Returns [`MoneyError::InvalidDecimal`] for text that is not that form, however long,
    /// [`ExactError::DivisionByZero`] for a zero denominator, or
    /// [`ExactError::ArithmeticLimit`] for a well-formed integer of more than 256 digits,
    /// refused after one byte scan without building a number.
    pub fn parse_fraction(numerator: &str, denominator: &str) -> Result<Self, ExactError> {
        let numerator = canonical_integer(numerator)?;
        let denominator = canonical_integer(denominator)?;
        if denominator.is_zero() {
            return Err(ExactError::DivisionByZero);
        }
        if denominator.is_negative() || !gcd(&numerator, &denominator).is_one() {
            return Err(MoneyError::InvalidDecimal.into());
        }
        Ok(Self {
            numerator,
            denominator,
        })
    }

    /// The reduced numerator, which carries the sign; `0` for zero.
    #[must_use]
    pub fn numer(&self) -> &BigInt {
        &self.numerator
    }

    /// The reduced denominator, always positive; `1` for a whole number.
    #[must_use]
    pub fn denom(&self) -> &BigInt {
        &self.denominator
    }

    /// The canonical fraction text (money ADR rule 6) as `(numerator, denominator)`
    /// integer strings: reduced, the sign on the numerator, a positive denominator, no `+`
    /// or leading zeros, and zero as `0`/`1`. `47/400` gives `("47", "400")` and `-3` gives
    /// `("-3", "1")`. This is the form for the wire, storage and digests;
    /// [`Self::parse_fraction`] reads it back, and `Display` is for messages only.
    #[must_use]
    pub fn canonical_fraction(&self) -> (String, String) {
        (
            self.numerator.to_str_radix(10),
            self.denominator.to_str_radix(10),
        )
    }

    /// Inspect the exact sign without narrowing an intermediate to a posted amount.
    #[must_use]
    pub fn is_negative(&self) -> bool {
        self.numerator.is_negative()
    }

    /// Add exactly, allowing bounded unreduced intermediates to cancel.
    ///
    /// # Errors
    /// Returns [`ExactError::ArithmeticLimit`] if the reduced result exceeds the budget.
    pub fn checked_add(&self, rhs: &Self) -> Result<Self, ExactError> {
        self.combine(rhs, false)
    }

    /// Subtract exactly, allowing bounded unreduced intermediates to cancel.
    ///
    /// # Errors
    /// Returns [`ExactError::ArithmeticLimit`] if the reduced result exceeds the budget.
    pub fn checked_sub(&self, rhs: &Self) -> Result<Self, ExactError> {
        self.combine(rhs, true)
    }

    /// Multiply without rounding or narrowing to the decimal carrier.
    ///
    /// # Errors
    /// Returns [`ExactError::ArithmeticLimit`] if the reduced result exceeds the budget.
    pub fn checked_mul(&self, rhs: &Self) -> Result<Self, ExactError> {
        self.check_operands(rhs)?;
        Self::reduce(
            product(&self.numerator, &rhs.numerator)?,
            product(&self.denominator, &rhs.denominator)?,
        )
    }

    /// Divide without approximating a non-terminating quotient.
    ///
    /// # Errors
    /// Returns [`ExactError::DivisionByZero`] for zero divisors, or
    /// [`ExactError::ArithmeticLimit`] if the reduced result exceeds the budget.
    pub fn checked_div(&self, rhs: &Self) -> Result<Self, ExactError> {
        if rhs.numerator.is_zero() {
            return Err(ExactError::DivisionByZero);
        }
        self.check_operands(rhs)?;
        Self::reduce(
            product(&self.numerator, &rhs.denominator)?,
            product(&self.denominator, &rhs.numerator)?,
        )
    }

    /// The smallest integer that is not below the value: `7/3` gives `3`, `-7/3` gives `-2`
    /// and `-1/3` gives `0`. The result is an exact whole number, not a posting.
    ///
    /// # Errors
    /// Returns [`ExactError::ArithmeticLimit`] if the fraction exceeds its budget.
    pub fn ceil(&self) -> Result<Self, ExactError> {
        self.check_budget()?;
        // Integer division truncates toward zero, which is already the ceiling of a negative
        // value; only a positive value with a remainder needs one more.
        let mut whole = &self.numerator / &self.denominator;
        if self.numerator.is_positive() && !(&self.numerator % &self.denominator).is_zero() {
            whole += 1;
        }
        Self::reduce(whole, BigInt::one())
    }

    /// The largest integer that is not above the value: `7/3` gives `2`, `-7/3` gives `-3`
    /// and `1/3` gives `0`. The result is an exact whole number, not a posting.
    ///
    /// # Errors
    /// Returns [`ExactError::ArithmeticLimit`] if the fraction exceeds its budget.
    pub fn floor(&self) -> Result<Self, ExactError> {
        self.check_budget()?;
        // Integer division truncates toward zero, which is already the floor of a positive
        // value; only a negative value with a remainder needs one less.
        let mut whole = &self.numerator / &self.denominator;
        if self.numerator.is_negative() && !(&self.numerator % &self.denominator).is_zero() {
            whole -= 1;
        }
        Self::reduce(whole, BigInt::one())
    }

    /// Encode an exact posting at the supplied currency scale without rounding.
    ///
    /// # Errors
    /// Returns a money error for a fractional posting increment or final overflow,
    /// or [`ExactError::ArithmeticLimit`] if the fraction exceeds its budget.
    pub fn into_posted_exact(self, currency: CurrencySpec) -> Result<PostedMoney, ExactError> {
        self.check_budget()?;
        let scaled = product(&self.numerator, &ten_pow(u32::from(currency.scale())))?;
        if !(&scaled % &self.denominator).is_zero() {
            return Err(MoneyError::InvalidPostingIncrement.into());
        }
        posted(scaled / &self.denominator, currency)
    }

    /// Render exact major-unit evidence at a known posting grid without imposing
    /// the bounded `PostedMoney` coefficient limit on an intermediate.
    ///
    /// # Errors
    /// Rejects invalid scale, fractional increments and the exact arithmetic budget.
    pub fn canonical_at_scale(&self, scale: u8) -> Result<String, ExactError> {
        if scale > 28 {
            return Err(MoneyError::ScaleOutOfRange.into());
        }
        self.check_budget()?;
        let scaled = product(&self.numerator, &ten_pow(u32::from(scale)))?;
        if !(&scaled % &self.denominator).is_zero() {
            return Err(MoneyError::InvalidPostingIncrement.into());
        }
        let coefficient = scaled / &self.denominator;
        let digits = coefficient.abs().to_str_radix(10);
        if coefficient.is_zero() {
            return Ok("0".to_owned());
        }
        let sign = if coefficient.is_negative() { "-" } else { "" };
        if scale == 0 {
            return Ok(format!("{sign}{digits}"));
        }
        let scale = usize::from(scale);
        let padded = if digits.len() <= scale {
            format!("{}{}", "0".repeat(scale + 1 - digits.len()), digits)
        } else {
            digits
        };
        let (whole, fraction) = padded.split_at(padded.len() - scale);
        let fraction = fraction.trim_end_matches('0');
        if fraction.is_empty() {
            Ok(format!("{sign}{whole}"))
        } else {
            Ok(format!("{sign}{whole}.{fraction}"))
        }
    }

    /// Round once at the requested currency scale, with ties going to an even coefficient.
    ///
    /// # Errors
    /// Returns a money error for final overflow, or [`ExactError::ArithmeticLimit`]
    /// if the fraction exceeds its budget.
    pub fn round_half_even(self, currency: CurrencySpec) -> Result<PostedMoney, ExactError> {
        self.round_half_even_exact(currency.scale())?
            .into_posted_exact(currency)
    }

    /// Round to a posting increment while retaining an exact, unbounded money intermediate.
    /// Used when a later anchor residual may bring the rounded amount back into range.
    ///
    /// # Errors
    /// Returns a named scale or arithmetic-budget error. Final money bounds are checked
    /// only when the caller converts the completed posting with `into_posted_exact`.
    pub fn round_half_even_exact(self, scale: u8) -> Result<Self, ExactError> {
        if scale > 28 {
            return Err(MoneyError::ScaleOutOfRange.into());
        }
        self.check_budget()?;
        let scaled = product(&self.numerator.abs(), &ten_pow(u32::from(scale)))?;
        let mut quotient = &scaled / &self.denominator;
        let remainder = scaled % &self.denominator;
        let twice_remainder = product(&remainder, &BigInt::from(2))?;
        if twice_remainder > self.denominator
            || (twice_remainder == self.denominator && (&quotient % 2_u8) != BigInt::zero())
        {
            quotient += 1;
        }
        if self.numerator.is_negative() {
            quotient = -quotient;
        }
        Self::reduce(quotient, ten_pow(u32::from(scale)))
    }

    /// Add or subtract cross-products, whose sum can have at most 513 digits.
    fn combine(&self, rhs: &Self, subtract: bool) -> Result<Self, ExactError> {
        self.check_operands(rhs)?;
        let left = product(&self.numerator, &rhs.denominator)?;
        let right = product(&rhs.numerator, &self.denominator)?;
        let numerator = if subtract { left - right } else { left + right };
        check_digits(&numerator, &SUM_BOUND)?;
        Self::reduce(numerator, product(&self.denominator, &rhs.denominator)?)
    }

    /// Validate reduced inputs before allocating operation scratch.
    fn check_operands(&self, rhs: &Self) -> Result<(), ExactError> {
        self.check_budget()?;
        rhs.check_budget()
    }

    /// Enforce the reduced fraction's final resource budget.
    fn check_budget(&self) -> Result<(), ExactError> {
        check_digits(&self.numerator, &FINAL_BOUND)?;
        check_digits(&self.denominator, &FINAL_BOUND)
    }

    /// Validate scratch, normalize signs and common factors, then check final bounds.
    fn reduce(numerator: BigInt, denominator: BigInt) -> Result<Self, ExactError> {
        if denominator.is_zero() {
            return Err(ExactError::DivisionByZero);
        }
        check_digits(&numerator, &SUM_BOUND)?;
        check_digits(&denominator, &PRODUCT_BOUND)?;
        let result = Self::normalize(numerator, denominator);
        result.check_budget()?;
        Ok(result)
    }

    /// Reduce known bounded integers with a nonzero denominator using Euclid's algorithm.
    fn normalize(mut numerator: BigInt, mut denominator: BigInt) -> Self {
        if numerator.is_zero() {
            return Self {
                numerator: BigInt::zero(),
                denominator: BigInt::one(),
            };
        }
        if denominator.is_negative() {
            numerator = -numerator;
            denominator = -denominator;
        }
        let divisor = gcd(&numerator, &denominator);
        Self {
            numerator: numerator / &divisor,
            denominator: denominator / divisor,
        }
    }
}

impl PartialEq for ExactAmount {
    /// Exact value equality, defined through [`Ord::cmp`] so `==` and the order
    /// cannot disagree.
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for ExactAmount {}

impl PartialOrd for ExactAmount {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ExactAmount {
    /// Exact value order. Denominators are positive, so comparing the cross
    /// products is exact whether or not the fractions are reduced; `Eq` is
    /// defined through it. The products of two in-budget fractions are bounded
    /// scratch, not a result.
    fn cmp(&self, other: &Self) -> Ordering {
        (&self.numerator * &other.denominator).cmp(&(&other.numerator * &self.denominator))
    }
}

impl std::fmt::Display for ExactAmount {
    /// Decimal text when the fraction terminates within 28 places, otherwise
    /// `numerator/denominator`; for messages only.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for scale in 0..=28_u8 {
            if let Ok(text) = self.canonical_at_scale(scale) {
                return f.write_str(&text);
            }
        }
        write!(f, "{}/{}", self.numerator, self.denominator)
    }
}

/// The integer coefficient of `amount` at `scale` (`12.3` at scale 2 is `1230`),
/// or `None` when `amount` is finer than `scale` or the coefficient overflows
/// `i128`. Trailing zeros of the decimal form are not finer digits (`1.50` at
/// scale 1 is `15`). The one rule for the `i128` fast path of [`ScaledSum`].
#[must_use]
pub fn coefficient_at_scale(amount: Decimal, scale: u8) -> Option<i128> {
    let amount = amount.normalize();
    let shift = u32::from(scale).checked_sub(amount.scale())?;
    amount.mantissa().checked_mul(10_i128.checked_pow(shift)?)
}

/// An exact running sum at one posting scale.
///
/// Terms are added as integer coefficients at that scale with checked `i128`
/// arithmetic, so a sum of postings costs no fraction work per term. The sum
/// moves to an exact [`ExactAmount`] only when a term is finer than the scale,
/// or a coefficient or the running total overflows `i128`; its value never
/// depends on the form that holds it, and a total the `i128` form cannot hold
/// is still narrowed exactly by [`Self::to_posted`].
#[derive(Clone, Debug)]
pub struct ScaledSum {
    scale: u8,
    form: SumForm,
}

/// The two exact forms of a [`ScaledSum`].
#[derive(Clone, Debug)]
enum SumForm {
    /// The value is `coefficient × 10^-scale`.
    Coefficient(i128),
    /// The exact value once the coefficient form could not hold it.
    Fraction(ExactAmount),
}

impl ScaledSum {
    /// An exact zero at `scale`.
    #[must_use]
    pub fn zero(scale: u8) -> Self {
        Self {
            scale,
            form: SumForm::Coefficient(0),
        }
    }

    /// An exact value held at `scale`.
    #[must_use]
    pub fn from_exact(value: ExactAmount, scale: u8) -> Self {
        Self {
            scale,
            form: SumForm::Fraction(value),
        }
    }

    /// The posting scale the coefficient form counts in.
    #[must_use]
    pub fn scale(&self) -> u8 {
        self.scale
    }

    /// Add `amount` exactly.
    ///
    /// # Errors
    /// [`ExactError::ArithmeticLimit`] when the exact budget is exceeded; the sum
    /// is unchanged then.
    pub fn add_decimal(&mut self, amount: Decimal) -> Result<(), ExactError> {
        if let SumForm::Coefficient(total) = &mut self.form
            && let Some(next) =
                coefficient_at_scale(amount, self.scale).and_then(|term| total.checked_add(term))
        {
            *total = next;
            return Ok(());
        }
        let next = self
            .to_exact()
            .checked_add(&ExactAmount::from_decimal(amount))?;
        self.form = SumForm::Fraction(next);
        Ok(())
    }

    /// Add another sum exactly (at any scale; the coefficient fast path needs
    /// both at the same scale).
    ///
    /// # Errors
    /// [`ExactError::ArithmeticLimit`] when the exact budget is exceeded; the sum
    /// is unchanged then.
    pub fn add_sum(&mut self, other: &Self) -> Result<(), ExactError> {
        if self.scale == other.scale
            && let (SumForm::Coefficient(total), SumForm::Coefficient(term)) =
                (&mut self.form, &other.form)
            && let Some(next) = total.checked_add(*term)
        {
            *total = next;
            return Ok(());
        }
        let next = self.to_exact().checked_add(&other.to_exact())?;
        self.form = SumForm::Fraction(next);
        Ok(())
    }

    /// The exact value.
    #[must_use]
    pub fn to_exact(&self) -> ExactAmount {
        match &self.form {
            SumForm::Coefficient(coefficient) => {
                ExactAmount::from_coefficient(*coefficient, self.scale)
            }
            SumForm::Fraction(value) => value.clone(),
        }
    }

    /// Whether the exact value is zero.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        match &self.form {
            SumForm::Coefficient(coefficient) => *coefficient == 0,
            SumForm::Fraction(value) => value.numerator.is_zero(),
        }
    }

    /// Narrow the sum to a posting in `currency` without rounding: the decimal
    /// carrier directly when the coefficient fits it, else through the exact
    /// value, which normalizes (`8` at scale 28 has a coefficient past the
    /// carrier but is a valid posting).
    ///
    /// # Errors
    /// As [`ExactAmount::into_posted_exact`]: a money error for a fractional
    /// increment or a total outside the posting contract.
    pub fn to_posted(&self, currency: CurrencySpec) -> Result<PostedMoney, ExactError> {
        if let SumForm::Coefficient(coefficient) = self.form
            && let Ok(amount) =
                Decimal::try_from_i128_with_scale(coefficient, u32::from(self.scale))
        {
            return Ok(PostedMoney::try_new(amount, currency)?);
        }
        self.to_exact().into_posted_exact(currency)
    }
}

impl PartialEq for ScaledSum {
    /// Exact value equality, whatever form or scale holds each side.
    fn eq(&self, other: &Self) -> bool {
        match (&self.form, &other.form) {
            (SumForm::Coefficient(left), SumForm::Coefficient(right))
                if self.scale == other.scale =>
            {
                left == right
            }
            _ => self.to_exact() == other.to_exact(),
        }
    }
}

impl Eq for ScaledSum {}

/// Sum matching postings exactly and validate only the final decimal amount.
///
/// Takes the postings by reference (a slice, a `Vec` by reference, or any
/// cloneable iterator of `&PostedMoney`) and the currency owned or borrowed, so
/// no caller clones postings to sum them. Every term's code and stored scale
/// is checked against `currency` before any addition.
///
/// # Errors
/// Returns a money error for mismatched currency/scale or final overflow,
/// or [`ExactError::ArithmeticLimit`] when the exact arithmetic budget is exceeded.
pub fn sum_posted<'a, I>(
    values: I,
    currency: impl Borrow<CurrencySpec>,
) -> Result<PostedMoney, ExactError>
where
    I: IntoIterator<Item = &'a PostedMoney>,
    I::IntoIter: Clone,
{
    let currency = currency.borrow();
    let values = values.into_iter();
    // Validate all metadata before any addition.
    for value in values.clone() {
        value.currency().ensure_same(currency)?;
    }
    let mut sum = ExactAmount::from_decimal(Decimal::ZERO);
    for value in values {
        sum = sum.checked_add(&ExactAmount::from_decimal(value.amount()))?;
    }
    sum.into_posted_exact(currency.clone())
}

/// Return the exact positive encoding factor for a decimal scale.
fn ten_pow(scale: u32) -> BigInt {
    BigInt::from(10).pow(scale)
}

/// `10^digits`, the smallest magnitude with `digits + 1` decimal digits.
fn digit_bound(digits: u32) -> BigUint {
    BigUint::from(10_u8).pow(digits)
}

/// Check absolute decimal digits against a [`digit_bound`], counting zero as one
/// digit.
fn check_digits(value: &BigInt, bound: &BigUint) -> Result<(), ExactError> {
    if value.magnitude() >= bound {
        return Err(ExactError::ArithmeticLimit);
    }
    Ok(())
}

/// The greatest common divisor of the magnitudes, by Euclid's algorithm; `gcd(0, d)` is
/// `|d|`.
fn gcd(left: &BigInt, right: &BigInt) -> BigInt {
    let mut divisor = left.abs();
    let mut remainder = right.abs();
    while !remainder.is_zero() {
        let next = &divisor % &remainder;
        divisor = remainder;
        remainder = next;
    }
    divisor
}

/// Parse ASCII decimal digits the caller has already validated.
fn parse_digits(text: &str) -> Result<BigInt, ExactError> {
    BigInt::parse_bytes(text.as_bytes(), 10).ok_or_else(|| MoneyError::InvalidDecimal.into())
}

/// A canonical integer string: plain decimal syntax without a point, at most 256 digits.
fn canonical_integer(text: &str) -> Result<BigInt, ExactError> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.len() > INTEGER_TEXT_DIGITS {
        // Without leading zeros the digit count is the magnitude's, so an integer this long is
        // past the budget and any other text is malformed: one byte scan tells them apart.
        let integer = !digits.starts_with('0') && digits.bytes().all(|byte| byte.is_ascii_digit());
        return Err(if integer {
            ExactError::ArithmeticLimit
        } else {
            MoneyError::InvalidDecimal.into()
        });
    }
    if text.contains('.') {
        return Err(MoneyError::InvalidDecimal.into());
    }
    let parts = split_decimal(text)?;
    let magnitude = parse_digits(parts.integer)?;
    Ok(if parts.negative {
        -magnitude
    } else {
        magnitude
    })
}

/// Multiply bounded operands and enforce the 512-digit scratch bound.
fn product(left: &BigInt, right: &BigInt) -> Result<BigInt, ExactError> {
    let value = left * right;
    check_digits(&value, &PRODUCT_BOUND)?;
    Ok(value)
}

/// Normalize an encoding coefficient before constructing the bounded decimal carrier.
fn posted(mut coefficient: BigInt, currency: CurrencySpec) -> Result<PostedMoney, ExactError> {
    let mut scale = u32::from(currency.scale());
    while scale > 0 && (&coefficient % 10_u8).is_zero() {
        coefficient /= 10_u8;
        scale -= 1;
    }
    if coefficient.abs() >= ten_pow(28) {
        return Err(MoneyError::AmountOutOfRange.into());
    }
    let coefficient = coefficient.to_i128().ok_or(MoneyError::AmountOutOfRange)?;
    Ok(PostedMoney::try_new(
        Decimal::from_i128_with_scale(coefficient, scale),
        currency,
    )?)
}

#[cfg(test)]
#[path = "exact_tests.rs"]
mod exact_tests;
