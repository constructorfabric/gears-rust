//! Presence and consistency of an input (`InputValidator`, rating DESIGN 01 §3.2; feature 02 §2.1
//! step 2).
//!
//! Every check here guards a property the steps rely on, and fails by name before any step runs:
//!
//! - **Instants and geometry.** Every instant is a whole UTC second; the served window lies in the
//!   fact's served extent and the billing period; a recurring child's window is the billing
//!   period, a usage child's is the window `window_geometry` gives for its binding's rating policy
//!   (T-D-75); suspensions and committed quantity intervals are ordered and disjoint.
//! - **Kinds.** The child, fact and declaration agree; one-time children wait for R-19.
//! - **Pricing.** The resolves are one chain — the period start, then each `ends_on` of the rated
//!   binding before, until no binding ends inside the served window (pricing D-425); every cell,
//!   pin, currency and decimal is well formed; a model is one its charge kind allows; a usage
//!   binding carries its meter and rating policy, the policy does not change inside the window,
//!   and a minimum fee sits only on a `BillingCycle`, subscription-line policy (T-D-78, R-34).
//! - **Meter.** A usage child's frozen declaration is its binding's derived usage type, and only
//!   `Sum` inputs are rated (R-06).
//! - **Layout.** The slices are exactly the cuts `split_points` gives (`layout_mismatch`
//!   otherwise, DESIGN 09 §3.3); no quantity is negative; a recurring slice carries the committed
//!   quantity in force over it; a usage slice's granules are UTC hours that touch the slice, in
//!   order; a window group's context agrees with the child it places (T-D-67).

use std::collections::BTreeSet;

use bss_money::{CurrencySpec, validate_amount};
use rust_decimal::Decimal;
use time::{Month, OffsetDateTime};
use uuid::Uuid;

use crate::error::EvaluationError;
use crate::exact::ExactQuantity;
use crate::geometry::{self, FactExtent, SplitInput};
use crate::input::{
    AggregationScope, ChargeKind, ChildKind, CoreBinding, CoreCell, CorePriceModel, CoreResolve,
    CoreUsagePolicy, DerivedExpr, EvaluationInput, FactKind, GroupMember, Interval,
    PartialWindowPolicy, PolicyScope, PricePin, RatingWindowKind, RecurringPeriod, ResetPolicy,
    SliceQuantity, WindowFold,
};
use crate::meter::DerivedMeterEvaluator;

/// The cell the child rates in every resolve: its item, the one dimension value the fact's
/// accepted binding names for it (the null value if it names none) and that pin.
pub struct RatedCell<'a> {
    /// The child's item.
    pub item: Uuid,
    /// The dimension value the accepted binding names, or none for the null value.
    pub value: Option<&'a str>,
    /// The accepted binding's pin for this cell, if the fact has one.
    pub accepted: Option<&'a PricePin>,
}

impl RatedCell<'_> {
    /// This cell in `resolve`, if the resolve answers it.
    pub fn in_resolve<'r>(&self, resolve: &'r CoreResolve) -> Option<&'r CoreCell> {
        resolve.cells.iter().find(|cell| {
            cell.selection.item_id == self.item
                && cell.selection.dimension_value.as_deref() == self.value
        })
    }

    /// This cell's binding in `resolve`, if the resolve covers it.
    pub fn binding<'r>(&self, resolve: &'r CoreResolve) -> Option<&'r CoreBinding> {
        self.in_resolve(resolve)
            .and_then(|cell| cell.binding.as_ref())
    }
}

/// What the steps take from a valid input.
pub struct Validated<'a> {
    /// The cell the child rates.
    pub rated: RatedCell<'a>,
    /// The derived meter of a usage child.
    pub meter: Option<DerivedMeterEvaluator<'a>>,
}

/// Checks `input` and returns what the steps rate it with.
///
/// # Errors
///
/// The [`EvaluationError`] of the first check that fails.
pub fn input(input: &EvaluationInput) -> Result<Validated<'_>, EvaluationError> {
    instants(input)?;
    geometry(input)?;
    kinds(input)?;
    let rated = rated_cell(input)?;
    pricing(input, &rated)?;
    let meter = usage(input, &rated)?;
    slices(input, &rated)?;
    group(input)?;
    Ok(Validated { rated, meter })
}

/// The split points of the child: the `ends_on` of each binding of its rated cell and, for a
/// recurring line, its committed quantity intervals.
#[must_use]
pub fn split_input(input: &EvaluationInput, rated: &RatedCell<'_>) -> SplitInput {
    SplitInput {
        kind: input.child.kind,
        window: input.child.window,
        ends_on: input
            .pricing
            .resolves
            .iter()
            .filter_map(|resolve| rated.binding(resolve).and_then(|b| b.price.ends_on))
            .collect(),
        quantity_intervals: input
            .subscription
            .quantity_intervals
            .iter()
            .map(|interval| interval.interval)
            .collect(),
    }
}

/// The committed quantity in force over a recurring slice: the subscription version's quantity
/// interval that holds it, or the fact's quantity when the version carries no interval
/// (DESIGN 14 §4.2: `quantity @ t`, sliced at each change).
///
/// # Errors
///
/// `layout_mismatch` when intervals exist and none holds the slice.
pub fn committed_quantity<'a>(
    input: &'a EvaluationInput,
    slice: &SliceQuantity,
) -> Result<Option<&'a ExactQuantity>, EvaluationError> {
    let intervals = &input.subscription.quantity_intervals;
    if intervals.is_empty() {
        return Ok(input.fact.quantity.as_ref());
    }
    intervals
        .iter()
        .find(|held| {
            held.interval.start <= slice.slice_start && slice.slice_end <= held.interval.end
        })
        .map(|held| Some(&held.quantity))
        .ok_or_else(|| {
            layout(format!(
                "no committed quantity interval holds slice [{}, {})",
                slice.slice_start, slice.slice_end
            ))
        })
}

/// The resolves that price part of the served window: the one in force at its start and every
/// later one before its end.
pub fn pricing_resolves(input: &EvaluationInput) -> &[CoreResolve] {
    let window = &input.child.window;
    let resolves = &input.pricing.resolves;
    let first = resolves
        .iter()
        .rposition(|resolve| geometry::midnight(resolve.resolve_date) <= window.served_from)
        .unwrap_or(0);
    let last = resolves
        .iter()
        .rposition(|resolve| geometry::midnight(resolve.resolve_date) < window.served_to)
        .map_or(first, |last| last.max(first));
    resolves.get(first..=last).unwrap_or_default()
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
    for interval in fact.suspensions.iter().chain(
        input
            .subscription
            .quantity_intervals
            .iter()
            .map(|held| &held.interval),
    ) {
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

/// The served window lies in the fact's served extent and the billing period, and a recurring
/// child's window is the whole billing period; suspensions and quantity intervals are ordered.
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
    if window.served_from < fact.served.start || window.served_to > fact.served.end {
        return Err(geometry_error(
            "the window's served part leaves the fact's served extent".into(),
        ));
    }
    let whole_period =
        window.window_start == fact.period.start && window.window_end == fact.period.end;
    if input.child.kind == ChildKind::PeriodLine && !whole_period {
        return Err(geometry_error(
            "a recurring child's window is its whole billing period".into(),
        ));
    }
    let inside = |interval: &Interval| {
        fact.period.start <= interval.start && interval.end <= fact.period.end
    };
    if !disjoint(&fact.suspensions) || !fact.suspensions.iter().all(inside) {
        return Err(geometry_error(
            "the suspensions are empty, overlap, are out of order or leave the billing period"
                .into(),
        ));
    }
    let intervals: Vec<Interval> = input
        .subscription
        .quantity_intervals
        .iter()
        .map(|held| held.interval)
        .collect();
    if !disjoint(&intervals) {
        return Err(geometry_error(
            "the committed quantity intervals are empty, overlap or are out of order".into(),
        ));
    }
    Ok(())
}

/// Each interval is non-empty and ends at or before the next one starts.
fn disjoint(intervals: &[Interval]) -> bool {
    intervals
        .iter()
        .all(|interval| interval.start < interval.end)
        && intervals
            .windows(2)
            .all(|pair| pair[0].end <= pair[1].start)
}

/// The child, fact and declaration agree, and the child is one this generation rates.
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
        (ChildKind::OneTime, _) => Err(unsupported(
            "one-time facts are stored, not rated, until R-19 is accepted".into(),
        )),
        (ChildKind::PeriodLine, Some(_)) => Err(unsupported(
            "a derived usage declaration on a recurring child".into(),
        )),
        // A usage child without a declaration is either an uncovered cell, which the bind step
        // fails `price_uncovered` (DESIGN §3.1), or a pipeline defect `usage` refuses.
        (ChildKind::UsageWindow, _) | (ChildKind::PeriodLine, None) => Ok(()),
    }
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
    // @cpt-begin:cpt-cf-bss-rating-algo-core-model:p1:inst-mod-dimension
    if pins.len() > 1 {
        return Err(unsupported(format!(
            "the accepted binding names {} dimension values for item {item}; one per item and \
             line until R-16",
            pins.len()
        )));
    }
    // @cpt-end:cpt-cf-bss-rating-algo-core-model:p1:inst-mod-dimension
    let accepted = pins.first().copied();
    Ok(RatedCell {
        item,
        value: accepted.and_then(|pin| pin.dimension_value.as_deref()),
        accepted,
    })
}

/// The resolves are one chain, and every cell, currency, decimal and model of them is well
/// formed for the child.
fn pricing(input: &EvaluationInput, rated: &RatedCell<'_>) -> Result<(), EvaluationError> {
    currency_code(&input.fact.currency, "billing currency")?;
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
    successor_present(input, rated)?;
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
        }
        if let Some(binding) = rated.binding(resolve) {
            model_allowed(binding)?;
            match input.child.kind {
                ChildKind::PeriodLine => recurring_period(input, binding)?,
                ChildKind::UsageWindow => usage_binding(binding)?,
                ChildKind::OneTime => {}
            }
        }
    }
    derived_decimals(input)
}

/// A later resolve takes over exactly at the `ends_on` of the rated cell's binding in the resolve
/// before (pricing D-425), inside the period.
fn handover(
    input: &EvaluationInput,
    rated: &RatedCell<'_>,
    before: &CoreResolve,
    after: &CoreResolve,
) -> Result<(), EvaluationError> {
    let ends_on = rated
        .binding(before)
        .and_then(|binding| binding.price.ends_on);
    if ends_on != Some(after.resolve_date) || after.resolve_date <= before.resolve_date {
        return Err(layout(format!(
            "the resolve of {} is not at the ends_on of the binding the resolve of {} gives \
             ({ends_on:?})",
            after.resolve_date, before.resolve_date
        )));
    }
    let at = geometry::midnight(after.resolve_date);
    if at <= input.fact.period.start || at >= input.fact.period.end {
        return Err(layout(format!(
            "the handover on {} is outside the billing period",
            after.resolve_date
        )));
    }
    Ok(())
}

/// The last resolve's rated binding does not end inside the served window: a slice after its
/// `ends_on` would be priced by a binding that has ended, so the resolve that takes over must be
/// in the input (DESIGN 01 §3.1: one resolve per `ends_on` up to the window end).
fn successor_present(
    input: &EvaluationInput,
    rated: &RatedCell<'_>,
) -> Result<(), EvaluationError> {
    let Some(last) = input.pricing.resolves.last() else {
        return Ok(());
    };
    match rated
        .binding(last)
        .and_then(|binding| binding.price.ends_on)
    {
        Some(ends_on) if geometry::midnight(ends_on) < input.child.window.served_to => {
            Err(layout(format!(
                "the binding the resolve of {} gives ends on {ends_on}, inside the served window, \
                 and no resolve takes over",
                last.resolve_date
            )))
        }
        _ => Ok(()),
    }
}

/// A resolve answers each item and dimension value once.
fn cells(resolve: &CoreResolve) -> Result<(), EvaluationError> {
    let mut seen = BTreeSet::new();
    for cell in &resolve.cells {
        let selection = &cell.selection;
        if !seen.insert((selection.item_id, selection.dimension_value.as_deref())) {
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

/// The currency is one `bss_money` accepts, and every decimal fits the canonical text bound: the
/// one check of a binding's form that `evaluate` and `evaluate_order` share.
///
/// # Errors
///
/// `binding_currency_mismatch` for a currency `bss_money` refuses, `precision_overflow` for a
/// decimal of more than 28 digits.
pub fn well_formed(binding: &CoreBinding) -> Result<(), EvaluationError> {
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

/// A billing or market currency is a code `bss_money` accepts, like every price's.
///
/// # Errors
///
/// `binding_currency_mismatch` naming the code.
pub fn currency_code(code: &str, what: &str) -> Result<(), EvaluationError> {
    CurrencySpec::try_new(code.to_owned(), 0)
        .map(|_| ())
        .map_err(|error| EvaluationError::BindingCurrencyMismatch {
            detail: format!(
                "the {what} {code:?} is not a currency code bss_money accepts: {error}"
            ),
        })
}

/// A model its charge kind allows (DESIGN 03 §4.1): `flat` for recurring and one-time charges,
/// `per_unit` for every kind, the tiered and package models for usage.
fn model_allowed(binding: &CoreBinding) -> Result<(), EvaluationError> {
    let allowed = match binding.price.model {
        CorePriceModel::Flat { .. } => binding.kind != ChargeKind::Usage,
        CorePriceModel::PerUnit { .. } => true,
        CorePriceModel::Graduated { .. }
        | CorePriceModel::Volume { .. }
        | CorePriceModel::Package { .. } => binding.kind == ChargeKind::Usage,
    };
    if allowed {
        return Ok(());
    }
    Err(unsupported(format!(
        "price {} prices a {:?} charge with a model that kind does not take",
        binding.price.price_id, binding.kind
    )))
}

/// A usage binding carries its derived meter and rating policy, which the pipeline guarantees,
/// and a minimum fee only where Pricing allows one (T-D-78; R-34 until Rating adopts a floor for
/// hourly or resource-scoped windows).
fn usage_binding(binding: &CoreBinding) -> Result<(), EvaluationError> {
    let (Some(_), Some(policy)) = (&binding.meter, &binding.usage_policy) else {
        return Err(unsupported(format!(
            "usage price {} reached the core without its meter or rating policy, which the \
             pipeline guarantees (a pipeline defect; the child fails closed)",
            binding.price.price_id
        )));
    };
    let floorable = policy.rating_window == RatingWindowKind::BillingCycle
        && policy.aggregation_scope == PolicyScope::SubscriptionLine;
    if binding.price.minimum_fee.is_some() && !floorable {
        return Err(unsupported(format!(
            "usage price {} carries a minimum fee on a {:?}, {:?}-scoped window; no floor is \
             agreed there (R-34)",
            binding.price.price_id, policy.rating_window, policy.aggregation_scope
        )));
    }
    Ok(())
}

/// A recurring price's period matches the fact's billing period (DESIGN 09 §4.1): the next
/// calendar month or year at the same time of day. The anchor rule is Subscriptions'
/// `BillingTerms`, which the input does not carry, so a month may be 28 to 31 days.
fn recurring_period(input: &EvaluationInput, binding: &CoreBinding) -> Result<(), EvaluationError> {
    const DAY: i64 = 86_400;
    if binding.recurring_period.is_none() {
        return Err(unsupported(format!(
            "recurring price {} has no recurring period; Pricing refuses such a price",
            binding.price.price_id
        )));
    }
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

/// A usage child: its window is the one its binding's policy gives, the policy and meter do not
/// change inside it, and its frozen declaration is the meter's. An uncovered cell has no
/// declaration and is left to the bind step (`price_uncovered`).
fn usage<'a>(
    input: &'a EvaluationInput,
    rated: &RatedCell<'_>,
) -> Result<Option<DerivedMeterEvaluator<'a>>, EvaluationError> {
    if input.child.kind != ChildKind::UsageWindow {
        return Ok(None);
    }
    let Some(frozen) = &input.derived else {
        let covered = pricing_resolves(input)
            .iter()
            .any(|resolve| rated.binding(resolve).is_some());
        if covered {
            return Err(unsupported(
                "a covered usage child reached the core without its derived declaration, which \
                 the pipeline guarantees (a pipeline defect; the child fails closed)"
                    .into(),
            ));
        }
        // Uncovered: no binding, so no meter and no declaration; the bind step fails it.
        return Ok(None);
    };
    let key = &input.child.aggregation_key;
    let mut policy: Option<&CoreUsagePolicy> = None;
    for binding in pricing_resolves(input)
        .iter()
        .filter_map(|resolve| rated.binding(resolve))
    {
        let (Some(meter), Some(this)) = (&binding.meter, &binding.usage_policy) else {
            continue;
        };
        if key.meter.as_deref() != Some(meter.usage_type_id.as_str()) {
            return Err(layout(format!(
                "price {} meters {}, the child counts {:?}",
                binding.price.price_id, meter.usage_type_id, key.meter
            )));
        }
        if format!("products.derived/{}@{}", frozen.code, frozen.version) != meter.usage_type_id
            || frozen.version.to_string() != meter.version
        {
            return Err(EvaluationError::DerivedEvaluationFailed {
                detail: format!(
                    "the frozen declaration {}@{} is not the meter {}@{} of price {}",
                    frozen.code,
                    frozen.version,
                    meter.usage_type_id,
                    meter.version,
                    binding.price.price_id
                ),
            });
        }
        let scope_matches = matches!(
            (this.aggregation_scope, &key.scope),
            (
                PolicyScope::SubscriptionLine,
                AggregationScope::SubscriptionLine
            ) | (PolicyScope::Resource, AggregationScope::Resource { .. })
        );
        if !scope_matches {
            return Err(layout(format!(
                "price {} folds {:?}, the child is keyed {:?}",
                binding.price.price_id, this.aggregation_scope, key.scope
            )));
        }
        match policy {
            Some(first) if !same_policy(first, this) => {
                return Err(EvaluationError::IntraWindowPolicyChange {
                    detail: format!(
                        "price {} rates the window under another usage rating policy",
                        binding.price.price_id
                    ),
                });
            }
            _ => policy = Some(this),
        }
    }
    if let Some(policy) = policy {
        let window = &input.child.window;
        let extent = FactExtent {
            served: input.fact.served,
            period: input.fact.period,
        };
        let expected = geometry::window_containing(policy, extent, window.served_from)?;
        if expected != *window {
            return Err(geometry_error(format!(
                "the child's window [{}, {}) served [{}, {}) is not the {:?} window \
                 window_geometry gives",
                window.window_start,
                window.window_end,
                window.served_from,
                window.served_to,
                policy.rating_window
            )));
        }
    }
    DerivedMeterEvaluator::new(frozen, input.budget).map(Some)
}

/// Two policies rate a window the same way: the continuation key compares their content, not
/// their identity (T-D-36).
fn same_policy(left: &CoreUsagePolicy, right: &CoreUsagePolicy) -> bool {
    /// The rules a policy rates by; destructured, so a field added to the policy breaks the build
    /// here until it is placed on one side or the other.
    fn content(
        policy: &CoreUsagePolicy,
    ) -> (
        RatingWindowKind,
        PolicyScope,
        ResetPolicy,
        PartialWindowPolicy,
        WindowFold,
    ) {
        let CoreUsagePolicy {
            policy_id: _,
            version: _,
            digest: _,
            rating_window,
            aggregation_scope,
            reset,
            partial_window,
            fold,
        } = policy;
        (
            *rating_window,
            *aggregation_scope,
            *reset,
            *partial_window,
            *fold,
        )
    }
    content(left) == content(right)
}

/// The slices are the cuts of `split_points`, no quantity is negative, a recurring slice carries
/// the committed quantity in force and no granule, and a usage slice's granules are its UTC hours.
fn slices(input: &EvaluationInput, rated: &RatedCell<'_>) -> Result<(), EvaluationError> {
    let window = &input.child.window;
    let slices = &input.quantities.slices;
    let mut bounds = vec![window.served_from];
    bounds.extend(geometry::split_points(&split_input(input, rated)));
    bounds.push(window.served_to);
    let laid_out = slices.len() + 1 == bounds.len()
        && slices
            .iter()
            .zip(bounds.windows(2))
            .all(|(slice, cut)| slice.slice_start == cut[0] && slice.slice_end == cut[1]);
    if !laid_out {
        return Err(layout(format!(
            "the child's {} slices are not the layout of its split points {:?}",
            slices.len(),
            &bounds[1..bounds.len() - 1]
        )));
    }
    let mut previous_granule: Option<OffsetDateTime> = None;
    for slice in slices {
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
        match input.child.kind {
            ChildKind::PeriodLine => recurring_slice(input, slice)?,
            ChildKind::UsageWindow => granules(slice, &mut previous_granule)?,
            ChildKind::OneTime => {}
        }
    }
    Ok(())
}

/// A recurring slice carries the committed quantity in force over it, and no granule.
fn recurring_slice(input: &EvaluationInput, slice: &SliceQuantity) -> Result<(), EvaluationError> {
    if !slice.granules.is_empty() {
        return Err(layout(format!(
            "recurring slice [{}, {}) carries usage granules",
            slice.slice_start, slice.slice_end
        )));
    }
    match committed_quantity(input, slice)? {
        Some(committed) if slice.q != *committed => Err(layout(format!(
            "slice [{}, {}) carries quantity {}, not the committed {committed}",
            slice.slice_start, slice.slice_end, slice.q
        ))),
        _ => Ok(()),
    }
}

/// A usage slice's granules are whole UTC hours that touch the slice, in order across the child.
fn granules(
    slice: &SliceQuantity,
    previous: &mut Option<OffsetDateTime>,
) -> Result<(), EvaluationError> {
    for granule in &slice.granules {
        let start = granule.granule_start;
        let hour = start.minute() == 0 && start.second() == 0;
        let touches = start < slice.slice_end && slice.slice_start < geometry::hour_after(start)?;
        let ordered = previous.is_none_or(|previous| previous < start);
        if !(hour && touches && ordered) {
            return Err(layout(format!(
                "granule {start} of slice [{}, {}) is not a UTC hour of the slice in order",
                slice.slice_start, slice.slice_end
            )));
        }
        *previous = Some(start);
    }
    Ok(())
}

/// A window group's context agrees with the child: the child is one member, listed with its own
/// fact version, layout and slices; the prefix is the earlier members' quantity and the total all
/// members' (T-D-67). Only usage windows form groups.
fn group(input: &EvaluationInput) -> Result<(), EvaluationError> {
    let Some(context) = &input.group_context else {
        return Ok(());
    };
    if input.child.kind != ChildKind::UsageWindow {
        return Err(unsupported(
            "a window group of a recurring child; only usage windows span facts".into(),
        ));
    }
    let members = &context.members;
    let position = members
        .iter()
        .position(|member| member.child_id == input.child.child_id);
    let ordered = members
        .windows(2)
        .all(|pair| pair[0].served_from < pair[1].served_from);
    let Some(position) = position.filter(|_| ordered) else {
        return Err(layout(
            "the window group's members are out of order or do not include the child".into(),
        ));
    };
    let member = &members[position];
    let own = &input.quantities.slices;
    let same = member.fact_id == input.fact.fact_id
        && member.fact_version == input.fact.fact_version
        && member.served_from == input.child.window.served_from
        && member.layout_version == input.quantities.layout_version
        && member.slices.len() == own.len()
        && member.slices.iter().zip(own).all(|(theirs, ours)| {
            (
                theirs.slice_start,
                theirs.slice_end,
                &theirs.q,
                theirs.q_version,
            ) == (ours.slice_start, ours.slice_end, &ours.q, ours.q_version)
        });
    if !same {
        return Err(layout(
            "the window group lists the child with another fact version, layout or slices".into(),
        ));
    }
    // An earlier member below zero would move this member's band axis below every band.
    if let Some(slice) = members
        .iter()
        .flat_map(|member| &member.slices)
        .find(|slice| slice.q.is_negative())
    {
        return Err(EvaluationError::NegativeWindowQuantity {
            detail: format!(
                "window group slice [{}, {}) has a quantity below zero",
                slice.slice_start, slice.slice_end
            ),
        });
    }
    let prefix = ExactQuantity::sum(quantities(&members[..position]), "window group prefix")?;
    let total = ExactQuantity::sum(quantities(members), "window group total")?;
    if prefix != context.prefix_before_this_member || total != context.group_total {
        return Err(layout(format!(
            "the window group's prefix {} and total {} are not its members' {prefix} and {total}",
            context.prefix_before_this_member, context.group_total
        )));
    }
    Ok(())
}

/// Every slice quantity of `members`, in order.
fn quantities(members: &[GroupMember]) -> impl Iterator<Item = &ExactQuantity> {
    members
        .iter()
        .flat_map(|member| member.slices.iter().map(|slice| &slice.q))
}

/// The constants of a derived declaration fit the canonical text bound.
fn derived_decimals(input: &EvaluationInput) -> Result<(), EvaluationError> {
    /// Collects every constant and divisor of the formula.
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

/// A price decimal has at most 28 significant digits, so it has canonical text.
fn decimal(value: Decimal, price_id: Uuid) -> Result<(), EvaluationError> {
    validate_amount(value)
        .map(|_| ())
        .map_err(|_| EvaluationError::PrecisionOverflow {
            detail: format!("a decimal of price {price_id} has more than 28 digits"),
        })
}

/// `period_geometry_mismatch` with `detail`.
fn geometry_error(detail: String) -> EvaluationError {
    EvaluationError::PeriodGeometryMismatch { detail }
}

/// `layout_mismatch` with `detail`.
fn layout(detail: String) -> EvaluationError {
    EvaluationError::LayoutMismatch { detail }
}

/// `unsupported_primitive` with `detail`.
fn unsupported(detail: String) -> EvaluationError {
    EvaluationError::UnsupportedPrimitive { detail }
}

#[cfg(test)]
#[path = "validate_tests.rs"]
mod tests;
