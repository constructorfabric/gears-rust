//! ISO 4217 money primitives: the validated [`CurrencyCode`], its minor-unit
//! scale, and the non-negative [`MinorAmount`].
//!
//! **This gear computes no charge.** It authors, validates and freezes price
//! rows; the arithmetic that turns a row into an amount owed lives in Tariffs
//! (evaluation) and Billing (invoicing). So there is deliberately **no add,
//! subtract, multiply, allocate or round here** — only the checks a publish
//! needs. A helper that added two `MinorAmount`s would invite exactly the
//! second implementation of charge arithmetic the design set puts downstream,
//! and the sibling ledger already owns the rounding and allocation idiom for
//! the money that is actually posted.
//!
//! Amounts are integer minor units (`amount_minor`), never a float and never a
//! decimal type. The per-currency **scale** is the ISO 4217 minor unit: there is
//! no flat two-decimal rule — JPY takes 0 and BHD takes 3 — which is why
//! `PRECISION_EXCEEDED` is declared a first-class publish rejection rather than
//! a rounding decision taken quietly at the boundary.
//!
//! # `PRECISION_EXCEEDED` is declared, and on the amount plane it is dormant
//!
//! Stated here because three design documents describe it as live enforcement —
//! `design/01-foundation.md` §3.3, `03-price-structure.md` §§ error sets,
//! `04-currency-tax.md` — and a reader checking whether
//! `cpt-cf-bss-pricing-fr-price-amount-validation` is satisfied would otherwise
//! be told "yes, by these" about functions nothing calls.
//!
//! **[`check_scale`], [`check_decimal`] and [`is_expressible`] have no caller in
//! this gear.** That is not an oversight and relaxing or deleting them would not
//! be the repair: the barrier is *representational*. Every amount reaches this
//! module as an `i64` count of minor units (`PriceContentView::amount_minor` →
//! [`MinorAmount::new`]), so no request can *declare* a scale for the rule to
//! compare, and a [`crate::domain::price_row::PriceRow`] cannot hold a violation
//! of it — the argument [`crate::domain::publish::rules`] makes for registering
//! no pipeline rule, and the one D-311 restates ("there is no check to relax,
//! and relaxing one would change nothing").
//!
//! **What owes the call is the first authoring surface that takes a decimal
//! literal**, in either scale: [`check_decimal`] for an amount, and
//! [`RateMinor::from_major_decimal`] — which carries the *rate* scale's own comparison
//! and not [`check_scale`]'s — for a rate. D-311 staged that surface and has not
//! built it: the rate wire spelling it chose is the integer `unitRateNanoMinor`,
//! read back through [`RateMinor::from_nano_minor`], so today
//! [`RateMinor::from_major_decimal`] is reached only from `domain::repricing_tests`. A
//! CSV or major-unit bulk import is the obvious first such surface, and the point
//! of recording this is that it will **not** reach the check by default.

use std::fmt;

use toolkit_macros::domain_model;

use crate::domain::error::DomainError;

/// The minor unit assumed for any code outside the two tables below.
///
/// Named rather than inlined so the fallback is a stated policy: an unlisted
/// code is treated as two-decimal, it is never rejected for being unlisted and
/// it never resolves to "unknown".
pub const DEFAULT_MINOR_UNIT: u32 = 2;

/// ISO 4217 codes with **no** minor unit.
///
/// A **launch subset**, deliberately: it carries the zero-decimal codes we are
/// confident about and nothing else, because a half-remembered table is worse
/// than a named fallback. Anything absent resolves to [`DEFAULT_MINOR_UNIT`].
/// The four-decimal fund codes (CLF, UYW) are absent on purpose — they are unit
/// of account codes, not currencies anything is sold in, and admitting one
/// would put a scale above 3 into the key.
const ZERO_DECIMAL_CODES: &[&str] = &[
    "BIF", "CLP", "DJF", "GNF", "ISK", "JPY", "KMF", "KRW", "PYG", "RWF", "UGX", "VND", "VUV",
    "XAF", "XOF", "XPF",
];

/// ISO 4217 codes with a **three**-decimal minor unit. Same launch-subset rule
/// as [`ZERO_DECIMAL_CODES`].
///
/// There is no one-decimal roster beside these two, and MGA and MRU are the
/// reason there could be: both subdivide by **five** rather than by ten, so no
/// power of ten is their minor unit. The register publishes them at exponent 2,
/// a fifth is exactly expressible at one decimal, and payment processors treat
/// at least MGA as zero-decimal — three defensible answers, which is precisely
/// the "half-remembered table" [`ZERO_DECIMAL_CODES`] refuses. They resolve to
/// [`DEFAULT_MINOR_UNIT`] until a tenant sells on one and the question is
/// answered rather than guessed.
const THREE_DECIMAL_CODES: &[&str] = &["BHD", "IQD", "JOD", "KWD", "LYD", "OMR", "TND"];

/// A validated ISO 4217 alpha-3 currency code, held uppercase.
///
/// Normalization is load-bearing, not cosmetic: `currency` is a **scope-key
/// axis**, so `usd` and `USD` must be one key or the duplicate-key index stops
/// seeing duplicates. The constructor therefore accepts either spelling and
/// stores exactly one of them.
///
/// Membership of the real ISO register is not checked here — that is a
/// taxonomy question the tenant currency configuration answers (Slice 4). What
/// this type guarantees is the *shape* every downstream axis comparison relies
/// on: three ASCII letters, uppercase.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CurrencyCode(String);

impl CurrencyCode {
    /// Validate and normalize an authored currency code.
    ///
    /// # Errors
    ///
    /// [`DomainError::CurrencyInvalid`] when `raw` is not three ASCII letters.
    pub fn new(raw: &str) -> Result<Self, DomainError> {
        let trimmed = raw.trim();
        if trimmed.len() != 3 || !trimmed.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Err(DomainError::CurrencyInvalid(raw.to_owned()));
        }
        Ok(Self(trimmed.to_ascii_uppercase()))
    }

    /// The normalized uppercase code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The ISO 4217 minor unit: the number of decimal places one unit of this
    /// currency is subdivided into, and therefore the maximum precision a price
    /// on it can express.
    #[must_use]
    pub fn minor_unit(&self) -> u32 {
        let code = self.0.as_str();
        if ZERO_DECIMAL_CODES.contains(&code) {
            0
        } else if THREE_DECIMAL_CODES.contains(&code) {
            3
        } else {
            DEFAULT_MINOR_UNIT
        }
    }
}

impl fmt::Display for CurrencyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An amount in integer **minor units**, validated non-negative.
///
/// Non-negativity is a checked property rather than an assumption: typed credit
/// rows are deliberately Future scope, so a negative price is an authoring
/// mistake and must be refused where it enters, not carried until some
/// downstream sum looks wrong.
///
/// The type deliberately carries **no currency**: the currency is a scope-key
/// axis, so pairing one into the amount would create a second place to disagree
/// about which currency a row is priced in.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MinorAmount(i64);

impl MinorAmount {
    /// Validate an authored minor-unit amount.
    ///
    /// # Errors
    ///
    /// [`DomainError::AmountNegative`] when `units` is below zero. Zero is
    /// valid — a free trial phase and a zero-rated usage row are both authored
    /// as `0`, not as an absent amount.
    pub fn new(units: i64) -> Result<Self, DomainError> {
        if units < 0 {
            return Err(DomainError::AmountNegative(units.to_string()));
        }
        Ok(Self(units))
    }

    /// The underlying minor-unit count.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

impl fmt::Display for MinorAmount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// The rate (D-311).
// ---------------------------------------------------------------------------

/// How many decimal places a rate carries **past** the currency's minor unit.
///
/// Nine, and the number is argued rather than picked. Six is not enough: AWS
/// Lambda's `$0.0000166667` per GB-second is `1666.67` micro-minor and stays
/// fractional. Nine expresses it exactly, and `i64` still reaches about
/// `$92,000,000` per unit — far past any rate. It matches the "nanos" convention
/// the estate's neighbours use.
///
/// It is **fixed** rather than carried per row on purpose: a stored scale is a
/// second field two rows could disagree about, and comparing two rates would
/// then mean normalising first — in a gear whose whole job is that two rows on
/// one key are comparable.
pub const RATE_SUB_DECIMALS: u32 = 9;

/// Minor units per stored unit, derived from [`RATE_SUB_DECIMALS`] rather than
/// written out, so the scale has exactly one place it can be changed.
const NANO_PER_MINOR: i128 = 10_i128.pow(RATE_SUB_DECIMALS);
const NANO_PER_MINOR_I64: i64 = 10_i64.pow(RATE_SUB_DECIMALS);

/// A **rate**: a multiplier, in 10⁻⁹ minor units (D-311).
///
/// # Why this is not [`MinorAmount`]
///
/// An amount reaches an invoice and cannot be finer than the currency's minor
/// unit — there is no way to bill `$0.015`. A rate is multiplied by a quantity,
/// and what rounds to a minor unit is the **product**. Metered pricing lives
/// below a cent as a matter of course: S3 at `$0.023` per GB-month, Lambda at
/// `$0.0000166667` per GB-second.
///
/// One type for both truncates: a `0.0150 / 0.0110` ladder and a
/// `0.0230 / 0.0120` ladder both collapse to `0.01` at every band, so two
/// different tariffs become the same tariff on rows that look well-formed.
///
/// This carries no arithmetic, for [`MinorAmount`]'s reason and more strongly:
/// multiplying a rate by a quantity **is** charge computation, and this gear
/// computes no charge.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RateMinor(i64);

impl RateMinor {
    /// The zero rate.
    ///
    /// A first-class value rather than a `from_nano_minor(0).expect(…)` at each
    /// site: zero is a rate — `inst-tb-first` says so in as many words ("a `$0`
    /// first band is valid") — and since D-45 the compiler *synthesises* one, so
    /// the `[0, N)` band it prepends has a name rather than a literal.
    pub const ZERO: Self = Self(0);

    /// Read a rate from a decimal literal in the currency's **major** units — the
    /// scale an authoring surface submits, and the one this type's other two
    /// constructors do not take.
    ///
    /// `from_major_decimal(usd, "0.01")`, [`RateMinor::from_minor_units(1)`] and
    /// [`RateMinor::from_nano_minor`]`(1_000_000_000)` are the same rate. Three
    /// constructors reading three scales is the shape a caller misreads by 100×,
    /// so the scale is in the name of each — this one included.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidRequest`] when the literal is malformed;
    /// [`DomainError::AmountNegative`] when it is below zero — zero itself is a
    /// rate, since a zero-rated usage row is authored rather than absent;
    /// [`DomainError::PrecisionExceeded`] when it declares more decimals than
    /// the stored scale can hold.
    ///
    /// **This is where `PRECISION_EXCEEDED` gets an operand — not yet a caller.**
    /// The code was a declared refusal nothing could raise, because `MinorAmount`
    /// could not hold a violation for it to reject. A rate *can* carry more digits
    /// than are stored, so the refusal is representable here. Two clauses of
    /// D-311's landing note are corrected rather than repeated, because both are
    /// checkable and both read as more than they are:
    ///
    /// - **This does not call [`check_scale`], and could not.** That predicate is
    ///   the *amount* rule — a scale above the currency's minor unit — and the
    ///   comparison below is the *rate* rule, against `minor_unit +
    ///   RATE_SUB_DECIMALS`. Two scales, two rules; folding them into one function
    ///   would give one name two meanings. [`check_scale`], [`check_decimal`] and
    ///   [`is_expressible`] therefore still have no caller at all.
    /// - **No production path reaches this constructor.** D-311's wire spelling is
    ///   the integer `unitRateNanoMinor`, read through
    ///   [`RateMinor::from_nano_minor`], so the only caller is
    ///   `domain::repricing_tests`. The operand exists; the surface that would
    ///   submit one does not. See the module doc for what is owed and by whom.
    pub fn from_major_decimal(currency: &CurrencyCode, literal: &str) -> Result<Self, DomainError> {
        let declared = fraction_digits(literal)?;
        let storable = currency.minor_unit() + RATE_SUB_DECIMALS;
        if declared > storable {
            return Err(DomainError::PrecisionExceeded(format!(
                "a rate on {currency} is stored to {storable} decimal(s), literal declares \
                 {declared}"
            )));
        }
        if literal.starts_with('-') {
            return Err(DomainError::AmountNegative(literal.to_owned()));
        }

        // Scaled by string rather than through a float: `0.0000166667` has no
        // exact binary representation, and a rate that changed in the ninth
        // decimal on its way through the parser is the defect this type exists
        // to prevent.
        let body = literal.split('.').collect::<Vec<_>>();
        let whole: i128 = body[0].parse().map_err(|_| {
            DomainError::InvalidRequest(format!("malformed decimal literal: {literal}"))
        })?;
        let mut nano = whole
            .checked_mul(10_i128.pow(currency.minor_unit()))
            .and_then(|minor| minor.checked_mul(NANO_PER_MINOR))
            .ok_or_else(|| DomainError::InvalidRequest(format!("rate out of range: {literal}")))?;
        if let Some(fraction) = body.get(1) {
            let digits: i128 = fraction.parse().map_err(|_| {
                DomainError::InvalidRequest(format!("malformed decimal literal: {literal}"))
            })?;
            let shift = storable - declared;
            let scaled = digits.checked_mul(10_i128.pow(shift)).ok_or_else(|| {
                DomainError::InvalidRequest(format!("rate out of range: {literal}"))
            })?;
            nano = nano.checked_add(scaled).ok_or_else(|| {
                DomainError::InvalidRequest(format!("rate out of range: {literal}"))
            })?;
        }
        let nano = i64::try_from(nano)
            .map_err(|_| DomainError::InvalidRequest(format!("rate out of range: {literal}")))?;
        Ok(Self(nano))
    }

    /// Wrap an already-scaled count in **nano-minor** units, as a stored row reads
    /// back — the storage scale, not an author's.
    ///
    /// # Errors
    ///
    /// [`DomainError::AmountNegative`] when below zero.
    pub fn from_nano_minor(nano: i64) -> Result<Self, DomainError> {
        if nano < 0 {
            return Err(DomainError::AmountNegative(nano.to_string()));
        }
        Ok(Self(nano))
    }

    /// A rate expressed in whole minor units — `1` meaning one cent per unit.
    ///
    /// The ordinary metered rate is sub-minor-unit, which is why this type
    /// exists; the whole-unit case is still a lawful rate, and stating it in the
    /// unit an author thinks in beats writing the nine zeroes out at each site.
    ///
    /// # Errors
    ///
    /// [`DomainError::AmountNegative`] when below zero, and
    /// [`DomainError::InvalidRequest`] when scaling would overflow `i64` — about
    /// 9.2 billion minor units, far beyond any authored rate.
    pub fn from_minor_units(minor_units: i64) -> Result<Self, DomainError> {
        let nano = minor_units.checked_mul(NANO_PER_MINOR_I64).ok_or_else(|| {
            DomainError::InvalidRequest(format!("rate out of range: {minor_units}"))
        })?;
        Self::from_nano_minor(nano)
    }

    /// The stored count, in 10⁻⁹ minor units.
    #[must_use]
    pub const fn nano_minor(self) -> i64 {
        self.0
    }
}

impl fmt::Display for RateMinor {
    /// The stored count, not a rendered decimal.
    ///
    /// Rendering needs the currency, which this type does not carry — the same
    /// arrangement [`MinorAmount`] has, and for the same reason: a `Display` that
    /// guessed a scale would put a second opinion about the currency's shape next
    /// to the one the row already holds.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Is an amount authored with `declared_scale` fraction digits expressible in
/// `currency`?
///
/// Crate-private, with [`check_scale`] and [`check_decimal`], for as long as the
/// cluster is dormant: every amount crossing the wire is `i64` minor units and no
/// surface accepts a decimal literal, so nothing outside this crate can supply a
/// declared scale. A `pub` predicate carrying `PRECISION_EXCEEDED` makes a grep
/// for the code read as enforcement the gear does not perform. D-311 stages the
/// authoring surface that gives them an operand; widening them again belongs to
/// that change, in the crate that gains the caller.
///
/// The predicate is over the **declared** precision, not over the value: a
/// price authored as `100.00` on JPY is not expressible, even though `100` is,
/// because the author has stated a two-decimal price on a currency that has no
/// decimals. Silently accepting it would mean the read model froze a scale the
/// currency cannot represent, and every consumer would have to re-derive which
/// digits were real.
#[must_use]
#[allow(
    dead_code,
    reason = "dormant until D-311's decimal authoring surface gives it a caller; see `is_expressible`"
)]
pub(crate) fn is_expressible(currency: &CurrencyCode, declared_scale: u32) -> bool {
    declared_scale <= currency.minor_unit()
}

/// The publish-time form of [`is_expressible`].
///
/// **Dormant: nothing calls this.** Kept rather than deleted because the code it
/// raises is declared by three design documents and an FR, and because the fault
/// becomes representable the moment an authoring surface takes a decimal literal —
/// which is the whole of what is owed. The module doc carries the argument; do not
/// read a caller into this signature without checking for one.
///
/// # Errors
///
/// [`DomainError::PrecisionExceeded`] when `declared_scale` is above the
/// currency's ISO 4217 minor unit.
#[allow(
    dead_code,
    reason = "dormant until D-311's decimal authoring surface gives it a caller; see `is_expressible`"
)]
pub(crate) fn check_scale(currency: &CurrencyCode, declared_scale: u32) -> Result<(), DomainError> {
    if is_expressible(currency, declared_scale) {
        return Ok(());
    }
    Err(DomainError::PrecisionExceeded(format!(
        "{currency} has {} decimal(s), amount declares {declared_scale}",
        currency.minor_unit()
    )))
}

/// The number of fraction digits an authored decimal literal declares.
///
/// This is the shape an authoring surface submits (`"1.50"`), so the scale
/// check has to be able to start from it rather than from an already-split
/// `(value, scale)` pair.
///
/// # Errors
///
/// [`DomainError::InvalidRequest`] when `literal` is not a decimal number: an
/// empty part, a second decimal point, or any non-digit outside the leading
/// sign. The sign itself is accepted here and judged by [`MinorAmount::new`] —
/// one rejection per problem, so an author is not told about precision when the
/// real fault is a negative price.
pub fn fraction_digits(literal: &str) -> Result<u32, DomainError> {
    let malformed = || DomainError::InvalidRequest(format!("malformed decimal literal: {literal}"));
    let body = literal.strip_prefix('-').unwrap_or(literal);
    let mut parts = body.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next();
    if parts.next().is_some() || whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return Err(malformed());
    }
    let Some(fraction) = fraction else {
        return Ok(0);
    };
    if fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err(malformed());
    }
    u32::try_from(fraction.len()).map_err(|_| malformed())
}

/// Is an authored decimal literal expressible in `currency`?
///
/// # Errors
///
/// [`DomainError::InvalidRequest`] when the literal is malformed;
/// [`DomainError::PrecisionExceeded`] when it declares more precision than the
/// currency's minor unit.
#[allow(
    dead_code,
    reason = "dormant until D-311's decimal authoring surface gives it a caller; see `is_expressible`"
)]
pub(crate) fn check_decimal(currency: &CurrencyCode, literal: &str) -> Result<(), DomainError> {
    check_scale(currency, fraction_digits(literal)?)
}

#[cfg(test)]
#[path = "money_tests.rs"]
mod money_tests;
