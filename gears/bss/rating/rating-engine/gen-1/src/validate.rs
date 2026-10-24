//! Presence and consistency of an input (`InputValidator`, rating DESIGN 01 §3.2; feature 02 §2.1
//! step 2).
//!
//! Every check here guards a property the steps rely on, and fails by name before any step runs:
//! instants are whole UTC seconds; the window lies in the billing period and the served extent in
//! it; a recurring period is one calendar month or year as its price says; the slices tile the
//! served window in order with non-negative quantities, and a recurring slice carries the
//! committed quantity; the resolves are one chain — the period start, then each `ends_on` of the
//! binding before — whose handovers fall on slice boundaries; every cell, pin, currency and
//! decimal is well formed. Generation 1 fails closed on what it does not rate yet: one-time
//! children (R-19), committed-quantity changes and paused suspensions.

use rating_gen1_money::{CurrencySpec, validate_amount};
use rust_decimal::Decimal;
use time::{Month, OffsetDateTime, Time};
use uuid::Uuid;

use crate::error::EvaluationError;
use crate::input::{
    ChildKind, CoreBinding, CoreCell, CorePriceModel, CoreResolve, DerivedExpr, EvaluationInput,
    FactKind, PricePin, RecurringPeriod, SuspensionPosture,
};
use crate::meter;

/// The cell the child rates in every resolve: its item, the one dimension value the fact's
/// accepted binding names for it (the null value if it names none) and that pin.
pub struct RatedCell<'a> {
    pub item: Uuid,
    pub value: Option<&'a String>,
    pub accepted: Option<&'a PricePin>,
}

impl RatedCell<'_> {
    /// This cell in `resolve`, if the resolve answers it.
    pub fn in_resolve<'r>(&self, resolve: &'r CoreResolve) -> Option<&'r CoreCell> {
        resolve.cells.iter().find(|cell| {
            cell.selection.item_id == self.item
                && cell.selection.dimension_value.as_ref() == self.value
        })
    }
}

/// Checks `input` and returns the cell it rates.
///
/// # Errors
///
/// The [`EvaluationError`] of the first check that fails.
pub fn input(input: &EvaluationInput) -> Result<RatedCell<'_>, EvaluationError> {
    instants(input)?;
    geometry(input)?;
    kinds(input)?;
    let rated = rated_cell(input)?;
    slices(input)?;
    pricing(input, &rated)?;
    Ok(rated)
}

/// Every instant is a whole UTC second (DESIGN 09 §4.1); the pipeline quarantines anything else.
fn instants(input: &EvaluationInput) -> Result<(), EvaluationError> {
    let window = &input.child.window;
    let fact = &input.fact;
    let mut instants = vec![
        window.window_start,
        window.window_end,
        window.served_from,
        window.served_to,
        fact.period.start,
        fact.period.end,
        fact.served.start,
        fact.served.end,
    ];
    for interval in &fact.suspensions {
        instants.extend([interval.start, interval.end]);
    }
    for slice in &input.quantities.slices {
        instants.extend([slice.slice_start, slice.slice_end]);
        instants.extend(slice.granules.iter().map(|granule| granule.granule_start));
    }
    match instants
        .iter()
        .find(|instant| instant.nanosecond() != 0 || !instant.offset().is_utc())
    {
        None => Ok(()),
        Some(instant) => Err(geometry_error(format!(
            "{instant} is not a whole UTC second"
        ))),
    }
}

/// The window lies in the billing period and its served part in the fact's served extent; a
/// recurring child's window is the whole billing period.
fn geometry(input: &EvaluationInput) -> Result<(), EvaluationError> {
    let window = &input.child.window;
    let fact = &input.fact;
    let ordered = window.window_start < window.window_end
        && window.window_start <= window.served_from
        && window.served_from < window.served_to
        && window.served_to <= window.window_end
        && fact.period.start < fact.period.end
        && fact.period.start <= fact.served.start
        && fact.served.start < fact.served.end
        && fact.served.end <= fact.period.end;
    if !ordered {
        return Err(geometry_error(
            "the window, served extent or billing period is empty or out of order".into(),
        ));
    }
    let inside = fact.period.start <= window.window_start
        && window.window_end <= fact.period.end
        && fact.served.start <= window.served_from
        && window.served_to <= fact.served.end;
    if !inside {
        return Err(geometry_error(
            "the window leaves the billing period, or its served part the fact's served extent"
                .into(),
        ));
    }
    let whole_period =
        window.window_start == fact.period.start && window.window_end == fact.period.end;
    if input.child.kind == ChildKind::PeriodLine && !whole_period {
        return Err(geometry_error(
            "a recurring child's window is its whole billing period".into(),
        ));
    }
    Ok(())
}

/// The child, fact and declaration agree, and the child is one this build rates.
fn kinds(input: &EvaluationInput) -> Result<(), EvaluationError> {
    let fact = &input.fact;
    let expected = match input.child.kind {
        ChildKind::UsageWindow => FactKind::Usage,
        ChildKind::PeriodLine => FactKind::Recurring,
        ChildKind::OneTime => FactKind::OneTime,
    };
    if fact.kind != expected {
        return Err(unsupported(format!(
            "a {:?} child of a {:?} fact",
            input.child.kind, fact.kind
        )));
    }
    match (input.child.kind, &input.derived) {
        (ChildKind::OneTime, _) => {
            return Err(unsupported(
                "one-time facts are stored, not rated, until R-19 is accepted".into(),
            ));
        }
        (ChildKind::UsageWindow, None) => {
            return Err(unsupported(
                "a usage child reached the core without its derived declaration".into(),
            ));
        }
        (ChildKind::UsageWindow, Some(frozen)) => {
            meter::declaration(frozen)?;
        }
        (ChildKind::PeriodLine, Some(_)) => {
            return Err(unsupported(
                "a derived usage declaration on a recurring child".into(),
            ));
        }
        (ChildKind::PeriodLine, None) => {}
    }
    if input.child.kind == ChildKind::PeriodLine {
        if fact.suspension_posture == SuspensionPosture::PauseRecurring
            && !fact.suspensions.is_empty()
        {
            return Err(unsupported(
                "proration over the un-suspended part of a period is not in this build of \
                 generation 1"
                    .into(),
            ));
        }
        if !input.subscription.quantity_intervals.is_empty() {
            return Err(unsupported(
                "slicing a recurring line at committed-quantity changes is not in this build of \
                 generation 1"
                    .into(),
            ));
        }
    }
    Ok(())
}

/// The one dimension value and price the fact's accepted binding names for the child's item.
fn rated_cell(input: &EvaluationInput) -> Result<RatedCell<'_>, EvaluationError> {
    let item = input.child.aggregation_key.item_id;
    let mut pins: Vec<&PricePin> = input
        .fact
        .accepted
        .iter()
        .filter(|pin| pin.item_id == item)
        .collect();
    pins.sort_by(|left, right| {
        (&left.dimension_value, left.price_id).cmp(&(&right.dimension_value, right.price_id))
    });
    pins.dedup();
    if pins
        .windows(2)
        .any(|pair| pair[0].dimension_value == pair[1].dimension_value)
    {
        return Err(EvaluationError::BindingMismatch {
            detail: format!("the accepted binding names two prices for one value of item {item}"),
        });
    }
    if pins.len() > 1 {
        return Err(unsupported(format!(
            "the accepted binding names {} dimension values for item {item}; one per item and \
             line until R-16",
            pins.len()
        )));
    }
    let accepted = pins.first().copied();
    Ok(RatedCell {
        item,
        value: accepted.and_then(|pin| pin.dimension_value.as_ref()),
        accepted,
    })
}

/// The slices tile part of the served window in order, no quantity is negative, and a recurring
/// slice carries the committed quantity.
fn slices(input: &EvaluationInput) -> Result<(), EvaluationError> {
    let window = &input.child.window;
    let slices = &input.quantities.slices;
    if slices.is_empty() {
        return Err(layout("the child has no slice".into()));
    }
    let mut previous_end = window.served_from;
    for slice in slices {
        if slice.slice_start < previous_end
            || slice.slice_start >= slice.slice_end
            || slice.slice_end > window.served_to
        {
            return Err(layout(format!(
                "slice [{}, {}) is empty, overlaps its predecessor or leaves the served window",
                slice.slice_start, slice.slice_end
            )));
        }
        previous_end = slice.slice_end;
        let negative = slice.q.is_negative()
            || slice
                .granules
                .iter()
                .flat_map(|granule| &granule.inputs)
                .any(|input| input.q.is_negative());
        if negative {
            return Err(EvaluationError::NegativeWindowQuantity {
                detail: format!(
                    "slice [{}, {}) has a quantity below zero",
                    slice.slice_start, slice.slice_end
                ),
            });
        }
        if input.child.kind == ChildKind::PeriodLine
            && let Some(committed) = &input.fact.quantity
            && slice.q != *committed
        {
            return Err(layout(format!(
                "slice [{}, {}) carries quantity {}, not the committed {committed}",
                slice.slice_start, slice.slice_end, slice.q
            )));
        }
    }
    Ok(())
}

/// The resolves are one chain whose handovers fall on slice boundaries, and every cell, currency
/// and decimal of them is well formed.
fn pricing(input: &EvaluationInput, rated: &RatedCell<'_>) -> Result<(), EvaluationError> {
    let resolves = &input.pricing.resolves;
    let Some(first) = resolves.first() else {
        return Err(EvaluationError::PriceUncovered {
            detail: "the input carries no resolve".into(),
        });
    };
    if first.resolve_date != input.fact.period.start.date() {
        return Err(layout(format!(
            "the first resolve is of {}, not of the period start {}",
            first.resolve_date,
            input.fact.period.start.date()
        )));
    }
    for pair in resolves.windows(2) {
        handover(input, rated, &pair[0], &pair[1])?;
    }
    let mut code: Option<&str> = None;
    for resolve in resolves {
        cells(resolve)?;
        for binding in resolve
            .cells
            .iter()
            .filter_map(|cell| cell.binding.as_ref())
        {
            well_formed(binding)?;
            if binding.item_id != rated.item {
                continue;
            }
            let this = binding.price.currency.code.as_str();
            match code {
                Some(first) if first != this => {
                    return Err(EvaluationError::BindingCurrencyMismatch {
                        detail: format!(
                            "the child's bindings carry {first} and {this}; never converted"
                        ),
                    });
                }
                _ => code = Some(this),
            }
            if input.child.kind == ChildKind::PeriodLine {
                recurring_period(input, binding)?;
            }
        }
    }
    derived_decimals(input)
}

/// A later resolve takes over exactly at the `ends_on` of the rated cell's binding in the resolve
/// before (pricing D-425), inside the period, and never inside a slice: until `split_points`
/// lands, the slices must already be cut there.
fn handover(
    input: &EvaluationInput,
    rated: &RatedCell<'_>,
    before: &CoreResolve,
    after: &CoreResolve,
) -> Result<(), EvaluationError> {
    let ends_on = rated
        .in_resolve(before)
        .and_then(|cell| cell.binding.as_ref())
        .and_then(|binding| binding.price.ends_on);
    if ends_on != Some(after.resolve_date) || after.resolve_date <= before.resolve_date {
        return Err(layout(format!(
            "the resolve of {} is not at the ends_on of the binding the resolve of {} gives \
             ({ends_on:?})",
            after.resolve_date, before.resolve_date
        )));
    }
    let at = OffsetDateTime::new_utc(after.resolve_date, Time::MIDNIGHT);
    if at <= input.fact.period.start || at >= input.fact.period.end {
        return Err(layout(format!(
            "the handover on {} is outside the billing period",
            after.resolve_date
        )));
    }
    match input
        .quantities
        .slices
        .iter()
        .find(|slice| slice.slice_start < at && at < slice.slice_end)
    {
        None => Ok(()),
        Some(slice) => Err(layout(format!(
            "slice [{}, {}) crosses the handover on {}",
            slice.slice_start, slice.slice_end, after.resolve_date
        ))),
    }
}

/// A resolve answers each item and dimension value once.
fn cells(resolve: &CoreResolve) -> Result<(), EvaluationError> {
    for (index, cell) in resolve.cells.iter().enumerate() {
        if resolve.cells[..index]
            .iter()
            .any(|other| other.selection == cell.selection)
        {
            return Err(EvaluationError::NonInjectiveMapping {
                detail: format!(
                    "the resolve of {} answers item {} value {:?} twice",
                    resolve.resolve_date, cell.selection.item_id, cell.selection.dimension_value
                ),
            });
        }
    }
    Ok(())
}

/// The currency is one `bss_money` accepts, and every decimal fits the canonical text bound.
fn well_formed(binding: &CoreBinding) -> Result<(), EvaluationError> {
    let currency = &binding.price.currency;
    if let Err(error) = CurrencySpec::try_new(currency.code.clone(), currency.scale) {
        return Err(EvaluationError::BindingCurrencyMismatch {
            detail: format!(
                "price {} carries currency {:?} at scale {}: {error}",
                binding.price.price_id, currency.code, currency.scale
            ),
        });
    }
    let mut decimals: Vec<Decimal> = match &binding.price.model {
        CorePriceModel::Flat { amount } => vec![*amount],
        CorePriceModel::PerUnit { unit_amount } => vec![*unit_amount],
        CorePriceModel::Volume { tiers } | CorePriceModel::Graduated { tiers } => tiers
            .iter()
            .flat_map(|tier| tier.up_to.into_iter().chain([tier.rate]))
            .collect(),
        CorePriceModel::Package {
            package_size,
            package_price,
        } => vec![*package_size, *package_price],
    };
    decimals.extend(binding.price.minimum_fee);
    decimals
        .into_iter()
        .try_for_each(|value| decimal(value, binding.price.price_id))
}

/// A recurring price's period matches the fact's billing period (DESIGN 09 §4.1): the next
/// calendar month or year at the same time of day. The anchor rule is Subscriptions'
/// `BillingTerms`, which the input does not carry, so a month may be 28 to 31 days.
fn recurring_period(input: &EvaluationInput, binding: &CoreBinding) -> Result<(), EvaluationError> {
    const DAY: i64 = 86_400;
    let (start, end) = (input.fact.period.start, input.fact.period.end);
    let seconds = end.unix_timestamp() - start.unix_timestamp();
    let matches = start.time() == end.time()
        && match binding.recurring_period {
            Some(RecurringPeriod::Month) => {
                let (year, month) = if start.month() == Month::December {
                    (start.year() + 1, Month::January)
                } else {
                    (start.year(), start.month().next())
                };
                (end.year(), end.month()) == (year, month)
                    && (28 * DAY..=31 * DAY).contains(&seconds)
            }
            Some(RecurringPeriod::Year) => {
                end.year() == start.year() + 1
                    && end.month() == start.month()
                    && (365 * DAY..=366 * DAY).contains(&seconds)
            }
            None => false,
        };
    if matches {
        return Ok(());
    }
    Err(geometry_error(format!(
        "price {} is {:?} but the billing period is [{start}, {end})",
        binding.price.price_id, binding.recurring_period
    )))
}

/// The constants of a derived declaration fit the canonical text bound.
fn derived_decimals(input: &EvaluationInput) -> Result<(), EvaluationError> {
    fn walk(expr: &DerivedExpr, found: &mut Vec<Decimal>) {
        match expr {
            DerivedExpr::Input { .. } => {}
            DerivedExpr::Const { value } => found.push(*value),
            DerivedExpr::Add { left, right }
            | DerivedExpr::Sub { left, right }
            | DerivedExpr::Mul { left, right } => {
                walk(left, found);
                walk(right, found);
            }
            DerivedExpr::DivConst { operand, divisor } => {
                found.push(*divisor);
                walk(operand, found);
            }
            DerivedExpr::Max { operands } | DerivedExpr::Min { operands } => {
                for operand in operands {
                    walk(operand, found);
                }
            }
            DerivedExpr::Ceil { operand }
            | DerivedExpr::Floor { operand }
            | DerivedExpr::Round { operand, .. } => walk(operand, found),
        }
    }
    let Some(frozen) = &input.derived else {
        return Ok(());
    };
    let mut found = Vec::new();
    walk(&frozen.declaration.formula, &mut found);
    found.into_iter().try_for_each(|value| {
        validate_amount(value)
            .map(|_| ())
            .map_err(|_| EvaluationError::PrecisionOverflow {
                detail: format!(
                    "a constant of derived usage type {}@{} has more than 28 digits",
                    frozen.code, frozen.version
                ),
            })
    })
}

fn decimal(value: Decimal, price_id: Uuid) -> Result<(), EvaluationError> {
    validate_amount(value)
        .map(|_| ())
        .map_err(|_| EvaluationError::PrecisionOverflow {
            detail: format!("a decimal of price {price_id} has more than 28 digits"),
        })
}

fn geometry_error(detail: String) -> EvaluationError {
    EvaluationError::PeriodGeometryMismatch { detail }
}

fn layout(detail: String) -> EvaluationError {
    EvaluationError::LayoutMismatch { detail }
}

fn unsupported(detail: String) -> EvaluationError {
    EvaluationError::UnsupportedPrimitive { detail }
}

#[cfg(test)]
#[path = "validate_tests.rs"]
mod tests;
