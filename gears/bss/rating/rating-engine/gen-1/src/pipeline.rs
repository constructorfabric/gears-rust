//! The fixed step order of generation 1 (feature 02 §2.1, §3.1; rating DESIGN 01 §3.6, PRD §17.1).
//!
//! `evaluate` checks the engine and the input, then rates every slice of the child as one line:
//! **bind** (the line's binding from `PricingInput`, `rating-val-02`), **meter**, **model** (the
//! price model over exact fractions, then the covered fraction of a period line), **guards**
//! (deferred adjustments, bundles, non-negative, precision) and lineage. Step 4, the minimum-fee
//! floor, runs at parent roll-up over the per-price totals returned here (T-D-78). No
//! configuration reorders a step; the first error ends the evaluation and nothing is persisted.
//!
//! Generation 1 is the generation under construction. Feature 02's pricing semantics (§5.2) that
//! are not in it yet fail closed: the derived meter of a usage line, the tiered and package models,
//! paused-suspension proration, committed-quantity changes and one-time children with
//! `unsupported_primitive`; slices that `split_points` would cut at a handover with
//! `layout_mismatch`. `window_geometry`, `split_points` and the floor function are not here yet.

// @cpt-algo:cpt-cf-bss-rating-algo-core-step-pipeline:p1

use std::collections::BTreeMap;

use time::OffsetDateTime;
use uuid::Uuid;

use crate::budget::ArithmeticBudget;
use crate::error::EvaluationError;
use crate::exact::{ExactMoney, ExactQuantity};
use crate::governance;
use crate::guards;
use crate::input::{
    ChargeKind, ChildKind, CoreBinding, CoreCell, CoreCurrency, CorePriceModel, CoreResolve,
    EvaluationInput, Interval, PricePin, PriceState, SliceQuantity,
};
use crate::model;
use crate::outcome::{
    EvaluationOutcome, Lineage, PolicyRef, PriceTotal, Proration, RatedLine, SnapshotBinding,
    SnapshotBody, Step, StepRecord,
};
use crate::validate::{self, RatedCell};
use crate::{ENGINE_DIGEST, GENERATION};

/// Evaluates one child (see the module documentation).
pub fn evaluate(input: &EvaluationInput) -> Result<EvaluationOutcome, EvaluationError> {
    // @cpt-begin:cpt-cf-bss-rating-flow-core-evaluate:p1:inst-ev-validate
    check_engine(input)?;
    let rated = validate::input(input)?;
    // @cpt-end:cpt-cf-bss-rating-flow-core-evaluate:p1:inst-ev-validate
    // @cpt-begin:cpt-cf-bss-rating-flow-core-evaluate:p1:inst-ev-steps
    // @cpt-begin:cpt-cf-bss-rating-algo-core-step-pipeline:p1:inst-sp-layout
    let period_seconds = seconds(&input.fact.period, "fact period")?;
    let mut lines = Vec::with_capacity(input.quantities.slices.len());
    let mut totals = Totals::default();
    for slice in &input.quantities.slices {
        let line = rate_slice(input, &rated, slice, period_seconds)?;
        totals.add(&line)?;
        lines.push(line.rated);
    }
    // @cpt-end:cpt-cf-bss-rating-algo-core-step-pipeline:p1:inst-sp-layout
    // @cpt-end:cpt-cf-bss-rating-flow-core-evaluate:p1:inst-ev-steps
    // @cpt-begin:cpt-cf-bss-rating-flow-core-evaluate:p1:inst-ev-return
    Ok(EvaluationOutcome {
        lines,
        obligations: Vec::new(),
        price_totals: totals.finish(period_seconds)?,
    })
    // @cpt-end:cpt-cf-bss-rating-flow-core-evaluate:p1:inst-ev-return
}

/// The input must name this generation, its digest and its budget; another is another engine,
/// never substituted.
fn check_engine(input: &EvaluationInput) -> Result<(), EvaluationError> {
    if input.engine.engine_generation != GENERATION || input.engine.engine_digest != ENGINE_DIGEST {
        return Err(EvaluationError::EngineUnavailable {
            detail: format!(
                "the input names generation {} with digest {}; this is generation {GENERATION} \
                 with digest {ENGINE_DIGEST}",
                input.engine.engine_generation, input.engine.engine_digest
            ),
        });
    }
    if input.budget != ArithmeticBudget::GEN_1 {
        return Err(EvaluationError::EngineUnavailable {
            detail: format!(
                "the input carries budget {:?}; generation {GENERATION} computes under {:?}",
                input.budget,
                ArithmeticBudget::GEN_1
            ),
        });
    }
    Ok(())
}

/// The binding a slice is priced by.
struct Bound<'a> {
    resolve: &'a CoreResolve,
    cell: &'a CoreCell,
    binding: &'a CoreBinding,
    /// The accepted pin of a period-start slice, recorded in the snapshot.
    accepted: Option<PricePin>,
}

/// Step 1: the slice's binding — the rated cell in the resolve with the latest date at or before
/// the slice start. Rating selects nothing: an uncovered cell, or a price that is not approved,
/// is never free (`price_uncovered`), and a slice bound by the period-start resolve must be priced
/// by the price the fact's accepted binding names (`binding_mismatch`, V33). A slice after an
/// `ends_on` is priced by the successor and never compared (V53).
// @cpt-begin:cpt-cf-bss-rating-algo-core-step-pipeline:p1:inst-sp-select
fn bind<'a>(
    input: &'a EvaluationInput,
    rated: &RatedCell<'a>,
    slice: &SliceQuantity,
) -> Result<Bound<'a>, EvaluationError> {
    let (item, value) = (rated.item, rated.value);
    let date = slice.slice_start.date();
    let resolves = &input.pricing.resolves;
    let resolve = resolves
        .iter()
        .rfind(|resolve| resolve.resolve_date <= date)
        .ok_or_else(|| EvaluationError::PriceUncovered {
            detail: format!("no resolve at or before {date}"),
        })?;
    let cell = rated
        .in_resolve(resolve)
        .ok_or_else(|| EvaluationError::PriceUncovered {
            detail: format!(
                "the resolve of {} has no cell for item {item} value {value:?}",
                resolve.resolve_date
            ),
        })?;
    let binding = cell
        .binding
        .as_ref()
        .filter(|binding| binding.price.state == PriceState::Approved)
        .ok_or_else(|| EvaluationError::PriceUncovered {
            detail: format!(
                "item {item} value {value:?} has no approved price on {}; never free",
                resolve.resolve_date
            ),
        })?;
    let expected = match input.child.kind {
        ChildKind::UsageWindow => ChargeKind::Usage,
        ChildKind::PeriodLine => ChargeKind::Recurring,
        ChildKind::OneTime => ChargeKind::OneTime,
    };
    if binding.kind != expected {
        return Err(EvaluationError::UnsupportedPrimitive {
            detail: format!(
                "a {:?} binding on a {:?} child",
                binding.kind, input.child.kind
            ),
        });
    }
    // Validation makes the resolve dates a strictly increasing chain from the period start.
    let period_start = resolves
        .first()
        .is_some_and(|first| first.resolve_date == resolve.resolve_date);
    let accepted = if period_start {
        let pin = rated
            .accepted
            .filter(|pin| pin.price_id == binding.price.price_id)
            .ok_or_else(|| EvaluationError::BindingMismatch {
                detail: format!(
                    "the period-start resolve binds item {item} value {value:?} to price {}; the \
                     fact accepted {}",
                    binding.price.price_id,
                    rated.accepted.map_or_else(
                        || "no price for it".to_owned(),
                        |pin| pin.price_id.to_string()
                    )
                ),
            })?;
        Some(pin.clone())
    } else {
        None
    };
    Ok(Bound {
        resolve,
        cell,
        binding,
        accepted,
    })
}
// @cpt-end:cpt-cf-bss-rating-algo-core-step-pipeline:p1:inst-sp-select

/// Step 2: the slice quantity the model prices. A recurring line prices the fact's committed
/// quantity (a flat price needs none); a usage line evaluates its derived meter per granule,
/// which this build of generation 1 does not do yet.
fn meter(
    input: &EvaluationInput,
    slice: &SliceQuantity,
    binding: &CoreBinding,
) -> Result<ExactQuantity, EvaluationError> {
    match (input.child.kind, &input.fact.quantity) {
        (ChildKind::PeriodLine | ChildKind::OneTime, Some(committed)) => Ok(committed.clone()),
        (ChildKind::PeriodLine | ChildKind::OneTime, None) => match binding.price.model {
            CorePriceModel::Flat { .. } => Ok(slice.q.clone()),
            _ => Err(EvaluationError::LayoutMismatch {
                detail: "a recurring line priced per unit has no committed quantity".into(),
            }),
        },
        (ChildKind::UsageWindow, _) => Err(EvaluationError::UnsupportedPrimitive {
            detail: "derived meter evaluation per granule is not in this build of generation 1"
                .into(),
        }),
    }
}

/// One rated line and what the per-price totals need from it.
struct Line {
    rated: RatedLine,
    amount: ExactMoney,
    minimum_fee: Option<ExactMoney>,
    covered_seconds: u64,
}

/// Rates one slice through the fixed order.
fn rate_slice(
    input: &EvaluationInput,
    rated: &RatedCell<'_>,
    slice: &SliceQuantity,
    period_seconds: u64,
) -> Result<Line, EvaluationError> {
    let mut steps = Vec::new();
    let bound = bind(input, rated, slice)?;
    governance::check_injective(bound.resolve, bound.cell, bound.binding)?;
    steps.push(StepRecord {
        step: Step::Bind,
        pre: None,
        post: None,
    });
    // @cpt-begin:cpt-cf-bss-rating-algo-core-step-pipeline:p1:inst-sp-model
    let quantity = meter(input, slice, bound.binding)?;
    steps.push(StepRecord {
        step: Step::Meter,
        pre: None,
        post: None,
    });
    let price = &bound.binding.price;
    let priced = model::price(&price.model, &price.currency, &quantity, input.budget)?;
    steps.push(StepRecord {
        step: Step::Model,
        pre: None,
        post: Some(priced.amount.to_lineage()?),
    });
    let covered_seconds = seconds(
        &Interval {
            start: slice.slice_start,
            end: slice.slice_end,
        },
        "slice",
    )?;
    let (amount, proration) = if input.child.kind == ChildKind::PeriodLine {
        let prorated = priced.amount.times_ratio(
            covered_seconds,
            period_seconds,
            input.budget,
            "proration",
        )?;
        steps.push(StepRecord {
            step: Step::Prorate,
            pre: Some(priced.amount.to_lineage()?),
            post: Some(prorated.to_lineage()?),
        });
        let proration = Proration {
            covered_seconds,
            period_seconds,
        };
        (prorated, Some(proration))
    } else {
        (priced.amount.clone(), None)
    };
    // @cpt-end:cpt-cf-bss-rating-algo-core-step-pipeline:p1:inst-sp-model
    governance::check_bundle(&input.fact)?;
    guards::deferred_adjustments(&input.fact, bound.binding)?;
    let guarded = guards::amount(amount, &mut steps)?;
    let lineage = Lineage {
        price_id: price.price_id,
        money_digest: price.money_digest.clone(),
        resolve_date: bound.resolve.resolve_date,
        via_default: bound.binding.via_default,
        steps,
        proration,
        clamped_from: guarded.clamped_from.clone(),
    };
    let rated = rated_line(input, slice, &bound, priced, guarded.wire.clone(), lineage);
    Ok(Line {
        rated,
        amount: guarded.amount,
        minimum_fee: price
            .minimum_fee
            .map(|fee| ExactMoney::from_price(fee, &price.currency)),
        covered_seconds,
    })
}

/// The line with its invoice inputs, null ASC 606 references and snapshot (feature 02 §3.7).
// @cpt-begin:cpt-cf-bss-rating-flow-core-evaluate:p1:inst-ev-snapshot
fn rated_line(
    input: &EvaluationInput,
    slice: &SliceQuantity,
    bound: &Bound<'_>,
    priced: model::Priced,
    exact_amount: crate::outcome::MoneyText,
    lineage: Lineage,
) -> RatedLine {
    let binding = bound.binding;
    let price = &binding.price;
    let dimension_value = bound.cell.selection.dimension_value.clone();
    let (currency, currency_scale) = (exact_amount.currency.clone(), exact_amount.currency_scale);
    // @cpt-begin:cpt-cf-bss-rating-algo-core-governance-passthrough:p2:inst-gov-asc606
    RatedLine {
        line_key: line_key(input.fact.fact_id, binding, dimension_value.as_deref()),
        slice_start: slice.slice_start,
        slice_end: slice.slice_end,
        item_id: binding.item_id,
        dimension_key: binding.dimension_key.clone(),
        dimension_value: dimension_value.clone(),
        sku_id: binding.sku_id,
        sku_version: binding.sku_version,
        plan_id: input.pricing.plan_id,
        revision_id: input.pricing.revision_id,
        price_id: price.price_id,
        price_book_entry_id: price.price_book_entry_id,
        charge_kind: binding.kind,
        model_kind: priced.kind,
        quantity: priced.quantity.clone(),
        billable_quantity: priced.quantity,
        band_offset: None,
        exact_amount,
        currency,
        currency_scale,
        unit: binding.unit.clone(),
        gl_code: binding.invoice.gl_code.clone(),
        tax_category: binding.invoice.tax_category.clone(),
        invoice_line_template: binding.invoice.template.clone(),
        performance_obligation_ref: None,
        ssp_snapshot_pointer: None,
        snapshot: SnapshotBody {
            plan_id: input.pricing.plan_id,
            revision_id: input.pricing.revision_id,
            resolve_date: bound.resolve.resolve_date,
            bindings: vec![SnapshotBinding {
                item_id: binding.item_id,
                dimension_value,
                price_id: price.price_id,
                price_book_entry_id: price.price_book_entry_id,
                money_digest: price.money_digest.clone(),
                sku_id: binding.sku_id,
                sku_version: binding.sku_version,
                usage_rating_policy: binding.usage_policy.as_ref().map(|policy| PolicyRef {
                    policy_id: policy.policy_id,
                    version: policy.version,
                    digest: policy.digest.clone(),
                }),
                invoice_inputs: binding.invoice.clone(),
                meter: binding.meter.clone(),
                derived_declaration_digest: input
                    .derived
                    .as_ref()
                    .map(|derived| derived.digest.clone()),
            }],
            binding_digest: input.pricing.binding_of_record.clone(),
            fact_id: input.fact.fact_id,
            fact_version: input.fact.fact_version,
            accepted_binding: bound.accepted.clone(),
            engine_generation: input.engine.engine_generation,
            engine_digest: input.engine.engine_digest.clone(),
        },
        lineage,
    }
    // @cpt-end:cpt-cf-bss-rating-algo-core-governance-passthrough:p2:inst-gov-asc606
}
// @cpt-end:cpt-cf-bss-rating-flow-core-evaluate:p1:inst-ev-snapshot

/// `{fact_id}|{meter or item}|{dimension_key}={dimension_value}|{kind}|{gl_code}/{tax_category}|{currency_scale}`
/// (T-D-58, DESIGN 15 §4.3): stable across revisions and windows; no hour, revision or price.
fn line_key(fact_id: Uuid, binding: &CoreBinding, dimension_value: Option<&str>) -> String {
    let component = match (&binding.kind, &binding.meter) {
        (ChargeKind::Usage, Some(meter)) => format!("{}@{}", meter.usage_type_id, meter.version),
        _ => binding.item_id.to_string(),
    };
    format!(
        "{fact_id}|{component}|{}={}|{}|{}/{}|{}",
        binding.dimension_key.as_deref().unwrap_or_default(),
        dimension_value.unwrap_or_default(),
        governance::kind_name(binding.kind),
        binding.invoice.gl_code,
        binding.invoice.tax_category,
        binding.price.currency.scale
    )
}

/// The whole UTC seconds of a half-open interval.
fn seconds(interval: &Interval, what: &str) -> Result<u64, EvaluationError> {
    let difference = unix_seconds(interval.end).checked_sub(unix_seconds(interval.start));
    difference
        .and_then(|difference| u64::try_from(difference).ok())
        .ok_or_else(|| EvaluationError::PeriodGeometryMismatch {
            detail: format!(
                "the {what} [{}, {}) has no length",
                interval.start, interval.end
            ),
        })
}

fn unix_seconds(instant: OffsetDateTime) -> i64 {
    instant.unix_timestamp()
}

/// Per price and stored scale: the exact rated sum, the covered seconds and the minimum fee.
#[derive(Default)]
struct Totals {
    entries: BTreeMap<(Uuid, u8), Total>,
}

struct Total {
    rated: ExactMoney,
    covered_seconds: u64,
    minimum_fee: Option<ExactMoney>,
}

impl Totals {
    fn add(&mut self, line: &Line) -> Result<(), EvaluationError> {
        let currency: &CoreCurrency = line.amount.currency();
        let key = (line.rated.price_id, currency.scale);
        let total = match self.entries.remove(&key) {
            None => Total {
                rated: line.amount.clone(),
                covered_seconds: line.covered_seconds,
                minimum_fee: line.minimum_fee.clone(),
            },
            Some(total) => Total {
                rated: total.rated.checked_add(&line.amount, "price total")?,
                covered_seconds: total
                    .covered_seconds
                    .checked_add(line.covered_seconds)
                    .ok_or_else(|| EvaluationError::PeriodGeometryMismatch {
                        detail: "the covered seconds of a price overflow".into(),
                    })?,
                minimum_fee: total.minimum_fee,
            },
        };
        self.entries.insert(key, total);
        Ok(())
    }

    fn finish(self, period_seconds: u64) -> Result<Vec<PriceTotal>, EvaluationError> {
        self.entries
            .into_iter()
            .map(|((price_id, currency_scale), total)| {
                Ok(PriceTotal {
                    price_id,
                    currency_scale,
                    rated: total.rated.to_wire("price total")?,
                    covered_seconds: total.covered_seconds,
                    period_seconds,
                    minimum_fee: total
                        .minimum_fee
                        .map(|fee| fee.to_wire("minimum fee"))
                        .transpose()?,
                })
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;
