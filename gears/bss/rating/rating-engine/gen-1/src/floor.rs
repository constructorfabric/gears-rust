//! The minimum-fee floor function roll-up applies (rating DESIGN 09 §4.2; T-D-38, T-D-78; feature
//! 02 §3.4 step 4).
//!
//! Step 4 of the fixed order runs at parent roll-up, over the [`PriceTotal`]s of every child of a
//! fact, because a price's floor spans the fact's children (hourly windows, slices before and
//! after an `ends_on`). Per price and stored scale:
//!
//! - `rated` = the exact sum of every slice and dimension value rated by the price;
//! - `covered_fraction` = the covered UTC seconds of the slices bound to the price / the billing
//!   period's UTC seconds (T-D-77), so an `ends_on` handover floors each price by its own share,
//!   never both by the whole (V52);
//! - `floor = minimum_fee × covered_fraction`, and the top-up is `floor − rated` when positive.
//!
//! A price whose slices carry two stored scales is floored per price and scale, each part by its
//! own covered fraction, so nothing is converted (V41). Where the top-up goes — the
//! `min_fee_topup` line of the rated `line_key`, one per key, a provenance entry per price — is
//! roll-up's (feature 09). Nothing is rounded: Billing rounds the invoice line.

use std::collections::BTreeMap;

use uuid::Uuid;

use crate::budget::ArithmeticBudget;
use crate::error::EvaluationError;
use crate::exact::{CoveredFraction, ExactMoney};
use crate::geometry;
use crate::input::Interval;
use crate::outcome::{FloorPart, PriceTotal};

/// What one price at one scale rated across a fact's children.
struct PriceSum {
    rated: ExactMoney,
    covered: Vec<Interval>,
    period_seconds: u64,
    minimum_fee: Option<ExactMoney>,
}

// @cpt-begin:cpt-cf-bss-rating-algo-core-geometry:p1:inst-geo-obligation
/// The floor of every price that carries a minimum fee, per price and stored scale, from the
/// price totals of all children of one fact, in price and scale order.
///
/// # Errors
///
/// `binding_currency_mismatch` when one price's totals carry two currencies,
/// `period_geometry_mismatch` when they name two billing periods or cover more than the period,
/// `binding_mismatch` when they carry two minimum fees, `money_contract_violation` for an amount
/// that is not canonical text, `precision_overflow` beyond the bounds.
pub fn minimum_fee_floor(totals: &[PriceTotal]) -> Result<Vec<FloorPart>, EvaluationError> {
    let mut prices: BTreeMap<(Uuid, u8), PriceSum> = BTreeMap::new();
    for total in totals {
        let rated = ExactMoney::from_wire(&total.rated)?;
        let minimum_fee = total
            .minimum_fee
            .as_ref()
            .map(ExactMoney::from_wire)
            .transpose()?;
        if rated.currency().scale != total.currency_scale {
            return Err(EvaluationError::MoneyContractViolation {
                detail: format!(
                    "the total of price {} is tagged with scale {} but carries scale {}",
                    total.price_id,
                    total.currency_scale,
                    rated.currency().scale
                ),
            });
        }
        if let Some(interval) = total
            .covered
            .iter()
            .find(|interval| interval.start >= interval.end)
        {
            return Err(EvaluationError::PeriodGeometryMismatch {
                detail: format!(
                    "the total of price {} covers the empty or reversed interval [{}, {})",
                    total.price_id, interval.start, interval.end
                ),
            });
        }
        let key = (total.price_id, total.currency_scale);
        let merged = match prices.remove(&key) {
            None => PriceSum {
                rated,
                covered: geometry::union(total.covered.iter().copied()),
                period_seconds: total.period_seconds,
                minimum_fee,
            },
            Some(earlier) => merge(total.price_id, &earlier, &rated, total, minimum_fee)?,
        };
        prices.insert(key, merged);
    }
    prices
        .into_iter()
        .filter_map(|((price_id, scale), rated)| {
            rated
                .minimum_fee
                .clone()
                .map(|fee| part(price_id, scale, &rated, &fee))
        })
        .collect()
}
// @cpt-end:cpt-cf-bss-rating-algo-core-geometry:p1:inst-geo-obligation

/// What a floor adds to a rated amount: `floor − rated` when positive, else zero. The one rule
/// roll-up's floor and purchase evaluation share (T-D-78).
///
/// # Errors
///
/// `binding_currency_mismatch` for amounts of two currencies or scales.
pub fn shortfall(floor: &ExactMoney, rated: &ExactMoney) -> Result<ExactMoney, EvaluationError> {
    let difference = floor.checked_sub(rated, "minimum-fee top-up")?;
    if difference.is_negative() {
        return Ok(ExactMoney::zero(floor.currency()));
    }
    Ok(difference)
}

/// One more child's total of a price already seen.
fn merge(
    price_id: Uuid,
    earlier: &PriceSum,
    rated: &ExactMoney,
    total: &PriceTotal,
    minimum_fee: Option<ExactMoney>,
) -> Result<PriceSum, EvaluationError> {
    if earlier.period_seconds != total.period_seconds {
        return Err(EvaluationError::PeriodGeometryMismatch {
            detail: format!(
                "the totals of price {price_id} name billing periods of {} and {} seconds",
                earlier.period_seconds, total.period_seconds
            ),
        });
    }
    if earlier.minimum_fee != minimum_fee {
        return Err(EvaluationError::BindingMismatch {
            detail: format!("the totals of price {price_id} carry two minimum fees"),
        });
    }
    Ok(PriceSum {
        rated: earlier.rated.checked_add(rated, "minimum-fee rated sum")?,
        covered: geometry::union(earlier.covered.iter().chain(&total.covered).copied()),
        period_seconds: earlier.period_seconds,
        minimum_fee,
    })
}

/// The floor and top-up of one price at one scale.
fn part(
    price_id: Uuid,
    currency_scale: u8,
    rated: &PriceSum,
    fee: &ExactMoney,
) -> Result<FloorPart, EvaluationError> {
    let covered_seconds = geometry::total_seconds(&rated.covered);
    // The covered time lies in one billing period, so it spans no more than the period.
    let span = match (rated.covered.first(), rated.covered.last()) {
        (Some(first), Some(last)) => geometry::total_seconds(&[Interval {
            start: first.start,
            end: last.end,
        }]),
        _ => 0,
    };
    if span > rated.period_seconds {
        return Err(EvaluationError::PeriodGeometryMismatch {
            detail: format!(
                "price {price_id} covers time spanning {span} seconds of a {}-second period",
                rated.period_seconds
            ),
        });
    }
    if covered_seconds > rated.period_seconds {
        return Err(EvaluationError::PeriodGeometryMismatch {
            detail: format!(
                "price {price_id} covers {covered_seconds} of a {}-second period",
                rated.period_seconds
            ),
        });
    }
    let floor = fee.times_fraction(
        CoveredFraction {
            covered_seconds,
            period_seconds: rated.period_seconds,
        },
        ArithmeticBudget::GEN_1,
        "minimum-fee floor",
    )?;
    let topup = shortfall(&floor, &rated.rated)?;
    Ok(FloorPart {
        price_id,
        currency_scale,
        minimum_fee: fee.to_wire("minimum fee")?,
        covered_seconds,
        period_seconds: rated.period_seconds,
        rated: rated.rated.to_wire("minimum-fee rated sum")?,
        floor: floor.to_wire("minimum-fee floor")?,
        topup: topup.to_wire("minimum-fee top-up")?,
    })
}

#[cfg(test)]
#[path = "floor_tests.rs"]
mod tests;
