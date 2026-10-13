//! Exact decimal amounts and half-open tier arithmetic.
//!
//! Every decimal of a price is plain decimal text. The authoring decode reads each one with its
//! field's limit (`bss_pricing_sdk::numeric`) and normalizes it (`0.10` becomes `0.1`). The
//! stored decode also requires the text to be canonical, so storage holds one spelling of each
//! value. Serialization always writes canonical text.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-tier-bands-half-open:p1
use super::{RuleError, price_book_entry::Model};
use bss_money::exact::{ExactAmount, ExactError};
use bss_pricing_sdk::numeric::{
    self, NumericError, QUANTITY_FRACTIONAL_DIGITS, RATE_FRACTIONAL_DIGITS,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One band of a tiered price. It is written as JSON, never read from it alone: [`decode`] and
/// [`decode_stored`] read it with its price.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Tier {
    #[serde(serialize_with = "bss_money::serde_text::option::serialize")]
    pub up_to: Option<Decimal>,
    #[serde(serialize_with = "bss_money::serde_text::serialize")]
    pub rate: Decimal,
}

/// Untagged storage content: the model is a separate column. It has no `Deserialize`: the only
/// way in from JSON is [`decode`] or [`decode_stored`], which check each decimal first.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum PriceData {
    Flat {
        #[serde(serialize_with = "bss_money::serde_text::serialize")]
        amount: Decimal,
    },
    PerUnit {
        #[serde(serialize_with = "bss_money::serde_text::serialize")]
        rate: Decimal,
    },
    Package {
        #[serde(serialize_with = "bss_money::serde_text::serialize")]
        package_size: Decimal,
        #[serde(serialize_with = "bss_money::serde_text::serialize")]
        package_price: Decimal,
    },
    Tiers {
        tiers: Vec<Tier>,
    },
}
/// The shape Serde reads a price body into, once [`canonical_price`] has checked each decimal.
/// Private, so no reader can skip that check.
#[toolkit_macros::domain_model]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TierWire {
    up_to: Option<Decimal>,
    rate: Decimal,
}
/// The untagged price body as Serde reads it; see [`TierWire`].
#[toolkit_macros::domain_model]
#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum PriceWire {
    Flat {
        amount: Decimal,
    },
    PerUnit {
        rate: Decimal,
    },
    Package {
        package_size: Decimal,
        package_price: Decimal,
    },
    Tiers {
        tiers: Vec<TierWire>,
    },
}
impl From<PriceWire> for PriceData {
    fn from(wire: PriceWire) -> Self {
        match wire {
            PriceWire::Flat { amount } => Self::Flat { amount },
            PriceWire::PerUnit { rate } => Self::PerUnit { rate },
            PriceWire::Package {
                package_size,
                package_price,
            } => Self::Package {
                package_size,
                package_price,
            },
            PriceWire::Tiers { tiers } => Self::Tiers {
                tiers: tiers
                    .into_iter()
                    .map(|t| Tier {
                        up_to: t.up_to,
                        rate: t.rate,
                    })
                    .collect(),
            },
        }
    }
}
/// Every decimal of a price is exact text: serde reads a JSON number through f64 and would
/// round it silently, so any number anywhere in the price is refused.
fn has_number(value: &Value) -> bool {
    match value {
        Value::Number(_) => true,
        Value::Array(items) => items.iter().any(has_number),
        Value::Object(fields) => fields.values().any(has_number),
        _ => false,
    }
}
/// What a client is told when it sent a decimal as a JSON number.
pub const DECIMALS_ARE_STRINGS: &str =
    "decimals are JSON strings (\"0.10\"), never JSON numbers, so no digit is lost to f64";
/// What a client is told when a decimal is not plain decimal text.
pub const PLAIN_DECIMAL_TEXT: &str = "decimals are plain decimal text such as \"0.10\": digits \
     with at most one point, and no exponent, + sign, spaces, separators or leading zeros";

/// A decimal field of a price, and the limit it follows.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceField {
    /// `amount` of a flat price: a rate.
    Amount,
    /// `rate` of a per-unit price or of a tier: a rate.
    Rate,
    /// `package_price` of a package price: a rate.
    PackagePrice,
    /// `package_size` of a package price: a positive quantity.
    PackageSize,
    /// `up_to` of a tier: a threshold.
    UpTo,
}
/// The decimal fields of a price body, in the order they are judged.
const PRICE_FIELDS: [PriceField; 4] = [
    PriceField::Amount,
    PriceField::Rate,
    PriceField::PackageSize,
    PriceField::PackagePrice,
];
/// The decimal fields of one tier, in the order they are judged.
const TIER_FIELDS: [PriceField; 2] = [PriceField::UpTo, PriceField::Rate];
impl PriceField {
    /// The field's JSON name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Amount => "amount",
            Self::Rate => "rate",
            Self::PackagePrice => "package_price",
            Self::PackageSize => "package_size",
            Self::UpTo => "up_to",
        }
    }
    /// The most fractional places the field takes.
    #[must_use]
    pub const fn places(self) -> u32 {
        match self {
            Self::Amount | Self::Rate | Self::PackagePrice => RATE_FRACTIONAL_DIGITS,
            Self::PackageSize | Self::UpTo => QUANTITY_FRACTIONAL_DIGITS,
        }
    }
    /// Read the field's text with its limit.
    fn parse(self, text: &str) -> Result<Decimal, NumericError> {
        match self {
            Self::Amount | Self::Rate | Self::PackagePrice => numeric::parse_rate(text),
            Self::PackageSize => numeric::parse_positive_quantity(text),
            Self::UpTo => numeric::parse_threshold(text),
        }
    }
    /// Check a value the caller already holds against the field's limit.
    fn check(self, value: Decimal) -> Result<Decimal, NumericError> {
        match self {
            Self::Amount | Self::Rate | Self::PackagePrice => numeric::validate_rate(value),
            Self::PackageSize => numeric::validate_positive_quantity(value),
            Self::UpTo => numeric::validate_threshold(value),
        }
    }
    /// The refusal of a value the caller already holds, if any.
    fn refusal(self, value: Decimal) -> Option<RuleError> {
        self.check(value)
            .err()
            .map(|error| RuleError::new(numeric_code(error)))
    }
}

/// The rule code of a refused decimal: `AMOUNT_OUT_OF_RANGE` over a limit, else
/// `AMOUNT_INVALID` (invalid text, a negative value, or a zero package size). The client's
/// message names the field.
const fn numeric_code(error: NumericError) -> &'static str {
    match error {
        NumericError::OutOfRange => "AMOUNT_OUT_OF_RANGE",
        NumericError::InvalidMinimumFee => "MIN_FEE_INVALID",
        NumericError::InvalidText | NumericError::Negative | NumericError::NotPositive => {
            "AMOUNT_INVALID"
        }
    }
}

/// Why a price body was not read.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// A decimal was sent as a JSON number.
    #[error("decimal sent as a JSON number")]
    JsonNumber,
    /// A decimal string that its field's limit refuses.
    #[error("{}: {error}", field.name())]
    Numeric {
        /// The refused field.
        field: PriceField,
        /// What the limit refused.
        error: NumericError,
    },
    /// A stored decimal that is not its canonical text. Only [`decode_stored`] returns it.
    #[error("{} is not canonical decimal text", field.name())]
    NotCanonical {
        /// The field that is not canonical.
        field: PriceField,
    },
    /// The JSON does not have the model's shape, or has extra fields.
    #[error("price is not in the shape of its model")]
    Shape,
}
impl DecodeError {
    /// The stable rule code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::JsonNumber | Self::NotCanonical { .. } => "AMOUNT_INVALID",
            Self::Numeric { error, .. } => numeric_code(error),
            Self::Shape => "PRICE_MISSING",
        }
    }
    /// What a client is told: a JSON number is told to send a string, invalid text to send plain
    /// decimal text, a value over its limit gets the limit, and a sign error names the field.
    #[must_use]
    pub fn message(self) -> String {
        match self {
            Self::JsonNumber => DECIMALS_ARE_STRINGS.into(),
            Self::Numeric {
                error: NumericError::InvalidText,
                ..
            } => PLAIN_DECIMAL_TEXT.into(),
            Self::Numeric {
                field,
                error: NumericError::OutOfRange,
            } => format!(
                "{} has at most {} fractional places and 28 digits in all",
                field.name(),
                field.places()
            ),
            Self::Numeric {
                field,
                error: NumericError::Negative,
            } => format!("{} must not be negative", field.name()),
            Self::Numeric {
                field,
                error: NumericError::NotPositive,
            } => format!("{} must be above zero", field.name()),
            Self::Numeric {
                error: NumericError::InvalidMinimumFee,
                ..
            }
            | Self::NotCanonical { .. }
            | Self::Shape => self.code().into(),
        }
    }
}
impl From<DecodeError> for RuleError {
    fn from(error: DecodeError) -> Self {
        Self::new(error.code())
    }
}
/// Read each decimal of `fields` named in `allowed` with its limit, and write it back as
/// canonical text. With `stored`, text that is not already canonical is refused.
fn canonical_fields(
    fields: &mut Map<String, Value>,
    allowed: &[PriceField],
    stored: bool,
) -> Result<(), DecodeError> {
    for &field in allowed {
        if let Some(Value::String(text)) = fields.get_mut(field.name()) {
            let value = field
                .parse(text)
                .map_err(|error| DecodeError::Numeric { field, error })?;
            let canonical = bss_money::canonical_decimal(value);
            if stored && canonical != *text {
                return Err(DecodeError::NotCanonical { field });
            }
            *text = canonical;
        }
    }
    Ok(())
}
/// [`canonical_fields`] over a price body and each of its tiers. Anything that is not where a
/// decimal belongs is left for Serde to refuse as a shape error.
fn canonical_price(value: &mut Value, stored: bool) -> Result<(), DecodeError> {
    let Value::Object(fields) = value else {
        return Ok(());
    };
    canonical_fields(fields, &PRICE_FIELDS, stored)?;
    if let Some(Value::Array(tiers)) = fields.get_mut("tiers") {
        for tier in tiers {
            if let Value::Object(tier) = tier {
                canonical_fields(tier, &TIER_FIELDS, stored)?;
            }
        }
    }
    Ok(())
}
/// Read a price body: no JSON number, each decimal within its limit, the model's shape.
fn read(model: Model, mut value: Value, stored: bool) -> Result<PriceData, DecodeError> {
    if has_number(&value) {
        return Err(DecodeError::JsonNumber);
    }
    canonical_price(&mut value, stored)?;
    let data: PriceData = serde_json::from_value::<PriceWire>(value)
        .map_err(|_| DecodeError::Shape)?
        .into();
    if shape_matches(model, &data) {
        Ok(data)
    } else {
        Err(DecodeError::Shape)
    }
}
/// Decode an authored price in the shape declared by the model. Each decimal is normalized:
/// `"0.10"` is read as `0.1`.
/// # Errors
/// [`DecodeError::JsonNumber`] for a decimal sent as a JSON number; [`DecodeError::Numeric`]
/// for text that is not plain decimal text or breaks its field's limit;
/// [`DecodeError::Shape`] for malformed JSON, extra fields or a model/shape mismatch.
pub fn decode(model: Model, value: Value) -> Result<PriceData, DecodeError> {
    read(model, value, false)
}
/// Decode a stored price: as [`decode`], and each decimal must be stored as its canonical text
/// (`"0.1"`, never `"0.10"`).
/// # Errors
/// Every refusal of [`decode`], and [`DecodeError::NotCanonical`] for another spelling.
pub fn decode_stored(model: Model, value: Value) -> Result<PriceData, DecodeError> {
    read(model, value, true)
}
/// Read a stored minimum fee: canonical decimal text (`30`, never `30.00`) that is not negative.
/// `None` means the stored text is corrupt.
#[must_use]
pub fn stored_fee(text: &str) -> Option<Decimal> {
    bss_money::parse_decimal(text)
        .ok()
        .filter(|fee| *fee >= Decimal::ZERO && bss_money::canonical_decimal(*fee) == text)
}
fn shape_matches(model: Model, data: &PriceData) -> bool {
    matches!(
        (model, data),
        (Model::Flat, PriceData::Flat { .. })
            | (Model::PerUnit, PriceData::PerUnit { .. })
            | (Model::Package, PriceData::Package { .. })
            | (Model::Graduated | Model::Volume, PriceData::Tiers { .. })
    )
}
/// Validate rates, thresholds and ascending bounds with one open top band.
#[must_use]
pub fn validate_tiers(tiers: &[Tier]) -> Vec<RuleError> {
    let mut errors = Vec::new();
    if tiers.is_empty() {
        return vec![RuleError::new("TIER_BAND_EMPTY")];
    }
    let mut previous = Decimal::ZERO;
    for (index, tier) in tiers.iter().enumerate() {
        errors.extend(PriceField::Rate.refusal(tier.rate));
        errors.extend(tier.up_to.and_then(|top| PriceField::UpTo.refusal(top)));
        if index + 1 == tiers.len() {
            if tier.up_to.is_some() {
                errors.push(RuleError::new("TIER_TOP_CLOSED"));
            }
        } else if let Some(top) = tier.up_to.filter(|top| *top > previous) {
            previous = top;
        } else {
            errors.push(RuleError::new("TIER_BANDS_ORDER"));
        }
    }
    errors
}
/// Validate prices without computing a period minimum or consumer allowances. Each decimal
/// follows its field's limit (`bss_pricing_sdk::numeric`).
#[must_use]
pub fn validate(model: Model, data: &PriceData) -> Vec<RuleError> {
    if !shape_matches(model, data) {
        return vec![RuleError::new("PRICE_MISSING")];
    }
    match data {
        PriceData::Flat { amount } => PriceField::Amount.refusal(*amount).into_iter().collect(),
        PriceData::PerUnit { rate } => PriceField::Rate.refusal(*rate).into_iter().collect(),
        PriceData::Package {
            package_size,
            package_price,
        } => PriceField::PackageSize
            .refusal(*package_size)
            .or_else(|| PriceField::PackagePrice.refusal(*package_price))
            .into_iter()
            .collect(),
        PriceData::Tiers { tiers } => validate_tiers(tiers),
    }
}
/// The exact value of a price decimal.
fn exact_decimal(value: Decimal) -> ExactAmount {
    ExactAmount::from_decimal(value)
}
/// A failed exact step as a rule refusal. Valid prices keep every step far inside the exact
/// budget, so a valid price never reaches this.
fn exact_step(result: Result<ExactAmount, ExactError>) -> Result<ExactAmount, RuleError> {
    result.map_err(|error| match error {
        ExactError::ArithmeticLimit => RuleError::new("AMOUNT_OUT_OF_RANGE"),
        ExactError::Money(_)
        | ExactError::DivisionByZero
        | ExactError::EmptyWeights
        | ExactError::NegativeWeight
        | ExactError::TooManyTerms => RuleError::new("AMOUNT_INVALID"),
    })
}
/// Evaluate one quantity in major currency units. No allowance, proration or floor.
///
/// The result is exact: nothing is rounded and nothing is narrowed to a `Decimal`. A flat,
/// per-unit, volume or graduated price only adds, subtracts and multiplies. A package price
/// counts whole packages, `ceil(quantity / package_size)`, then multiplies that count by the
/// package price. The count is a number of packages, not money.
///
/// The bounds all fit the 256-digit budget of [`ExactAmount`]:
/// - an input (a quantity, a threshold or a package size) is below 10^28 and has at most 28
///   places;
/// - a product is below 10^56 and has at most 56 places;
/// - a package count is at most 10^56;
/// - a package amount is below 10^84.
///
/// A rate has at most 12 places, so an amount has at most 40. A `quantity` is any `Decimal`
/// that is not negative; one above 10^28 also fits.
/// # Errors
/// Invalid model content and negative quantities are refused.
pub fn amount_for(
    model: Model,
    data: &PriceData,
    quantity: Decimal,
) -> Result<ExactAmount, RuleError> {
    if let Some(error) = validate(model, data).first() {
        return Err(*error);
    }
    if quantity < Decimal::ZERO {
        return Err(RuleError::new("AMOUNT_INVALID"));
    }
    let units = exact_decimal(quantity);
    match data {
        PriceData::Flat { amount } => Ok(exact_decimal(*amount)),
        PriceData::PerUnit { rate } => exact_step(units.checked_mul(&exact_decimal(*rate))),
        PriceData::Package {
            package_size,
            package_price,
        } => {
            let packages = exact_step(units.checked_div(&exact_decimal(*package_size)))?;
            let packages = exact_step(packages.ceil())?;
            exact_step(packages.checked_mul(&exact_decimal(*package_price)))
        }
        PriceData::Tiers { tiers } if model == Model::Volume => {
            let band = tiers
                .iter()
                .find(|tier| tier.up_to.is_none_or(|top| quantity < top))
                .ok_or_else(|| RuleError::new("TIER_TOP_CLOSED"))?;
            exact_step(units.checked_mul(&exact_decimal(band.rate)))
        }
        PriceData::Tiers { tiers } => {
            let mut total = exact_decimal(Decimal::ZERO);
            let mut lower = Decimal::ZERO;
            for tier in tiers {
                let upper = tier.up_to.unwrap_or(quantity).min(quantity);
                if upper > lower {
                    let width =
                        exact_step(exact_decimal(upper).checked_sub(&exact_decimal(lower)))?;
                    let charge = exact_step(width.checked_mul(&exact_decimal(tier.rate)))?;
                    total = exact_step(total.checked_add(&charge))?;
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
#[path = "money_tests.rs"]
mod tests;
