//! The fixed step order of generation 1 (feature 02 §2.1, §3.1; rating DESIGN 01 §3.6, PRD §17.1).
//!
//! `evaluate` checks the engine and the input, then rates every slice of the child as one line:
//! **bind** (the line's binding from `PricingInput`, `rating-val-02`), **meter** (the committed
//! quantity of a recurring slice, the derived meter of a usage slice), **model** (the price model
//! over exact fractions at the slice's place on the band axis, then the covered fraction of a
//! recurring slice), **guards** (deferred adjustments, bundles, non-negative, precision) and
//! lineage. Step 4, the minimum-fee floor, runs at parent roll-up over the per-price totals
//! returned here (`floor`, T-D-78). No configuration reorders a step; the first error ends the
//! evaluation and nothing is persisted.
//!
//! The slices of a child share one band axis (DESIGN 03 §4.3): a slice's offset is the quantity of
//! the earlier slices, after the earlier members of its window group; a volume price selects the
//! band of the window total (or the group total). Split points add slices and never reset it.

// @cpt-algo:cpt-cf-bss-rating-algo-core-step-pipeline:p1
// @cpt-algo:cpt-cf-bss-rating-algo-core-selection:p1

use std::collections::BTreeMap;

use uuid::Uuid;

use crate::error::EvaluationError;
use crate::exact::{ExactMoney, ExactQuantity};
use crate::generation;
use crate::geometry;
use crate::governance;
use crate::guards;
use crate::input::{
    ChargeKind, ChildKind, CoreBinding, CoreCell, CoreCurrency, CorePriceModel, CoreResolve,
    EvaluationInput, Interval, PricePin, PriceState, SliceQuantity, SuspensionPosture,
};
use crate::model::{self, Placement};
use crate::outcome::{
    EvaluationOutcome, GranuleOutput, Lineage, MoneyText, PolicyRef, PriceTotal, Proration,
    RatedLine, SnapshotBinding, SnapshotBody, Step, StepRecord,
};
use crate::validate::{self, RatedCell, Validated};

/// Evaluates one child (see the module documentation).
pub fn evaluate(input: &EvaluationInput) -> Result<EvaluationOutcome, EvaluationError> {
    // @cpt-begin:cpt-cf-bss-rating-flow-core-evaluate:p1:inst-ev-validate
    generation::check(&input.engine, input.budget)?;
    let validated = validate::input(input)?;
    // @cpt-end:cpt-cf-bss-rating-flow-core-evaluate:p1:inst-ev-validate
    // @cpt-begin:cpt-cf-bss-rating-flow-core-evaluate:p1:inst-ev-steps
    // @cpt-begin:cpt-cf-bss-rating-algo-core-step-pipeline:p1:inst-sp-layout
    let period_seconds = geometry::seconds(&input.fact.period, "fact period")?;
    let mut metered = Vec::with_capacity(input.quantities.slices.len());
    for slice in &input.quantities.slices {
        let bound = bind(input, &validated.rated, slice)?;
        // @cpt-begin:cpt-cf-bss-rating-flow-core-rule-verdicts:p1:inst-rv-target
        // Nothing is registered in Pricing's publish pipeline: the check runs here, at evaluation.
        // @cpt-begin:cpt-cf-bss-rating-flow-core-rule-verdicts:p1:inst-rv-return
        governance::check_injective(bound.resolve, bound.cell, bound.binding)?;
        // @cpt-end:cpt-cf-bss-rating-flow-core-rule-verdicts:p1:inst-rv-return
        // @cpt-end:cpt-cf-bss-rating-flow-core-rule-verdicts:p1:inst-rv-target
        let quantity = meter(input, &validated, slice, bound.binding)?;
        metered.push((slice, bound, quantity));
    }
    let (mut offset, selecting_total) = band_axis(input, &metered)?;
    let mut lines = Vec::with_capacity(metered.len());
    let mut totals = Totals::default();
    for (slice, bound, quantity) in metered {
        let axis = Placement {
            quantity: &quantity.quantity,
            offset: &offset,
            selecting_total: &selecting_total,
        };
        let line = rate_slice(input, slice, &bound, &quantity, axis, period_seconds)?;
        // @cpt-begin:cpt-cf-bss-rating-algo-core-model:p1:inst-mod-return
        if input.child.kind == ChildKind::UsageWindow {
            offset = offset.checked_add(&quantity.quantity, "band offset")?;
        }
        // @cpt-end:cpt-cf-bss-rating-algo-core-model:p1:inst-mod-return
        totals.add(&line)?;
        lines.push(line.rated);
    }
    // @cpt-end:cpt-cf-bss-rating-algo-core-step-pipeline:p1:inst-sp-layout
    // @cpt-end:cpt-cf-bss-rating-flow-core-evaluate:p1:inst-ev-steps
    // @cpt-begin:cpt-cf-bss-rating-flow-core-evaluate:p1:inst-ev-return
    // @cpt-begin:cpt-cf-bss-rating-algo-core-step-pipeline:p1:inst-sp-return
    Ok(EvaluationOutcome {
        lines,
        obligations: Vec::new(),
        price_totals: totals.finish(period_seconds)?,
    })
    // @cpt-end:cpt-cf-bss-rating-algo-core-step-pipeline:p1:inst-sp-return
    // @cpt-end:cpt-cf-bss-rating-flow-core-evaluate:p1:inst-ev-return
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
    // @cpt-begin:cpt-cf-bss-rating-algo-core-selection:p1:inst-sel-phase
    let resolve = resolves
        .iter()
        .rfind(|resolve| resolve.resolve_date <= date)
        .ok_or_else(|| EvaluationError::PriceUncovered {
            detail: format!("no resolve at or before {date}"),
        })?;
    // @cpt-end:cpt-cf-bss-rating-algo-core-selection:p1:inst-sel-phase
    // @cpt-begin:cpt-cf-bss-rating-algo-core-selection:p1:inst-sel-candidates
    let cell = rated
        .in_resolve(resolve)
        .ok_or_else(|| EvaluationError::PriceUncovered {
            detail: format!(
                "the resolve of {} has no cell for item {item} value {value:?}",
                resolve.resolve_date
            ),
        })?;
    // @cpt-end:cpt-cf-bss-rating-algo-core-selection:p1:inst-sel-candidates
    // @cpt-begin:cpt-cf-bss-rating-algo-core-selection:p1:inst-sel-assert
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
    // @cpt-end:cpt-cf-bss-rating-algo-core-selection:p1:inst-sel-assert
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
    // @cpt-begin:cpt-cf-bss-rating-algo-core-selection:p1:inst-sel-binding
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
    // @cpt-end:cpt-cf-bss-rating-algo-core-selection:p1:inst-sel-binding
    // @cpt-begin:cpt-cf-bss-rating-algo-core-selection:p1:inst-sel-return
    Ok(Bound {
        resolve,
        cell,
        binding,
        accepted,
    })
    // @cpt-end:cpt-cf-bss-rating-algo-core-selection:p1:inst-sel-return
}
// @cpt-end:cpt-cf-bss-rating-algo-core-step-pipeline:p1:inst-sp-select

/// A slice's metered quantity and, for a usage slice, the granule outputs it sums.
struct Metered {
    quantity: ExactQuantity,
    granules: Vec<GranuleOutput>,
}

/// Step 2: the slice quantity the model prices. A recurring slice prices the committed quantity
/// in force over it (a flat price needs none); a usage slice sums its derived meter per granule,
/// and that sum must be the quantity the slice was counted with.
fn meter(
    input: &EvaluationInput,
    validated: &Validated<'_>,
    slice: &SliceQuantity,
    binding: &CoreBinding,
) -> Result<Metered, EvaluationError> {
    let Some(evaluator) = &validated.meter else {
        let quantity = match validate::committed_quantity(input, slice)? {
            Some(committed) => committed.clone(),
            None if matches!(binding.price.model, CorePriceModel::Flat { .. }) => slice.q.clone(),
            None => {
                return Err(EvaluationError::LayoutMismatch {
                    detail: "a recurring line priced per unit has no committed quantity".into(),
                });
            }
        };
        return Ok(Metered {
            quantity,
            granules: Vec::new(),
        });
    };
    let metered = evaluator.slice(&slice.granules)?;
    if metered.quantity != slice.q {
        return Err(EvaluationError::LayoutMismatch {
            detail: format!(
                "slice [{}, {}) was counted as {}, its granules' derived outputs sum to {}",
                slice.slice_start, slice.slice_end, slice.q, metered.quantity
            ),
        });
    }
    Ok(Metered {
        quantity: metered.quantity,
        granules: metered.granules,
    })
}

/// Where the child's band axis starts and the total a volume price selects its band by: the
/// window group's prefix and total, or zero and the sum of the child's own slices. Only usage has
/// bands; a recurring line's committed quantities are priced per slice and never summed.
fn band_axis(
    input: &EvaluationInput,
    metered: &[(&SliceQuantity, Bound<'_>, Metered)],
) -> Result<(ExactQuantity, ExactQuantity), EvaluationError> {
    if input.child.kind != ChildKind::UsageWindow {
        return Ok((ExactQuantity::zero(), ExactQuantity::zero()));
    }
    if let Some(context) = &input.group_context {
        return Ok((
            context.prefix_before_this_member.clone(),
            context.group_total.clone(),
        ));
    }
    let total = ExactQuantity::sum(
        metered.iter().map(|(_, _, quantity)| &quantity.quantity),
        "window total",
    )?;
    Ok((ExactQuantity::zero(), total))
}

/// One rated line and what the per-price totals need from it.
struct Line {
    rated: RatedLine,
    amount: ExactMoney,
    minimum_fee: Option<ExactMoney>,
    covered: Vec<Interval>,
}

/// Rates one bound and metered slice through the model, proration and guards.
fn rate_slice(
    input: &EvaluationInput,
    slice: &SliceQuantity,
    bound: &Bound<'_>,
    metered: &Metered,
    axis: Placement<'_>,
    period_seconds: u64,
) -> Result<Line, EvaluationError> {
    let mut steps = vec![
        StepRecord {
            step: Step::Bind,
            pre: None,
            post: None,
        },
        StepRecord {
            step: Step::Meter,
            pre: None,
            post: None,
        },
    ];
    // @cpt-begin:cpt-cf-bss-rating-algo-core-step-pipeline:p1:inst-sp-model
    let price = &bound.binding.price;
    // @cpt-begin:cpt-cf-bss-rating-algo-core-model:p1:inst-mod-composite
    let mut priced = model::price_slice(&price.model, &price.currency, axis, input.budget)?;
    // @cpt-end:cpt-cf-bss-rating-algo-core-model:p1:inst-mod-composite
    steps.push(StepRecord {
        step: Step::Model,
        pre: None,
        post: Some(priced.amount.to_lineage()),
    });
    let span = Interval {
        start: slice.slice_start,
        end: slice.slice_end,
    };
    let (amount, proration, covered) = if input.child.kind == ChildKind::PeriodLine {
        // @cpt-begin:cpt-cf-bss-rating-algo-core-geometry:p1:inst-geo-prorate
        let suspended: &[Interval] = match input.fact.suspension_posture {
            SuspensionPosture::PauseRecurring => &input.fact.suspensions,
            SuspensionPosture::Continue => &[],
        };
        let covered = geometry::covered(span, suspended);
        let covered_seconds = geometry::total_seconds(&covered);
        let proration = Proration {
            covered_seconds,
            period_seconds,
        };
        let prorated = priced
            .amount
            .times_fraction(proration, input.budget, "proration")?;
        // @cpt-end:cpt-cf-bss-rating-algo-core-geometry:p1:inst-geo-prorate
        steps.push(StepRecord {
            step: Step::Prorate,
            pre: Some(priced.amount.to_lineage()),
            post: Some(prorated.to_lineage()),
        });
        (prorated, Some(proration), covered)
    } else {
        (priced.amount.clone(), None, vec![span])
    };
    // @cpt-end:cpt-cf-bss-rating-algo-core-step-pipeline:p1:inst-sp-model
    governance::check_bundle(&input.fact)?;
    guards::deferred_adjustments(&input.fact, bound.binding)?;
    let guards::Guarded {
        amount,
        wire,
        clamped_from,
    } = guards::amount(amount, &mut steps)?;
    let lineage = Lineage {
        price_id: price.price_id,
        money_digest: price.money_digest.clone(),
        resolve_date: bound.resolve.resolve_date,
        via_default: bound.binding.via_default,
        steps,
        proration,
        band_placement: priced.placement.take(),
        granules: metered.granules.clone(),
        clamped_from,
    };
    let rated = rated_line(input, slice, bound, priced, wire, lineage);
    Ok(Line {
        rated,
        amount,
        minimum_fee: price
            .minimum_fee
            .map(|fee| ExactMoney::from_price(fee, &price.currency)),
        covered,
    })
}

/// The line with its invoice inputs, null ASC 606 references and snapshot (feature 02 §3.7).
// @cpt-begin:cpt-cf-bss-rating-flow-core-evaluate:p1:inst-ev-snapshot
fn rated_line(
    input: &EvaluationInput,
    slice: &SliceQuantity,
    bound: &Bound<'_>,
    priced: model::Priced,
    exact_amount: MoneyText,
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
        quantity: priced.quantity,
        billable_quantity: priced.billable_quantity,
        band_offset: lineage
            .band_placement
            .as_ref()
            .map(|placement| placement.offset.clone()),
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
    // A derived meter's id already names its version (`products.derived/<code>@<n>`).
    let component = match (&binding.kind, &binding.meter) {
        (ChargeKind::Usage, Some(meter)) => meter.usage_type_id.clone(),
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

/// Per price and stored scale: the exact rated sum, the covered time and the minimum fee.
#[derive(Default)]
struct Totals {
    entries: BTreeMap<(Uuid, u8), Total>,
}

/// What one price at one stored scale rated in the child.
struct Total {
    rated: ExactMoney,
    covered: Vec<Interval>,
    minimum_fee: Option<ExactMoney>,
}

impl Totals {
    /// Adds a line to its price and scale: its amount, exactly, and its covered time.
    fn add(&mut self, line: &Line) -> Result<(), EvaluationError> {
        let currency: &CoreCurrency = line.amount.currency();
        let key = (line.rated.price_id, currency.scale);
        let total = match self.entries.remove(&key) {
            None => Total {
                rated: line.amount.clone(),
                covered: geometry::union(line.covered.iter().copied()),
                minimum_fee: line.minimum_fee.clone(),
            },
            Some(total) => Total {
                rated: total.rated.checked_add(&line.amount, "price total")?,
                covered: geometry::union(
                    total
                        .covered
                        .into_iter()
                        .chain(line.covered.iter().copied()),
                ),
                minimum_fee: total.minimum_fee,
            },
        };
        self.entries.insert(key, total);
        Ok(())
    }

    /// The totals in price and scale order, each amount within the delivery bound.
    fn finish(self, period_seconds: u64) -> Result<Vec<PriceTotal>, EvaluationError> {
        self.entries
            .into_iter()
            .map(|((price_id, currency_scale), total)| {
                Ok(PriceTotal {
                    price_id,
                    currency_scale,
                    rated: total.rated.to_wire("price total")?,
                    covered: total.covered,
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
