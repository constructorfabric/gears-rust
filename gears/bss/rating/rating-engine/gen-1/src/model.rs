//! Step 3 of the fixed order: the binding's model formula over exact fractions (rating DESIGN
//! 03 §4.1, §4.3; T-D-74, T-D-80).
//!
//! The formulas are Pricing's `amount_for`, evaluated over exact fractions:
//!
//! - `flat` → `amount`; `per_unit` → `unit_amount × q`;
//! - `graduated` → the slice's span `[offset, offset + q)` priced band by band, each part at its
//!   band's rate;
//! - `volume` → `q × rate` of the band that holds the selecting total (the window total, or the
//!   window group's total), the first band with `total < up_to`;
//! - `package` → `ceil((offset + q) / size) − ceil(offset / size)` blocks at `package_price`, so
//!   the straddling block belongs to the slice that opened it.
//!
//! Bands are half-open `[from, up_to)` and the last band is open: a quantity exactly at `up_to` is
//! in the next band. Each amount is a reduced fraction in major units tagged with the price's
//! currency and stored scale; nothing is scaled by `10^currency_scale` or rounded. The package
//! block `ceil` is declared business rounding of a quantity, never of money.

// @cpt-algo:cpt-cf-bss-rating-algo-core-model:p1

use bss_money::exact::ExactAmount;
use rust_decimal::Decimal;

use crate::budget::ArithmeticBudget;
use crate::error::EvaluationError;
use crate::exact::{ExactMoney, ExactQuantity};
use crate::input::{CoreCurrency, CorePriceModel, CoreTier};
use crate::outcome::{BandPlacement, BandSegment, ModelKind};

/// The amount a model gives for a slice, and the quantities it priced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Priced {
    pub kind: ModelKind,
    pub amount: ExactMoney,
    /// The quantity the formula used; none for a flat price.
    pub quantity: Option<ExactQuantity>,
    /// The quantity billed: the package block count, else the quantity.
    pub billable_quantity: Option<ExactQuantity>,
    /// Where the slice sat on the bands of a tiered or package price.
    pub placement: Option<BandPlacement>,
}

/// Where a slice sits on its price's band axis (DESIGN 03 §4.3).
#[derive(Debug, Clone, Copy)]
pub struct Placement<'a> {
    /// The slice quantity.
    pub quantity: &'a ExactQuantity,
    /// The quantity of the band axis before the slice: the earlier slices of the child and the
    /// earlier members of its window group.
    pub offset: &'a ExactQuantity,
    /// The quantity whose band a volume price selects: the window or window-group total.
    pub selecting_total: &'a ExactQuantity,
}

/// One band of a tier table as exact bounds.
struct Band {
    lower: ExactAmount,
    upper: Option<ExactAmount>,
    rate: Decimal,
}

/// Prices one quantity alone: offset zero, the quantity its own volume total.
///
/// # Errors
///
/// As [`price_slice`].
pub fn price(
    model: &CorePriceModel,
    currency: &CoreCurrency,
    quantity: &ExactQuantity,
    budget: ArithmeticBudget,
) -> Result<Priced, EvaluationError> {
    let zero = ExactQuantity::zero();
    price_slice(
        model,
        currency,
        Placement {
            quantity,
            offset: &zero,
            selecting_total: quantity,
        },
        budget,
    )
}

// @cpt-begin:cpt-cf-bss-rating-algo-core-model:p1:inst-mod-slices
/// Prices one slice at its place on the band axis.
///
/// # Errors
///
/// `unsupported_primitive` for a malformed tier table or package size, `precision_overflow`
/// beyond the budget, `quantity_out_of_range` for a placed quantity beyond the bound.
pub fn price_slice(
    model: &CorePriceModel,
    currency: &CoreCurrency,
    placement: Placement<'_>,
    budget: ArithmeticBudget,
) -> Result<Priced, EvaluationError> {
    let quantity = placement.quantity;
    match model {
        CorePriceModel::Flat { amount } => Ok(Priced {
            kind: ModelKind::Flat,
            amount: ExactMoney::from_price(*amount, currency),
            quantity: None,
            billable_quantity: None,
            placement: None,
        }),
        CorePriceModel::PerUnit { unit_amount } => Ok(Priced {
            kind: ModelKind::PerUnit,
            amount: ExactMoney::from_price(*unit_amount, currency)
                .times_quantity(quantity, budget, "per_unit")?,
            quantity: Some(quantity.clone()),
            billable_quantity: Some(quantity.clone()),
            placement: None,
        }),
        CorePriceModel::Graduated { tiers } => graduated(tiers, currency, placement, budget),
        CorePriceModel::Volume { tiers } => volume(tiers, currency, placement, budget),
        CorePriceModel::Package {
            package_size,
            package_price,
        } => package(*package_size, *package_price, currency, placement, budget),
    }
}
// @cpt-end:cpt-cf-bss-rating-algo-core-model:p1:inst-mod-slices

/// `graduated`: the span `[offset, offset + q)` priced band by band (marginal).
fn graduated(
    tiers: &[CoreTier],
    currency: &CoreCurrency,
    placement: Placement<'_>,
    budget: ArithmeticBudget,
) -> Result<Priced, EvaluationError> {
    let start = placement.offset.to_exact()?;
    let end = add(&start, &placement.quantity.to_exact()?, "graduated span")?;
    let mut amount = ExactMoney::zero(currency);
    let mut segments = Vec::new();
    for (index, band) in bands(tiers)?.into_iter().enumerate() {
        let lower = start.clone().max(band.lower);
        let upper = band
            .upper
            .map_or_else(|| end.clone(), |up| up.min(end.clone()));
        if upper <= lower {
            continue;
        }
        let width = sub(&upper, &lower, "graduated band")?;
        let charge =
            ExactMoney::from_price(band.rate, currency).times(&width, budget, "graduated")?;
        amount = amount.checked_add(&charge, "graduated")?;
        segments.push(BandSegment {
            band: band_index(index),
            quantity: ExactQuantity::from_exact(&width, "graduated band")?,
            rate: band.rate,
        });
    }
    Ok(Priced {
        kind: ModelKind::Graduated,
        amount,
        quantity: Some(placement.quantity.clone()),
        billable_quantity: Some(placement.quantity.clone()),
        placement: Some(BandPlacement {
            offset: placement.offset.clone(),
            selecting_total: None,
            segments,
        }),
    })
}

/// `volume`: the slice quantity at the rate of the band that holds the selecting total.
fn volume(
    tiers: &[CoreTier],
    currency: &CoreCurrency,
    placement: Placement<'_>,
    budget: ArithmeticBudget,
) -> Result<Priced, EvaluationError> {
    let total = placement.selecting_total.to_exact()?;
    let (index, band) = bands(tiers)?
        .into_iter()
        .enumerate()
        .find(|(_, band)| band.upper.as_ref().is_none_or(|upper| total < *upper))
        .ok_or_else(|| malformed("the volume tiers have no band for the total"))?;
    Ok(Priced {
        kind: ModelKind::Volume,
        amount: ExactMoney::from_price(band.rate, currency).times_quantity(
            placement.quantity,
            budget,
            "volume",
        )?,
        quantity: Some(placement.quantity.clone()),
        billable_quantity: Some(placement.quantity.clone()),
        placement: Some(BandPlacement {
            offset: placement.offset.clone(),
            selecting_total: Some(placement.selecting_total.clone()),
            segments: vec![BandSegment {
                band: band_index(index),
                quantity: placement.quantity.clone(),
                rate: band.rate,
            }],
        }),
    })
}

/// `package`: the blocks the slice opens on the axis, at `package_price` each.
fn package(
    size: Decimal,
    block_price: Decimal,
    currency: &CoreCurrency,
    placement: Placement<'_>,
    budget: ArithmeticBudget,
) -> Result<Priced, EvaluationError> {
    if size <= Decimal::ZERO {
        return Err(malformed("the package size is not positive"));
    }
    let size = ExactAmount::from_decimal(size);
    let start = placement.offset.to_exact()?;
    let end = add(&start, &placement.quantity.to_exact()?, "package span")?;
    let blocks = sub(
        &blocks_through(&end, &size, budget)?,
        &blocks_through(&start, &size, budget)?,
        "package blocks",
    )?;
    Ok(Priced {
        kind: ModelKind::Package,
        amount: ExactMoney::from_price(block_price, currency).times(&blocks, budget, "package")?,
        quantity: Some(placement.quantity.clone()),
        billable_quantity: Some(ExactQuantity::from_exact(&blocks, "package blocks")?),
        placement: Some(BandPlacement {
            offset: placement.offset.clone(),
            selecting_total: None,
            segments: Vec::new(),
        }),
    })
}

/// `ceil(quantity / size)`: the blocks a band axis has opened up to `quantity`.
fn blocks_through(
    quantity: &ExactAmount,
    size: &ExactAmount,
    budget: ArithmeticBudget,
) -> Result<ExactAmount, EvaluationError> {
    budget.check_operands("package", quantity, size)?;
    quantity
        .checked_div(size)
        .and_then(|ratio| ratio.ceil())
        .map_err(|error| EvaluationError::from_money("package", &error))
}

/// The bands of a tier table, checked as Pricing's `validate_tiers` does: at least one band,
/// closed bands with strictly increasing positive `up_to`, and an open last band.
fn bands(tiers: &[CoreTier]) -> Result<Vec<Band>, EvaluationError> {
    if tiers.is_empty() {
        return Err(malformed("the tier table has no band"));
    }
    let mut lower = Decimal::ZERO;
    let mut bands = Vec::with_capacity(tiers.len());
    for (index, tier) in tiers.iter().enumerate() {
        let last = index + 1 == tiers.len();
        match (tier.up_to, last) {
            (None, true) => bands.push(Band {
                lower: ExactAmount::from_decimal(lower),
                upper: None,
                rate: tier.rate,
            }),
            (Some(up_to), false) if up_to > lower => {
                bands.push(Band {
                    lower: ExactAmount::from_decimal(lower),
                    upper: Some(ExactAmount::from_decimal(up_to)),
                    rate: tier.rate,
                });
                lower = up_to;
            }
            _ => {
                return Err(malformed(
                    "the tier table's bands are not increasing or its last band is not open",
                ));
            }
        }
    }
    Ok(bands)
}

/// A band's position in its table.
fn band_index(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

/// The exact sum of two quantities at `site`.
fn add(
    left: &ExactAmount,
    right: &ExactAmount,
    site: &str,
) -> Result<ExactAmount, EvaluationError> {
    left.checked_add(right)
        .map_err(|error| EvaluationError::from_money(site, &error))
}

/// The exact difference of two quantities at `site`.
fn sub(
    left: &ExactAmount,
    right: &ExactAmount,
    site: &str,
) -> Result<ExactAmount, EvaluationError> {
    left.checked_sub(right)
        .map_err(|error| EvaluationError::from_money(site, &error))
}

/// `unsupported_primitive` for a price Pricing would have refused.
fn malformed(detail: &str) -> EvaluationError {
    EvaluationError::UnsupportedPrimitive {
        detail: format!("{detail}; Pricing refuses such a price"),
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
