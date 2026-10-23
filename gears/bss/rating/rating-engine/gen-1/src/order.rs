//! `evaluate_order`: the pure function under purchase evaluation (feature 02 §3.8, feature 10;
//! rating DESIGN 11 §4.10; T-D-54, T-D-83; Orders D-197, D-216).
//!
//! Orders asks one batched, nonbinding evaluation of the exact selected Pricing bindings of an
//! order before any subscription exists, and stores the answer verbatim: Rating owns the model
//! formulas, minimum fees, quantity application, display rounding and every aggregate. This
//! function prices those bindings as given — no resolve, no successor walk — with the formulas
//! of `evaluate`:
//!
//! - **Recurring** items are priced per cycle (`flat` → `amount`, `per_unit` →
//!   `unit_amount × quantity`) and floored per cycle at the price's `minimum_fee` (covered
//!   fraction 1, T-D-78); a **one-time** item once, floored the same way. No activation-time
//!   proration: the evaluation is indicative (Orders DESIGN, D-197).
//! - **Usage** has no committed amount: it is reported as its rates and rating window, with its
//!   `minimum_fee` reported but not applied, because consumption is unknown.
//! - **Horizon.** A line's recurring aggregate is its per-cycle sum times the periods of its term:
//!   `count` for `FixedPeriods`, 12 for a rolling `month` and 1 for a rolling `year`; the basis is
//!   `finite_term` or `rolling_annualized`, and an order whose recurring lines have both is
//!   `mixed` (an order without recurring amounts takes its lines' terms). Per-cycle figures stay
//!   in `recurring_by_cycle`, never summed across cycles.
//! - **TCV** = the recurring aggregates plus the one-time amounts, net, pre-tax, usage excluded;
//!   one carrier at order scope. A Preview line whose term or cycle is missing withholds the TCV
//!   and is listed; every other figure is still returned.
//! - **Discount** is `unavailable` (promotions are deferred, pricing D-409), never a guessed zero;
//!   net equals gross.
//! - Every amount is exact canonical decimal text in major units (at most 28 significant and 28
//!   fractional digits, Orders D-216) and a display estimate rounded half to even at the currency
//!   scale. Nothing is persisted; the pipeline digests the request.

// @cpt-algo:cpt-cf-bss-rating-algo-core-evaluate-order:p1

use std::collections::BTreeSet;
use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::budget::ArithmeticBudget;
use crate::error::EvaluationError;
use crate::exact::{CoreCurrency, ExactMoney, ExactQuantity};
use crate::floor;
use crate::generation;
use crate::input::{
    ChargeKind, CoreBinding, CorePriceModel, EngineId, PriceState, RatingWindowKind,
    RecurringPeriod,
};
use crate::model;
use crate::validate;

/// One purchase evaluation request, as the generation reads it (Orders D-197 commercial inputs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderEvaluationInput {
    pub assessment_id: Uuid,
    /// The market currency; a binding in another currency fails `fx_not_supported`.
    pub currency: String,
    /// A contract the order names; contract terms are deferred, so naming one fails
    /// `adjustment_unsupported`.
    pub contract_ref: Option<String>,
    pub lines: Vec<OrderLine>,
    pub budget: ArithmeticBudget,
    pub engine: EngineId,
}

/// One order line: its term, its billing cycle and the selected binding of each item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderLine {
    pub line_id: Uuid,
    pub plan_revision_id: Uuid,
    pub term: OrderTerm,
    /// `BillingTerms.cycle`; absent only on a Preview line whose cycle is not chosen yet.
    pub cycle: Option<RecurringPeriod>,
    pub items: Vec<OrderItem>,
}

/// A line's term (Pricing `Term`, Orders D-193); `missing` only on Preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OrderTerm {
    Rolling,
    /// A finite term of `count` invoice periods; zero periods is no term (Orders D-193) and does
    /// not decode.
    FixedPeriods {
        count: NonZeroU32,
    },
    /// Preview only: the term is not chosen yet, so the line's horizon and the TCV are withheld.
    Missing,
}

/// One item: its explicit quantity and the binding Pricing selected for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderItem {
    pub item_id: Uuid,
    /// The item's quantity as the order maps it; never an implicit multiplier.
    pub quantity: ExactQuantity,
    pub binding: CoreBinding,
}

/// The evaluation of an order: per line and per order, by charge kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderEvaluation {
    pub assessment_id: Uuid,
    pub currency: String,
    pub currency_scale: u8,
    pub lines: Vec<LineEvaluation>,
    /// The order-scope rows; the `recurring` row is always present and carries the TCV.
    pub order: Vec<ChargeRow>,
    /// Absent when a line's term or cycle is missing (Preview).
    pub tcv: Option<Tcv>,
    /// The lines whose missing term or cycle withholds the TCV.
    pub tcv_withheld: Vec<Uuid>,
    /// The generation whose formulas and policy produced the answer.
    pub engine: EngineId,
}

/// One line's items and rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineEvaluation {
    pub line_id: Uuid,
    pub items: Vec<ItemEvaluation>,
    /// One row per charge kind the line has.
    pub rows: Vec<ChargeRow>,
    /// The term evidence of the line's horizon; absent when withheld.
    pub horizon: Option<Horizon>,
}

/// How many periods a line's recurring aggregate covers, and on which basis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Horizon {
    pub term: OrderTerm,
    pub cycle: RecurringPeriod,
    pub periods: u32,
    pub basis: AmountBasis,
}

/// The valuation horizon of a recurring aggregate (Orders `amount_basis`, `tcv_basis`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmountBasis {
    FiniteTerm,
    RollingAnnualized,
    Mixed,
}

/// One item's figures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemEvaluation {
    pub item_id: Uuid,
    pub price_id: Uuid,
    pub charge_kind: ChargeKind,
    /// The cycle a recurring item's amounts are per.
    pub cycle: Option<RecurringPeriod>,
    /// The model's amount before the minimum-fee floor; none for usage.
    pub rated: Option<OrderAmount>,
    pub minimum_fee: Option<OrderAmount>,
    /// Whether the floor raised the amount; false for usage, whose fee is reported only.
    pub minimum_fee_applied: bool,
    /// Per cycle for recurring, once for one-time; none for usage.
    pub gross: Option<OrderAmount>,
    pub discount_status: DiscountStatus,
    pub net: Option<OrderAmount>,
    pub usage_rates: Option<UsageRates>,
}

/// A usage item's rates: what it charges once consumption is known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageRates {
    pub model: CorePriceModel,
    pub unit: Option<String>,
    pub rating_window: RatingWindowKind,
}

/// One row of a line or of the order, for one charge kind (Orders D-216 stored totals).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChargeRow {
    pub charge_kind: ChargeKind,
    pub amount_status: AmountStatus,
    /// Recurring: the aggregate over the horizon; one-time: the sum; usage: none. Absent for a
    /// recurring row whose horizon is withheld.
    pub gross: Option<OrderAmount>,
    pub discount_status: DiscountStatus,
    pub net: Option<OrderAmount>,
    pub amount_basis: Option<AmountBasis>,
    /// Recurring: the per-cycle sums, one entry per cycle, never summed across cycles.
    pub recurring_by_cycle: Vec<CycleAmount>,
}

/// A recurring amount per one cycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CycleAmount {
    pub cycle: RecurringPeriod,
    pub gross: OrderAmount,
    pub net: OrderAmount,
}

/// Whether a row carries a committed amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmountStatus {
    Committed,
    /// Usage: priced at rating time, so no amount — distinct from a zero-priced charge.
    UncommittedUsage,
}

/// Whether a discount is known. Promotions are deferred (pricing D-409), so it never is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscountStatus {
    Unavailable,
}

/// The total contract value: net, pre-tax, usage excluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tcv {
    pub amount: OrderAmount,
    pub basis: AmountBasis,
    /// Per line, the horizon its recurring amounts were counted over.
    pub evidence: Vec<LineHorizon>,
}

/// One line's horizon in the TCV evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineHorizon {
    pub line_id: Uuid,
    pub horizon: Horizon,
}

/// An amount as Orders stores it: exact canonical decimal text in major units, and the display
/// estimate rounded half to even at the currency scale (money ADR rule 7, Orders D-216).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderAmount {
    pub exact: String,
    pub display: String,
}

/// Annualization of a rolling term: 12 monthly or 1 yearly periods (Orders D-40, D-197).
const MONTHS_PER_YEAR: u32 = 12;

/// Evaluates one order (see the module documentation).
///
/// # Errors
///
/// `engine_unavailable` for another generation, digest or budget; `adjustment_unsupported` for a
/// contract; `fx_not_supported` for a binding in another currency; `binding_currency_mismatch`
/// for two stored scales or a currency `bss_money` refuses; `binding_mismatch` for a binding of
/// another item; `non_injective_mapping` for an item listed twice; `price_uncovered` for a price
/// that is not approved; `unsupported_primitive` for a model or term the sale profile does not
/// take, or an order without lines or a line without items; `period_geometry_mismatch` for a recurring price whose period is not the line's cycle;
/// `negative_window_quantity` for a negative quantity; `precision_overflow` for an amount that is
/// not decimal text within 28 digits or leaves the budget.
pub fn evaluate_order(input: &OrderEvaluationInput) -> Result<OrderEvaluation, EvaluationError> {
    // @cpt-begin:cpt-cf-bss-rating-algo-core-evaluate-order:p1:inst-ord-steps
    generation::check(&input.engine, input.budget)?;
    if let Some(contract) = &input.contract_ref {
        return Err(EvaluationError::AdjustmentUnsupported {
            detail: format!(
                "the order names contract {contract:?}; contract terms are deferred and never \
                 applied by default"
            ),
        });
    }
    if input.lines.is_empty() || input.lines.iter().any(|line| line.items.is_empty()) {
        return Err(EvaluationError::UnsupportedPrimitive {
            detail: "an order without lines, or a line without items, has nothing to evaluate"
                .into(),
        });
    }
    let currency = order_currency(input)?;
    let mut lines = Vec::with_capacity(input.lines.len());
    let mut order = Aggregate::new(&currency);
    let mut withheld = Vec::new();
    let mut evidence = Vec::new();
    let mut recurring_bases = Vec::new();
    for line in &input.lines {
        let evaluated = evaluate_line(input, line, &currency)?;
        match evaluated.horizon {
            Some(horizon) => {
                if evaluated.aggregate.has(ChargeKind::Recurring) {
                    recurring_bases.push(horizon.basis);
                }
                evidence.push(LineHorizon {
                    line_id: line.line_id,
                    horizon,
                });
            }
            None => withheld.push(line.line_id),
        }
        order.add(&evaluated.aggregate)?;
        lines.push(evaluated.result);
    }
    // The basis describes how the recurring amounts were valued, so it is that of the lines that
    // carry them; an order without recurring amounts takes its lines' terms.
    let basis = basis(recurring_bases.into_iter())
        .or_else(|| basis(evidence.iter().map(|line| line.horizon.basis)));
    let tcv = if withheld.is_empty() {
        let recurring = order
            .horizon
            .clone()
            .unwrap_or_else(|| ExactMoney::zero(&currency));
        let total = recurring.checked_add(&order.one_time, "tcv")?;
        Some(Tcv {
            amount: amount(&total, "tcv")?,
            basis: basis.ok_or_else(|| EvaluationError::LayoutMismatch {
                detail: "a TCV without a line horizon".into(),
            })?,
            evidence,
        })
    } else {
        None
    };
    // @cpt-end:cpt-cf-bss-rating-algo-core-evaluate-order:p1:inst-ord-steps
    // @cpt-begin:cpt-cf-bss-rating-algo-core-evaluate-order:p1:inst-ord-return
    Ok(OrderEvaluation {
        assessment_id: input.assessment_id,
        currency: currency.code.clone(),
        currency_scale: currency.scale,
        lines,
        order: order.rows(true, basis)?,
        tcv,
        tcv_withheld: withheld,
        engine: input.engine.clone(),
    })
    // @cpt-end:cpt-cf-bss-rating-algo-core-evaluate-order:p1:inst-ord-return
}

/// The one currency and stored scale of every binding of the order.
fn order_currency(input: &OrderEvaluationInput) -> Result<CoreCurrency, EvaluationError> {
    validate::currency_code(&input.currency, "market currency")?;
    let mut found: Option<&CoreCurrency> = None;
    for item in input.lines.iter().flat_map(|line| &line.items) {
        let price = &item.binding.price;
        if price.currency.code != input.currency {
            return Err(EvaluationError::FxNotSupported {
                detail: format!(
                    "price {} is in {:?}, the market in {:?}; nothing is converted",
                    price.price_id, price.currency.code, input.currency
                ),
            });
        }
        match found {
            Some(first) if first.scale != price.currency.scale => {
                return Err(EvaluationError::BindingCurrencyMismatch {
                    detail: format!(
                        "the order's prices carry scales {} and {}; one total has one scale",
                        first.scale, price.currency.scale
                    ),
                });
            }
            _ => found = Some(&price.currency),
        }
    }
    found
        .cloned()
        .ok_or_else(|| EvaluationError::UnsupportedPrimitive {
            detail: "an order without bindings has nothing to evaluate".into(),
        })
}

/// One line's result and its exact sums for the order.
struct EvaluatedLine {
    result: LineEvaluation,
    aggregate: Aggregate,
    horizon: Option<Horizon>,
}

/// Prices every item of a line and sums them by charge kind.
fn evaluate_line(
    input: &OrderEvaluationInput,
    line: &OrderLine,
    currency: &CoreCurrency,
) -> Result<EvaluatedLine, EvaluationError> {
    let mut seen = BTreeSet::new();
    let mut items = Vec::with_capacity(line.items.len());
    let mut aggregate = Aggregate::new(currency);
    let horizon = horizon(line);
    for item in &line.items {
        if !seen.insert(item.item_id) {
            return Err(EvaluationError::NonInjectiveMapping {
                detail: format!("line {} lists item {} twice", line.line_id, item.item_id),
            });
        }
        let priced = price_item(input, line, item)?;
        aggregate.item(&priced, item.binding.recurring_period)?;
        items.push(priced.result);
    }
    aggregate.close_line(horizon.map(|horizon| horizon.periods))?;
    let rows = aggregate.rows(false, horizon.map(|horizon| horizon.basis))?;
    Ok(EvaluatedLine {
        result: LineEvaluation {
            line_id: line.line_id,
            items,
            rows,
            horizon,
        },
        aggregate,
        horizon,
    })
}

/// The periods a line's recurring aggregate covers, or none when Preview has not chosen its term
/// or cycle.
fn horizon(line: &OrderLine) -> Option<Horizon> {
    let cycle = line.cycle?;
    let (periods, basis) = match line.term {
        OrderTerm::Missing => return None,
        OrderTerm::FixedPeriods { count } => (count.get(), AmountBasis::FiniteTerm),
        OrderTerm::Rolling => match cycle {
            RecurringPeriod::Month => (MONTHS_PER_YEAR, AmountBasis::RollingAnnualized),
            RecurringPeriod::Year => (1, AmountBasis::RollingAnnualized),
        },
    };
    Some(Horizon {
        term: line.term,
        cycle,
        periods,
        basis,
    })
}

/// One item priced: its result and its exact gross.
struct PricedItem {
    result: ItemEvaluation,
    kind: ChargeKind,
    gross: Option<ExactMoney>,
}

/// Prices one item by its binding, as given.
fn price_item(
    input: &OrderEvaluationInput,
    line: &OrderLine,
    item: &OrderItem,
) -> Result<PricedItem, EvaluationError> {
    let binding = &item.binding;
    let price = &binding.price;
    check_item(line, item)?;
    let fee = price
        .minimum_fee
        .map(|fee| ExactMoney::from_price(fee, &price.currency));
    let minimum_fee = fee
        .as_ref()
        .map(|fee| amount(fee, "minimum fee"))
        .transpose()?;
    if binding.kind == ChargeKind::Usage {
        let rating_window = binding
            .usage_policy
            .as_ref()
            .map(|policy| policy.rating_window)
            .ok_or_else(|| EvaluationError::UnsupportedPrimitive {
                detail: format!("usage price {} has no rating policy", price.price_id),
            })?;
        return Ok(PricedItem {
            result: ItemEvaluation {
                item_id: item.item_id,
                price_id: price.price_id,
                charge_kind: binding.kind,
                cycle: None,
                rated: None,
                minimum_fee,
                minimum_fee_applied: false,
                gross: None,
                discount_status: DiscountStatus::Unavailable,
                net: None,
                usage_rates: Some(UsageRates {
                    model: price.model.clone(),
                    unit: binding.unit.clone(),
                    rating_window,
                }),
            },
            kind: binding.kind,
            gross: None,
        });
    }
    // The model formula of `evaluate`, priced alone (offset zero), then the floor of roll-up at a
    // covered fraction of 1 (T-D-78).
    let rated = model::price(&price.model, &price.currency, &item.quantity, input.budget)?.amount;
    let topup = match &fee {
        Some(fee) => floor::shortfall(fee, &rated)?,
        None => ExactMoney::zero(&price.currency),
    };
    let applied = !topup.is_zero();
    let gross = rated.checked_add(&topup, "item gross")?;
    let gross_text = amount(&gross, "item gross")?;
    Ok(PricedItem {
        result: ItemEvaluation {
            item_id: item.item_id,
            price_id: price.price_id,
            charge_kind: binding.kind,
            cycle: binding.recurring_period,
            rated: Some(amount(&rated, "item amount")?),
            minimum_fee,
            minimum_fee_applied: applied,
            gross: Some(gross_text.clone()),
            discount_status: DiscountStatus::Unavailable,
            net: Some(gross_text),
            usage_rates: None,
        },
        kind: binding.kind,
        gross: Some(gross),
    })
}

/// The item's binding is its own, approved, in the sale profile (pricing D-504), and its
/// recurring period is the line's cycle.
fn check_item(line: &OrderLine, item: &OrderItem) -> Result<(), EvaluationError> {
    let binding = &item.binding;
    let price = &binding.price;
    if binding.item_id != item.item_id {
        return Err(EvaluationError::BindingMismatch {
            detail: format!(
                "item {} carries the binding of item {}",
                item.item_id, binding.item_id
            ),
        });
    }
    if price.state != PriceState::Approved {
        return Err(EvaluationError::PriceUncovered {
            detail: format!("price {} is not approved; never free", price.price_id),
        });
    }
    if item.quantity.is_negative() {
        return Err(EvaluationError::NegativeWindowQuantity {
            detail: format!("item {} has a quantity below zero", item.item_id),
        });
    }
    let profile = match (binding.kind, &price.model) {
        (ChargeKind::Recurring | ChargeKind::OneTime, model) => matches!(
            model,
            CorePriceModel::Flat { .. } | CorePriceModel::PerUnit { .. }
        ),
        (ChargeKind::Usage, model) => matches!(
            model,
            CorePriceModel::PerUnit { .. }
                | CorePriceModel::Volume { .. }
                | CorePriceModel::Graduated { .. }
        ),
    };
    if !profile {
        return Err(EvaluationError::UnsupportedPrimitive {
            detail: format!(
                "price {} prices a {:?} item with a model the sale profile does not take \
                 (pricing D-504)",
                price.price_id, binding.kind
            ),
        });
    }
    let period_fits = match (binding.kind, binding.recurring_period, line.cycle) {
        (ChargeKind::Recurring, None, _) => {
            return Err(EvaluationError::UnsupportedPrimitive {
                detail: format!(
                    "recurring price {} has no recurring period; Pricing refuses such a price",
                    price.price_id
                ),
            });
        }
        (ChargeKind::Recurring, Some(period), Some(cycle)) => period == cycle,
        (ChargeKind::Recurring, Some(_), None) => true,
        (ChargeKind::Usage | ChargeKind::OneTime, period, _) => period.is_none(),
    };
    if !period_fits {
        return Err(EvaluationError::PeriodGeometryMismatch {
            detail: format!(
                "price {} is {:?}, the line's billing cycle is {:?}",
                price.price_id, binding.recurring_period, line.cycle
            ),
        });
    }
    validate::well_formed(binding)
}

/// The exact sums of a line or an order by charge kind.
struct Aggregate {
    currency: CoreCurrency,
    /// Per cycle, the recurring gross of one period.
    by_cycle: Vec<(RecurringPeriod, ExactMoney)>,
    /// The recurring gross over the horizon; none while a line's horizon is withheld.
    horizon: Option<ExactMoney>,
    one_time: ExactMoney,
    kinds: BTreeSet<ChargeKind>,
}

impl Aggregate {
    /// Nothing yet.
    fn new(currency: &CoreCurrency) -> Self {
        Self {
            currency: currency.clone(),
            by_cycle: Vec::new(),
            horizon: Some(ExactMoney::zero(currency)),
            one_time: ExactMoney::zero(currency),
            kinds: BTreeSet::new(),
        }
    }

    /// Whether an item of `kind` was added.
    fn has(&self, kind: ChargeKind) -> bool {
        self.kinds.contains(&kind)
    }

    /// Adds one priced item of a line.
    fn item(
        &mut self,
        item: &PricedItem,
        cycle: Option<RecurringPeriod>,
    ) -> Result<(), EvaluationError> {
        self.kinds.insert(item.kind);
        match (item.kind, &item.gross, cycle) {
            (ChargeKind::Recurring, Some(gross), Some(cycle)) => self.add_cycle(cycle, gross),
            (ChargeKind::OneTime, Some(gross), _) => {
                self.one_time = self.one_time.checked_add(gross, "one-time sum")?;
                Ok(())
            }
            (ChargeKind::Usage, None, _) => Ok(()),
            (kind, gross, cycle) => Err(EvaluationError::UnsupportedPrimitive {
                detail: format!(
                    "a {kind:?} item with amount {} and cycle {cycle:?} cannot be summed",
                    if gross.is_some() { "present" } else { "absent" }
                ),
            }),
        }
    }

    /// Adds a recurring amount of one period of `cycle`.
    fn add_cycle(
        &mut self,
        cycle: RecurringPeriod,
        gross: &ExactMoney,
    ) -> Result<(), EvaluationError> {
        match self.by_cycle.iter_mut().find(|(this, _)| *this == cycle) {
            Some((_, sum)) => *sum = sum.checked_add(gross, "recurring per cycle")?,
            None => self.by_cycle.push((cycle, gross.clone())),
        }
        self.by_cycle
            .sort_by_key(|(cycle, _)| *cycle == RecurringPeriod::Year);
        Ok(())
    }

    /// Closes a line: its recurring horizon is its per-cycle sum times its periods, or withheld.
    /// A line without recurring items has a known zero whatever its term, so a Preview line that
    /// only withholds the TCV never withholds the order's recurring aggregate.
    fn close_line(&mut self, periods: Option<u32>) -> Result<(), EvaluationError> {
        self.horizon = match periods {
            None if !self.has(ChargeKind::Recurring) => Some(ExactMoney::zero(&self.currency)),
            None => None,
            Some(periods) => {
                let per_cycle = self
                    .by_cycle
                    .iter()
                    .try_fold(ExactMoney::zero(&self.currency), |sum, (_, gross)| {
                        sum.checked_add(gross, "recurring per cycle")
                    })?;
                Some(per_cycle.times_count(
                    periods,
                    ArithmeticBudget::GEN_1,
                    "recurring horizon",
                )?)
            }
        };
        Ok(())
    }

    /// Adds one closed line's sums to the order's; a withheld line horizon withholds the order's.
    fn add(&mut self, line: &Self) -> Result<(), EvaluationError> {
        self.kinds.extend(line.kinds.iter().copied());
        for (cycle, gross) in &line.by_cycle {
            self.add_cycle(*cycle, gross)?;
        }
        self.one_time = self.one_time.checked_add(&line.one_time, "one-time sum")?;
        self.horizon = match (self.horizon.take(), &line.horizon) {
            (Some(sum), Some(line)) => Some(sum.checked_add(line, "recurring horizon")?),
            _ => None,
        };
        Ok(())
    }

    /// The rows by charge kind present; an order always has its recurring row, the TCV carrier.
    fn rows(
        &self,
        order_scope: bool,
        basis: Option<AmountBasis>,
    ) -> Result<Vec<ChargeRow>, EvaluationError> {
        let mut rows = Vec::new();
        if order_scope || self.kinds.contains(&ChargeKind::Recurring) {
            let gross = self
                .horizon
                .as_ref()
                .map(|horizon| amount(horizon, "recurring aggregate"))
                .transpose()?;
            let amount_basis = gross.as_ref().and(basis);
            rows.push(ChargeRow {
                charge_kind: ChargeKind::Recurring,
                amount_status: AmountStatus::Committed,
                gross: gross.clone(),
                discount_status: DiscountStatus::Unavailable,
                net: gross,
                amount_basis,
                recurring_by_cycle: self
                    .by_cycle
                    .iter()
                    .map(|(cycle, gross)| {
                        let text = amount(gross, "recurring per cycle")?;
                        Ok(CycleAmount {
                            cycle: *cycle,
                            gross: text.clone(),
                            net: text,
                        })
                    })
                    .collect::<Result<_, EvaluationError>>()?,
            });
        }
        if self.kinds.contains(&ChargeKind::OneTime) {
            let sum = amount(&self.one_time, "one-time sum")?;
            rows.push(ChargeRow {
                charge_kind: ChargeKind::OneTime,
                amount_status: AmountStatus::Committed,
                gross: Some(sum.clone()),
                discount_status: DiscountStatus::Unavailable,
                net: Some(sum),
                amount_basis: None,
                recurring_by_cycle: Vec::new(),
            });
        }
        if self.kinds.contains(&ChargeKind::Usage) {
            rows.push(ChargeRow {
                charge_kind: ChargeKind::Usage,
                amount_status: AmountStatus::UncommittedUsage,
                gross: None,
                discount_status: DiscountStatus::Unavailable,
                net: None,
                amount_basis: None,
                recurring_by_cycle: Vec::new(),
            });
        }
        Ok(rows)
    }
}

/// The basis of several lines: theirs when they share it, else `mixed`; none for no line.
fn basis(lines: impl Iterator<Item = AmountBasis>) -> Option<AmountBasis> {
    lines.reduce(|first, next| {
        if first == next {
            first
        } else {
            AmountBasis::Mixed
        }
    })
}

/// An exact amount and its display estimate.
fn amount(value: &ExactMoney, site: &str) -> Result<OrderAmount, EvaluationError> {
    Ok(OrderAmount {
        exact: value.to_decimal_text(site)?,
        display: value.display(site)?,
    })
}

#[cfg(test)]
#[path = "order_tests.rs"]
mod tests;
