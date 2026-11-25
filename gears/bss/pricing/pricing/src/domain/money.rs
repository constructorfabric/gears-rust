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
use bss_pricing_sdk::amount::{self, AmountError, PriceViolation};
use bss_pricing_sdk::numeric::{
    self, NumericError, QUANTITY_FRACTIONAL_DIGITS, RATE_FRACTIONAL_DIGITS,
};
use bss_pricing_sdk::read::{self as contract, PriceModel};
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
/// Where the first JSON number sits in a price body, judged in the order the decimals are: the
/// body's own fields, then each tier's. A number under a key that is no decimal of a price is
/// `None`: the shape check refuses it, so the violation names the price.
fn number_place(value: &Value) -> Option<Place> {
    let Value::Object(fields) = value else {
        return None;
    };
    let at = |fields: &Map<String, Value>, allowed: &[PriceField], tier: Option<usize>| {
        allowed
            .iter()
            .copied()
            .find(|field| fields.get(field.name()).is_some_and(Value::is_number))
            .map(|field| Place { field, tier })
    };
    at(fields, &PRICE_FIELDS, None).or_else(|| {
        let Some(Value::Array(tiers)) = fields.get("tiers") else {
            return None;
        };
        tiers.iter().enumerate().find_map(|(i, tier)| match tier {
            Value::Object(tier) => at(tier, &TIER_FIELDS, Some(i)),
            _ => None,
        })
    })
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
}

/// Where a decimal sits in a price body: its field, and the index of the tier it is in, if any.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    /// The decimal's field.
    pub field: PriceField,
    /// The index of the tier the field is in; `None` for a field of the body itself.
    pub tier: Option<usize>,
}
impl Place {
    /// The place's path under the request's `price`, for a field violation (D-538):
    /// `price.rate`, `price.package_size`, `price.tiers[1].up_to`.
    #[must_use]
    pub fn path(self) -> String {
        match self.tier {
            Some(i) => format!("price.tiers[{i}].{}", self.field.name()),
            None => format!("price.{}", self.field.name()),
        }
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
    JsonNumber {
        /// Where the first number sits; `None` when it is under no decimal field of a price.
        at: Option<Place>,
    },
    /// A decimal string that its field's limit refuses.
    #[error("{}: {error}", at.field.name())]
    Numeric {
        /// The refused decimal.
        at: Place,
        /// What the limit refused.
        error: NumericError,
    },
    /// A stored decimal that is not its canonical text. Only [`decode_stored`] returns it.
    #[error("{} is not canonical decimal text", at.field.name())]
    NotCanonical {
        /// The decimal that is not canonical.
        at: Place,
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
            Self::JsonNumber { .. } | Self::NotCanonical { .. } => "AMOUNT_INVALID",
            Self::Numeric { error, .. } => numeric_code(error),
            Self::Shape => "PRICE_MISSING",
        }
    }
    /// The field a violation names (D-538): the refused decimal's path under `price`, and
    /// `price` itself when no one decimal is named (a shape error, or a JSON number under no
    /// decimal field).
    #[must_use]
    pub fn path(self) -> String {
        match self {
            Self::JsonNumber { at: Some(at) }
            | Self::Numeric { at, .. }
            | Self::NotCanonical { at } => at.path(),
            Self::JsonNumber { at: None } | Self::Shape => "price".to_owned(),
        }
    }
    /// What a client is told: a JSON number is told to send a string, invalid text to send plain
    /// decimal text, a value over its limit gets the limit, and a sign error names the field.
    #[must_use]
    pub fn message(self) -> String {
        match self {
            Self::JsonNumber { .. } => DECIMALS_ARE_STRINGS.into(),
            Self::Numeric {
                error: NumericError::InvalidText,
                ..
            } => PLAIN_DECIMAL_TEXT.into(),
            Self::Numeric {
                at,
                error: NumericError::OutOfRange,
            } => format!(
                "{} has at most {} fractional places and 28 digits in all",
                at.field.name(),
                at.field.places()
            ),
            Self::Numeric {
                at,
                error: NumericError::Negative,
            } => format!("{} must not be negative", at.field.name()),
            Self::Numeric {
                at,
                error: NumericError::NotPositive,
            } => format!("{} must be above zero", at.field.name()),
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
    tier: Option<usize>,
) -> Result<(), DecodeError> {
    for &field in allowed {
        if let Some(Value::String(text)) = fields.get_mut(field.name()) {
            let at = Place { field, tier };
            let value = field
                .parse(text)
                .map_err(|error| DecodeError::Numeric { at, error })?;
            let canonical = bss_money::canonical_decimal(value);
            if stored && canonical != *text {
                return Err(DecodeError::NotCanonical { at });
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
    canonical_fields(fields, &PRICE_FIELDS, stored, None)?;
    if let Some(Value::Array(tiers)) = fields.get_mut("tiers") {
        for (i, tier) in tiers.iter_mut().enumerate() {
            if let Value::Object(tier) = tier {
                canonical_fields(tier, &TIER_FIELDS, stored, Some(i))?;
            }
        }
    }
    Ok(())
}
/// Read a price body: no JSON number, each decimal within its limit, the model's shape.
fn read(model: Model, mut value: Value, stored: bool) -> Result<PriceData, DecodeError> {
    if has_number(&value) {
        return Err(DecodeError::JsonNumber {
            at: number_place(&value),
        });
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
/// The bands as the SDK reads them.
fn contract_tiers(tiers: &[Tier]) -> Vec<contract::Tier> {
    tiers
        .iter()
        .map(|tier| contract::Tier {
            up_to: tier.up_to,
            rate: tier.rate,
        })
        .collect()
}
/// The price as the SDK's [`PriceModel`]; `None` when the content is not in the shape of its
/// model.
#[must_use]
pub fn price_model(model: Model, data: &PriceData) -> Option<PriceModel> {
    Some(match (model, data) {
        (Model::Flat, PriceData::Flat { amount }) => PriceModel::Flat { amount: *amount },
        (Model::PerUnit, PriceData::PerUnit { rate }) => PriceModel::PerUnit { unit_amount: *rate },
        (
            Model::Package,
            PriceData::Package {
                package_size,
                package_price,
            },
        ) => PriceModel::Package {
            package_size: *package_size,
            package_price: *package_price,
        },
        (Model::Volume, PriceData::Tiers { tiers }) => PriceModel::Volume {
            tiers: contract_tiers(tiers),
        },
        (Model::Graduated, PriceData::Tiers { tiers }) => PriceModel::Graduated {
            tiers: contract_tiers(tiers),
        },
        _ => return None,
    })
}
impl From<PriceViolation> for RuleError {
    fn from(violation: PriceViolation) -> Self {
        Self::new(match violation {
            PriceViolation::Numeric(error) => numeric_code(error),
            PriceViolation::NoBands => "TIER_BAND_EMPTY",
            PriceViolation::TopBandClosed => "TIER_TOP_CLOSED",
            PriceViolation::BandsOutOfOrder => "TIER_BANDS_ORDER",
        })
    }
}
/// A failed exact step as a rule refusal. Valid prices keep every step far inside the exact
/// budget, so a valid price never reaches this.
const fn exact_code(error: &ExactError) -> &'static str {
    match error {
        ExactError::ArithmeticLimit => "AMOUNT_OUT_OF_RANGE",
        ExactError::Money(_)
        | ExactError::DivisionByZero
        | ExactError::EmptyWeights
        | ExactError::NegativeWeight
        | ExactError::TooManyTerms => "AMOUNT_INVALID",
    }
}
impl From<AmountError> for RuleError {
    fn from(error: AmountError) -> Self {
        match error {
            AmountError::Price(violation) => violation.into(),
            AmountError::NegativeQuantity => Self::new("AMOUNT_INVALID"),
            AmountError::Arithmetic(error) => Self::new(exact_code(&error)),
        }
    }
}
/// Validate rates, thresholds and ascending bounds with one open top band, by the SDK's band
/// rules (D-549), reporting every violation in band order.
#[must_use]
pub fn validate_tiers(tiers: &[Tier]) -> Vec<RuleError> {
    amount::tier_violations(&contract_tiers(tiers))
        .into_iter()
        .map(RuleError::from)
        .collect()
}
/// Validate prices without computing a period minimum or consumer allowances. Each decimal
/// follows its field's limit (`bss_pricing_sdk::numeric`); the content rules are the SDK's
/// (`bss_pricing_sdk::amount::price_violations`, D-549), and a shape the model does not take is
/// `PRICE_MISSING`.
#[must_use]
pub fn validate(model: Model, data: &PriceData) -> Vec<RuleError> {
    price_model(model, data).map_or_else(
        || vec![RuleError::new("PRICE_MISSING")],
        |price| {
            amount::price_violations(&price)
                .into_iter()
                .map(RuleError::from)
                .collect()
        },
    )
}
/// Evaluate one quantity in major currency units. No allowance, proration or floor.
///
/// The evaluation is the SDK's reference function, `bss_pricing_sdk::amount::amount_for`
/// (D-549), over this price as the SDK reads it; its doc gives the formulas and the exact bounds.
/// The result is exact: nothing is rounded and nothing is narrowed to a `Decimal`.
/// # Errors
/// Invalid model content and negative quantities are refused: a shape the model does not take is
/// `PRICE_MISSING`, a content rule its code, and a negative quantity `AMOUNT_INVALID`.
pub fn amount_for(
    model: Model,
    data: &PriceData,
    quantity: Decimal,
) -> Result<ExactAmount, RuleError> {
    let price = price_model(model, data).ok_or(RuleError::new("PRICE_MISSING"))?;
    amount::amount_for(&price, quantity).map_err(RuleError::from)
}
#[cfg(test)]
#[path = "money_tests.rs"]
mod tests;
