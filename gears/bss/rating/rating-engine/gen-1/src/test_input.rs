#![allow(clippy::expect_used, clippy::unwrap_used)]
//! A recurring `period_line` input the unit tests vary: September 2026 (30 days, 2 592 000 s), one
//! item priced in EUR at scale 2, one slice over the whole period.

use time::OffsetDateTime;
use time::macros::{date, datetime};
use uuid::Uuid;

use crate::budget::ArithmeticBudget;
use crate::exact::ExactQuantity;
use crate::input::{
    AggregationKey, AggregationScope, BillingTiming, CellSelection, ChargeKind, ChildKind,
    ChildQuantities, ChildSpec, CoreBinding, CoreCell, CoreCurrency, CoreInvoiceInputs, CorePrice,
    CorePriceModel, CoreResolve, EvaluationInput, FactKind, FactVersion, InputSource, Interval,
    PricePin, PriceState, PricingInput, RatingWindow, RecurringPeriod, SliceQuantity,
    StoredRounding, SubscriptionVersion, SuspensionPosture, TenantAxes,
};
use crate::input::{
    DerivedDeclaration, DerivedExpr, DerivedInput, FrozenDerivedDeclaration, Granularity,
    GranuleFold, RoundMode,
};
use rust_decimal::Decimal;

pub const PERIOD_START: OffsetDateTime = datetime!(2026-09-01 00:00:00 UTC);
pub const PERIOD_END: OffsetDateTime = datetime!(2026-10-01 00:00:00 UTC);

pub fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

pub fn item() -> Uuid {
    id(0x1_0001)
}

pub fn price_id() -> Uuid {
    id(0x2_0001)
}

pub fn eur() -> CoreCurrency {
    CoreCurrency {
        code: "EUR".to_owned(),
        scale: 2,
    }
}

pub fn quantity(text: &str) -> ExactQuantity {
    ExactQuantity::parse(text).unwrap()
}

pub fn binding(
    item_id: Uuid,
    price: Uuid,
    model: CorePriceModel,
    currency: CoreCurrency,
) -> CoreBinding {
    CoreBinding {
        item_id,
        price_book_entry_id: id(0x3_0001),
        dimension_key: None,
        dimension_value: None,
        sku_id: id(0x4_0000 + item_id.as_u128()),
        sku_version: 1,
        sku_code: "PRO".to_owned(),
        sku_name: "Pro plan".to_owned(),
        unit: None,
        meter: None,
        price: CorePrice {
            price_id: price,
            price_book_entry_id: id(0x3_0001),
            money_digest: "aa".repeat(32),
            currency,
            model,
            minimum_fee: None,
            effective_from: date!(2026 - 01 - 01),
            ends_on: None,
            state: PriceState::Approved,
        },
        kind: ChargeKind::Recurring,
        recurring_period: Some(RecurringPeriod::Month),
        via_default: false,
        usage_policy: None,
        invoice: CoreInvoiceInputs {
            template: "Pro plan, {period}".to_owned(),
            template_digest: "bb".repeat(32),
            template_source: InputSource::Entry,
            gl_code: "4000".to_owned(),
            tax_category: "std".to_owned(),
            timing: BillingTiming::Advance,
            rounding: StoredRounding::HalfEven,
        },
    }
}

pub fn cell(binding: CoreBinding) -> CoreCell {
    CoreCell {
        selection: CellSelection {
            item_id: binding.item_id,
            dimension_value: None,
        },
        binding: Some(binding),
    }
}

/// One slice `[start, end)` with quantity `q`.
pub fn slice(start: OffsetDateTime, end: OffsetDateTime, q: &str) -> SliceQuantity {
    SliceQuantity {
        slice_start: start,
        slice_end: end,
        q: quantity(q),
        q_version: 1,
        granules: Vec::new(),
    }
}

/// A recurring line of `model` over the whole of September with committed quantity `q`.
pub fn period_line(model: CorePriceModel, q: &str) -> EvaluationInput {
    let axes = TenantAxes {
        seller_tenant_id: id(0xA),
        payer_tenant_id: id(0xB),
        resource_tenant_id: id(0xC),
    };
    let period = Interval {
        start: PERIOD_START,
        end: PERIOD_END,
    };
    EvaluationInput {
        child: ChildSpec {
            child_id: id(0xC0),
            kind: ChildKind::PeriodLine,
            window: RatingWindow {
                window_start: PERIOD_START,
                window_end: PERIOD_END,
                served_from: PERIOD_START,
                served_to: PERIOD_END,
            },
            aggregation_key: AggregationKey {
                tenant_axes: axes,
                subscription_id: id(0x5),
                sub_line_key: "plan#1".to_owned(),
                item_id: item(),
                meter: None,
                dimension_key: None,
                scope: AggregationScope::SubscriptionLine,
            },
        },
        fact: FactVersion {
            fact_id: id(0xF),
            fact_version: 1,
            kind: FactKind::Recurring,
            subscription_id: id(0x5),
            sub_line_key: "plan#1".to_owned(),
            currency: "EUR".to_owned(),
            period,
            served: period,
            plan_revision_id: id(0x7),
            accepted: vec![PricePin {
                item_id: item(),
                dimension_value: None,
                price_id: price_id(),
            }],
            quantity: Some(quantity(q)),
            suspensions: Vec::new(),
            suspension_posture: SuspensionPosture::Continue,
            adjustments: Vec::new(),
            bundle: None,
        },
        pricing: PricingInput {
            plan_id: id(0x6),
            revision_id: id(0x7),
            pins: vec![PricePin {
                item_id: item(),
                dimension_value: None,
                price_id: price_id(),
            }],
            binding_of_record: "cc".repeat(32),
            resolves: vec![CoreResolve {
                resolve_date: date!(2026 - 09 - 01),
                binding_digest: "dd".repeat(32),
                cells: vec![cell(binding(item(), price_id(), model, eur()))],
            }],
        },
        derived: None,
        subscription: SubscriptionVersion {
            subscription_id: id(0x5),
            subscription_version: 1,
            tenant_axes: axes,
            plan_id: id(0x6),
            plan_revision_id: id(0x7),
            quantity_intervals: Vec::new(),
            brand: None,
        },
        quantities: ChildQuantities {
            layout_version: 1,
            slices: vec![slice(PERIOD_START, PERIOD_END, q)],
        },
        group_context: None,
        budget: ArithmeticBudget::GEN_1,
        engine: crate::engine_id(),
    }
}

/// Ends the first binding on `date` and adds the resolve of that date, which binds `successor`.
pub fn hand_over(input: &mut EvaluationInput, date: time::Date, successor: CoreBinding) {
    only_binding(input).price.ends_on = Some(date);
    input.pricing.resolves.push(CoreResolve {
        resolve_date: date,
        binding_digest: "ee".repeat(32),
        cells: vec![cell(successor)],
    });
}

/// The binding of the input's only cell, for a test to change.
pub fn only_binding(input: &mut EvaluationInput) -> &mut CoreBinding {
    input.pricing.resolves[0].cells[0].binding.as_mut().unwrap()
}

/// A cloudlet-hour: the larger of the RAM and CPU shares, each rounded up (Products' example).
pub fn cloudlet() -> FrozenDerivedDeclaration {
    let share = |name: &str, per: i64| DerivedExpr::Ceil {
        operand: Box::new(DerivedExpr::DivConst {
            operand: Box::new(DerivedExpr::Input {
                name: name.to_owned(),
            }),
            divisor: Decimal::from(per),
        }),
    };
    let input = |name: &str, unit: &str| DerivedInput {
        name: name.to_owned(),
        usage_type_ref: format!("gts.cf.core.uc.usage_record.v1~cf.test.{name}.v1"),
        granule_fold: GranuleFold::Sum,
        max_hold_seconds: None,
        unit: unit.to_owned(),
    };
    FrozenDerivedDeclaration {
        code: "cloudlet".to_owned(),
        version: 1,
        digest: "ff".repeat(32),
        declaration: DerivedDeclaration {
            output_unit: "cloudlet-hour".to_owned(),
            granularity: Granularity::Hour,
            inputs: vec![input("ram", "MB-hour"), input("cpu", "MHz-hour")],
            formula: DerivedExpr::Max {
                operands: vec![share("ram", 128), share("cpu", 400)],
            },
            output_scale: 0,
            output_round: RoundMode::Up,
        },
    }
}
