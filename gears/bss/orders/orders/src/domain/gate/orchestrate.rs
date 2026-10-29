//! The orchestrator's pure half (S3-11/S3-12; F03 §3.6 steps 2–5; D-215, D-180, M2-6): what one
//! gate run asks of each port, derived from the basket and the answers already in hand, and the
//! [`AssessmentInput`] the collected answers assemble into. No call, no storage, no clock: the
//! async shell (`infra::gate::orchestrate`) calls the ports in F03 order, one batched call per
//! port per basket, and feeds their answers back through these functions.
//!
//! "Not asked" (D-215, D-228): with the payer relationship unconfirmed the shell never calls the
//! payer-dependent ports (contracts, overlap key, occupancy) nor billing terms, whose query
//! carries the payer and the contract (D-215 item 2 withholds any payer-related commercial fact
//! until the relationship is confirmed), and it calls the evaluation only when
//! [`evaluation_request`] can be built. [`Answers`] states a port it did not ask as `None`;
//! [`assemble`] retains evaluation prerequisite reasons in `PortAnswer::Skipped`; other unasked
//! ports use `PortAnswer::Unavailable(Unavailable::Missing)`. That encoding is
//! safe because [`assess`](super::assess::assess) ignores the payer-dependent answers while the
//! relationship is unconfirmed: their rows carry predicate 1's reason and their engine inputs are
//! `Resolved`, the composite gate resolving them by withholding (its own predicate carries
//! predicate 1's failure; D-113 `Precluded` is reserved for inputs an earlier-registered guard
//! makes moot). Unasked terms are withheld, not a down port, so the composed pin rows carry
//! predicate 1's reason too and `PinComposition` and `Evaluation` are `Resolved` (D-228 item 6);
//! with the relationship confirmed, an unasked evaluation carries its prerequisite reason.
//! Whenever the primary is a definitive local refusal every composite-gate input is
//! presented `Resolved`, so the engine settles the gate's 400, not an input's 503 (D-230). The
//! shell never logs a port it did not call.
use super::answers::{
    AcceptancePolicy, AcceptancePolicyQuery, AssessmentAnswer, AssessmentLine, AssessmentQuery,
    BillingTermsQuery, ContractFacts, ContractQuery, EvaluationBinding, EvaluationChain,
    EvaluationChargeKind, EvaluationCycle, EvaluationItem, EvaluationLine, Market,
    OccupancyObservation, OccupancyQuery, OrderEvaluation, OrderEvaluationRequest, OverlapKey,
    OverlapKeyLine, PayerProfile, PayerProfileQuery, ResolvedTerms, TaxQuery, TermIntentInput,
};
use super::assess::{AssessmentInput, BasketLine};
use super::commercial::CommercialError;
use super::ports::{GatePort, PortAnswer, Unavailable};
use super::pricing_map::{CatalogAnswer, LineCatalogRequest, PinOutcome, map_line};
use super::vector::Operation;
use crate::domain::dates::DateBasis;
use bss_orders_sdk::authoring::{self, AuthoredTerm};
use bss_orders_sdk::commercial::{CodecError, term::TermIntent as SdkTermIntent};
use bss_pricing_sdk::acceptance::Term;
use bss_pricing_sdk::read::{AcceptedBinding, ChargeKind};
use bss_pricing_sdk::terms::BillingCycle;
use std::collections::{BTreeMap, BTreeSet};
use time::{Date, OffsetDateTime, Time};
use toolkit_macros::domain_model;
use uuid::Uuid;

/// The producer profile the gate asks the D-189 assessment for (proposed; no PR names one). The
/// ask that settles the name is `cpt-cf-bss-orders-upreq-pricing-purchase-assessment`.
pub const ASSESSMENT_PROFILE: &str = "orders-gate/1";

/// The three tenant axes of an order.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TenantAxes {
    /// The tenant that receives the resources.
    pub resource: Uuid,
    /// The paying tenant.
    pub payer: Uuid,
    /// The selling tenant.
    pub seller: Uuid,
}

/// Whether authored intent is representable by the billing-terms port.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineTermIntent {
    /// Exact authored intent, expressed in the port's month-based form.
    Stated(TermIntentInput),
    /// Missing authored term; seller policy decides.
    Unstated,
    /// Authored intent cannot be represented exactly; acceptance must refuse.
    Unsupported,
}

/// The independent authored term state and invoice cycle.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineTerms {
    /// Tri-state authored intent.
    pub intent: LineTermIntent,
    /// The invoice cycle the caller chose, if any.
    pub cycle: Option<EvaluationCycle>,
}
impl LineTerms {
    /// No authored intent or cycle.
    pub const UNSTATED: Self = Self {
        intent: LineTermIntent::Unstated,
        cycle: None,
    };
}

/// Resolve authored intent using the SDK's exact invoice-period conversion (D-193), then
/// express the supported result in the proposed resolver's month-based query. Missing is
/// seller policy; authored incomplete or unsupported intent is never seller policy.
#[must_use]
pub fn line_terms(
    term: Option<&AuthoredTerm>,
    cycle: Option<authoring::BillingCycle>,
) -> LineTerms {
    let pricing_cycle = cycle.map(|c| match c {
        authoring::BillingCycle::Month => BillingCycle::Month,
        authoring::BillingCycle::Year => BillingCycle::Year,
    });
    let intent = match SdkTermIntent::from(term.copied()).resolve(pricing_cycle) {
        Ok(Term::Rolling) => LineTermIntent::Stated(TermIntentInput::Rolling),
        Ok(Term::FixedPeriods { count }) => {
            let months = match pricing_cycle {
                Some(BillingCycle::Month) => Some(count),
                Some(BillingCycle::Year) => count.checked_mul(12),
                None => None,
            };
            months.map_or(LineTermIntent::Unsupported, |months| {
                LineTermIntent::Stated(TermIntentInput::Fixed { months })
            })
        }
        Err(CodecError::Incomplete) if term.is_none() => LineTermIntent::Unstated,
        Err(_) => LineTermIntent::Unsupported,
    };
    LineTerms {
        intent,
        cycle: pricing_cycle.map(self::cycle),
    }
}

/// What one gate run gates: the operation together with the order it belongs to. A Preview has
/// no order; a submission or amendment always has one, so the two can never disagree.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateTarget {
    /// A preview; it has no order.
    Preview,
    /// The submission of `order_id`.
    Submit {
        /// The order submitted.
        order_id: Uuid,
    },
    /// The amendment of `order_id`.
    Amendment {
        /// The order amended.
        order_id: Uuid,
    },
}

impl GateTarget {
    /// The operation gated.
    #[must_use]
    pub const fn operation(self) -> Operation {
        match self {
            Self::Preview => Operation::Preview,
            Self::Submit { .. } => Operation::Submit,
            Self::Amendment { .. } => Operation::Amendment,
        }
    }

    /// The order the run belongs to; `None` for a Preview.
    #[must_use]
    pub const fn order_id(self) -> Option<Uuid> {
        match self {
            Self::Preview => None,
            Self::Submit { order_id } | Self::Amendment { order_id } => Some(order_id),
        }
    }
}

/// One gate run's basket, as the caller prepared it.
#[domain_model]
#[derive(Debug, Clone)]
pub struct Basket {
    /// Instant captured before external evaluation; also used for frozen evidence/diagnostics.
    pub assessed_at: OffsetDateTime,
    /// The operation gated and the order it belongs to.
    pub target: GateTarget,
    /// The run's assessment identity (D-189): the assessment query and evaluation carry it.
    pub assessment_id: Uuid,
    /// The order's tenant axes.
    pub axes: TenantAxes,
    /// The order's contract, if any; a line's own `contract_ref` takes precedence for its terms.
    pub contract_id: Option<Uuid>,
    /// The lines, in request order.
    pub lines: Vec<BasketLine>,
    /// Term intent and cycle per line; a line without an entry is [`LineTerms::UNSTATED`].
    pub terms: BTreeMap<Uuid, LineTerms>,
    /// The date the catalog resolves on (the run's proposed UTC date).
    pub resolve_date: Date,
    /// The run's frozen date basis; a line it does not cover fails its date row closed.
    pub dates: DateBasis,
}

impl Basket {
    fn line_terms(&self, line: Uuid) -> LineTerms {
        self.terms
            .get(&line)
            .copied()
            .unwrap_or(LineTerms::UNSTATED)
    }
}

/// What a run asks of the ports whose queries follow from the basket alone. The overlap-key,
/// occupancy, assessment, evaluation and tax queries need earlier answers: see
/// [`overlap_key_lines`], [`occupancy_queries`], [`assessment_query`], [`evaluation_request`]
/// and [`tax_query`].
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The payer profile query.
    pub identity: PayerProfileQuery,
    /// One catalog request per line, in basket order (one `read_lines` call, M2-6).
    pub catalog: Vec<LineCatalogRequest>,
    /// One query per distinct order or line contract, in contract-id order; empty when neither
    /// the order nor a line references one (the port is then not called).
    pub contracts: Vec<ContractQuery>,
    /// One billing-terms query per supported line, in basket order.
    pub terms: Vec<BillingTermsQuery>,
    /// The seller's acceptance policy query.
    pub policy: AcceptancePolicyQuery,
    /// Whether the run asks for indicative tax (Preview only).
    pub tax: bool,
}

/// What a run asks, derived purely from the basket.
#[must_use]
pub fn plan(basket: &Basket) -> Plan {
    let TenantAxes {
        resource,
        payer,
        seller,
    } = basket.axes;
    let contracts: BTreeSet<Uuid> = basket
        .contract_id
        .into_iter()
        .chain(basket.lines.iter().filter_map(|l| l.contract_ref))
        .collect();
    Plan {
        identity: PayerProfileQuery {
            payer_tenant_id: payer,
            seller_tenant_id: seller,
            resource_tenant_id: resource,
        },
        catalog: basket.lines.iter().map(|l| l.catalog.clone()).collect(),
        contracts: contracts
            .into_iter()
            .map(|contract_id| ContractQuery {
                contract_id,
                payer_tenant_id: payer,
                seller_tenant_id: seller,
            })
            .collect(),
        terms: basket
            .lines
            .iter()
            .filter_map(|l| {
                let intent = match basket.line_terms(l.line_id).intent {
                    LineTermIntent::Stated(intent) => intent,
                    LineTermIntent::Unstated => TermIntentInput::SellerPolicy,
                    LineTermIntent::Unsupported => return None,
                };
                Some(BillingTermsQuery {
                    line_id: l.line_id,
                    payer_tenant_id: payer,
                    seller_tenant_id: seller,
                    plan_revision_id: l.catalog.revision_id,
                    intent,
                    contract_id: l.contract_ref.or(basket.contract_id),
                })
            })
            .collect(),
        policy: AcceptancePolicyQuery {
            seller_tenant_id: seller,
        },
        tax: basket.target.operation() == Operation::Preview,
    }
}

/// The identity gate after the payer profile answered (D-215 item 2, D-228).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityGate {
    /// Both payer relationships are confirmed: the payer-dependent ports are asked.
    Confirmed,
    /// The profile answered and a relationship is not confirmed: no payer-dependent port is
    /// asked and the assessment withholds their rows.
    Unconfirmed,
    /// The profile did not answer: no payer fact is known, so no payer-dependent port is asked.
    Unavailable,
}

/// The identity gate of a payer profile answer. "Confirmed" is exactly the assessment's test:
/// both relationship flags true (invalid axes alone fail predicate 1 there, they do not
/// withhold).
#[must_use]
pub fn gate_after_identity(identity: &PortAnswer<PayerProfile>) -> IdentityGate {
    match identity {
        PortAnswer::Unavailable(_) | PortAnswer::Skipped(_) => IdentityGate::Unavailable,
        PortAnswer::Answered(p)
            if p.in_relationship_with_seller && p.in_relationship_with_resource =>
        {
            IdentityGate::Confirmed
        }
        PortAnswer::Answered(_) => IdentityGate::Unconfirmed,
    }
}

/// The payer's market, a payer fact disclosed only once the relationship is confirmed (D-215).
#[must_use]
pub fn payer_market(identity: &PortAnswer<PayerProfile>) -> Option<Market> {
    match (gate_after_identity(identity), identity) {
        (IdentityGate::Confirmed, PortAnswer::Answered(p)) => Some(Market {
            currency: p.currency.clone(),
            region: p.region.clone(),
        }),
        _ => None,
    }
}

/// Each line's accepted bindings, from its composed pin (`map_line`); a line whose pin does not
/// compose has no entry.
#[must_use]
pub fn line_bindings(
    basket: &Basket,
    catalog: &BTreeMap<Uuid, CatalogAnswer>,
) -> BTreeMap<Uuid, Vec<AcceptedBinding>> {
    basket
        .lines
        .iter()
        .filter(|l| l.catalog.line_id == l.line_id)
        .filter_map(|l| {
            let answer = catalog.get(&l.line_id)?;
            match map_line(&l.catalog, answer, basket.resolve_date).pin {
                PinOutcome::Composed { bindings, .. } => Some((l.line_id, bindings)),
                PinOutcome::Unresolvable(_) | PinOutcome::Unevaluable(_) => None,
            }
        })
        .collect()
}

/// Every composed selected recurring binding, retaining the original requested dimension.
/// Keyless and unresolved lines make no request; incomplete recurring evidence fails closed.
#[must_use]
pub fn overlap_key_lines(
    basket: &Basket,
    bindings: &BTreeMap<Uuid, Vec<AcceptedBinding>>,
) -> Vec<OverlapKeyLine> {
    overlap_key_requests(&basket.lines, bindings)
}

/// Every selected recurring binding, paired with its original requested selection.
#[must_use]
pub fn overlap_key_requests(
    lines: &[BasketLine],
    bindings: &BTreeMap<Uuid, Vec<AcceptedBinding>>,
) -> Vec<OverlapKeyLine> {
    lines
        .iter()
        .flat_map(|line| {
            line.catalog
                .items
                .iter()
                .zip(bindings.get(&line.line_id).into_iter().flatten())
                .filter(|(_, binding)| binding.kind == ChargeKind::Recurring)
                .map(|(selection, binding)| OverlapKeyLine {
                    line_id: line.line_id,
                    plan_revision_id: line.catalog.revision_id,
                    paid_recurring_sku: binding.sku_id,
                    item_id: selection.item_id,
                    dimension_value: selection.selected_dim_value.clone(),
                    sku_version: binding.sku_version,
                })
        })
        .collect()
}

/// Historical scalar-only projection, retained for legacy callers. The collection pipeline
/// uses `item_keys::validated_line_keys` instead. Each line has one distinct key, as the
/// assessment reads it (`ks.len() == 1`); a line answered with several keys is ambiguous and
/// gets no entry.
#[must_use]
pub fn line_keys(answered: &[OverlapKey]) -> BTreeMap<Uuid, OverlapKey> {
    let mut per_line: BTreeMap<Uuid, Vec<&OverlapKey>> = BTreeMap::new();
    for k in answered {
        let keys = per_line.entry(k.line_id).or_default();
        if !keys
            .iter()
            .any(|seen| seen.overlap_scope_key == k.overlap_scope_key)
        {
            keys.push(k);
        }
    }
    per_line
        .into_iter()
        .filter_map(|(line, keys)| match keys.as_slice() {
            [only] => Some((line, (*only).clone())),
            _ => None,
        })
        .collect()
}

/// One occupancy query per distinct overlap scope key, in key order, for the order's payer and
/// resource tenant (D-198: the scope is resource-bound, as the in-flight claims are).
#[must_use]
pub fn occupancy_queries(keys: &[OverlapKey], axes: &TenantAxes) -> Vec<OccupancyQuery> {
    let distinct: BTreeSet<&str> = keys.iter().map(|k| k.overlap_scope_key.as_str()).collect();
    distinct
        .into_iter()
        .map(|k| OccupancyQuery {
            own_context: None,
            payer_tenant_id: axes.payer,
            resource_tenant_id: Some(axes.resource),
            overlap_scope_key: k.to_owned(),
        })
        .collect()
}

/// The resolved terms of `line`, if answered.
fn terms_of(terms: Option<&[ResolvedTerms]>, line: Uuid) -> Option<&ResolvedTerms> {
    terms?.iter().find(|t| t.line_id == line)
}

/// The D-189 assessment query: every line with its selections, and the market (a confirmed
/// payer's only), term and cycle where known.
#[must_use]
pub fn assessment_query(
    basket: &Basket,
    market: Option<&Market>,
    terms: Option<&[ResolvedTerms]>,
) -> AssessmentQuery {
    AssessmentQuery {
        assessment_id: basket.assessment_id,
        seller_tenant_id: basket.axes.seller,
        resolve_date: basket.resolve_date,
        profile: ASSESSMENT_PROFILE.to_owned(),
        lines: basket
            .lines
            .iter()
            .map(|l| AssessmentLine {
                line_id: l.line_id,
                plan_id: l.catalog.plan_id,
                revision_id: l.catalog.revision_id,
                selections: l.catalog.items.clone(),
                market: market.cloned(),
                term: terms_of(terms, l.line_id).map(|t| t.term),
                cycle: basket.line_terms(l.line_id).cycle,
            })
            .collect(),
    }
}

const fn cycle(c: BillingCycle) -> EvaluationCycle {
    match c {
        BillingCycle::Month => EvaluationCycle::Month,
        BillingCycle::Year => EvaluationCycle::Year,
    }
}

const fn charge_kind(k: &ChargeKind) -> EvaluationChargeKind {
    match k {
        ChargeKind::Recurring => EvaluationChargeKind::Recurring,
        ChargeKind::Usage => EvaluationChargeKind::Usage,
        ChargeKind::OneTime => EvaluationChargeKind::OneTime,
    }
}

/// Preserve the complete native binding alongside legacy projected identity.
///
/// # Errors
/// The schema-2 codec's refusal of a binding it cannot write (an out-of-range scalar).
pub(super) fn evaluation_binding(b: &AcceptedBinding) -> Result<EvaluationBinding, CodecError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let money_digest = b
        .price
        .money_digest
        .iter()
        .flat_map(|byte| [HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 0xf)]])
        .map(char::from)
        .collect();
    Ok(EvaluationBinding {
        native_binding: Some(bss_orders_sdk::commercial::encode_accepted_binding(b)?),
        item_id: b.item_id,
        price_book_entry_id: b.price_book_entry_id,
        price_id: b.price.price_id,
        money_digest,
        currency: b.price.currency.code().to_owned(),
        currency_scale: b.price.currency.scale(),
        sku_id: b.sku_id,
        sku_version: b.sku_version,
        dimension_key: b.dimension_key.clone(),
        dimension_value: b.dimension_value.clone(),
        kind: charge_kind(&b.kind),
        recurring_period: b.recurring_period.map(cycle),
        via_default: b.via_default,
    })
}

/// The evaluation cycle: authored cycle, else the first recurring binding's period.
#[must_use]
pub fn line_cycle(
    basket: &Basket,
    line: Uuid,
    bindings: &[AcceptedBinding],
) -> Option<EvaluationCycle> {
    basket.line_terms(line).cycle.or_else(|| {
        bindings
            .iter()
            .find(|b| b.kind == ChargeKind::Recurring)
            .and_then(|b| b.recurring_period)
            .map(cycle)
    })
}

/// The Rating evaluation request (D-197: exactly the accepted bindings, never re-resolved), or
/// `Ok(None)` when it cannot be built: no confirmed payer market, or any line lacking a composed
/// binding set, resolved terms, resolved service start, or a cycle (the line's chosen cycle,
/// else its first recurring binding's period). Inferring the cycle from the first recurring
/// binding's period is an Orders choice, not a producer rule; a line with neither is not
/// defaulted. An `Ok(None)` is never asked; assembly preserves the missing prerequisite's source
/// reason as skipped evidence, subject to payer withholding and definitive local-refusal
/// precedence.
///
/// # Errors
/// `Unsupported` on the line whose binding the schema-2 codec refuses to encode: a local defect,
/// not a missing prerequisite. The caller logs it and takes the `Ok(None)` path, so the
/// outcome and wire are unchanged (D-220 item 1, as `line_requests_value`).
pub fn evaluation_request(
    basket: &Basket,
    bindings: &BTreeMap<Uuid, Vec<AcceptedBinding>>,
    terms: Option<&[ResolvedTerms]>,
    market: Option<&Market>,
    policy: Option<&AcceptancePolicy>,
) -> Result<Option<OrderEvaluationRequest>, CommercialError> {
    let Some(market) = market else {
        return Ok(None);
    };
    let market = market.clone();
    let mut lines = Vec::with_capacity(basket.lines.len());
    for l in &basket.lines {
        let Some(bound) = bindings.get(&l.line_id) else {
            return Ok(None);
        };
        let Some(resolved_terms) = terms_of(terms, l.line_id) else {
            return Ok(None);
        };
        let term = resolved_terms.term;
        let Some(cycle) = line_cycle(basket, l.line_id, bound) else {
            return Ok(None);
        };
        // A composed pin binds every selected slot once, in request order (`map_line`), so
        // the bindings pair with the items one to one.
        if bound.len() != l.catalog.items.len() {
            return Ok(None);
        }
        let mut items = Vec::with_capacity(bound.len());
        for (i, b) in l.catalog.items.iter().zip(bound) {
            let binding = evaluation_binding(b).map_err(|_| CommercialError::Unsupported {
                line_id: l.line_id,
                what: "native binding encoding",
            })?;
            items.push(EvaluationItem {
                item_id: i.item_id,
                quantity: i.quantity.clone(),
                chains: vec![EvaluationChain {
                    dim_value: i.selected_dim_value.clone(),
                    binding,
                }],
            });
        }
        let Some(line_dates) = basket.dates.lines.get(&l.line_id) else {
            return Ok(None);
        };
        let start_at = match line_dates {
            crate::domain::dates::LineResolution::Resolved { dates } => dates
                .service_activation
                .date()
                .with_time(Time::MIDNIGHT)
                .assume_utc(),
            crate::domain::dates::LineResolution::Invalid { .. } => return Ok(None),
        };
        lines.push(EvaluationLine {
            plan_id: Some(l.catalog.plan_id),
            start_at: Some(start_at),
            resolved_terms: Some(resolved_terms.clone()),
            line_id: l.line_id,
            plan_revision_id: l.catalog.revision_id,
            term,
            cycle,
            items,
        });
    }
    Ok(Some(OrderEvaluationRequest {
        tenant_axes: Some(basket.axes),
        assessed_at: Some(basket.assessed_at),
        acceptance_policy: policy.cloned(),
        assessment_id: basket.assessment_id,
        resolve_date: basket.resolve_date,
        market,
        contract_id: basket.contract_id,
        lines,
    }))
}

/// The indicative tax query of a Preview: each evaluated line's net amount, copied verbatim (no
/// money arithmetic, D-216); `None` for any other operation.
#[must_use]
pub fn tax_query(basket: &Basket, evaluation: &OrderEvaluation) -> Option<TaxQuery> {
    (basket.target.operation() == Operation::Preview).then(|| TaxQuery {
        payer_tenant_id: basket.axes.payer,
        seller_tenant_id: basket.axes.seller,
        lines: evaluation
            .lines
            .iter()
            .map(|l| super::answers::LineAmount {
                line_id: l.line_id,
                amount: l.totals.net.clone(),
            })
            .collect(),
    })
}

/// Occupancy per asked overlap scope (#5131 `getOverlapOccupancy`).
pub type OccupancyAnswer = PortAnswer<Vec<(OccupancyQuery, OccupancyObservation)>>;

/// Every answer one run collected. `None` is a port the run did not ask (module doc).
#[domain_model]
#[derive(Debug, Clone)]
pub struct Answers {
    /// The catalog answers per line (one `read_lines` call).
    pub catalog: BTreeMap<Uuid, CatalogAnswer>,
    /// The payer profile.
    pub identity: PortAnswer<PayerProfile>,
    /// Contract facts; `None` when not asked.
    pub contracts: Option<PortAnswer<BTreeMap<Uuid, ContractFacts>>>,
    /// Overlap keys; `None` when not asked.
    pub overlap_keys: Option<PortAnswer<Vec<OverlapKey>>>,
    /// Occupancy per scope; `None` when not asked.
    pub occupancy: Option<OccupancyAnswer>,
    /// Keys held by other orders for the same payer and resource tenant.
    pub inflight_same_tuple: std::collections::BTreeSet<String>,
    /// Per overlap scope key, the lines carrying it on other in-flight orders of the payer under
    /// another resource tenant (the D-180 interim addend: a line count, not a claim count).
    pub inflight_other_orders: BTreeMap<String, u32>,
    /// Billing terms per line; `None` when not asked (relationship unconfirmed, D-215 item 2).
    pub billing_terms: Option<PortAnswer<Vec<ResolvedTerms>>>,
    /// The seller's acceptance policy.
    pub acceptance_policy: PortAnswer<Option<AcceptancePolicy>>,
    /// The D-189 assessment.
    pub assessment: PortAnswer<AssessmentAnswer>,
    /// The Rating evaluation; `None` when not asked.
    pub evaluation: Option<PortAnswer<OrderEvaluation>>,
}

/// A port the run did not ask (module doc).
const fn not_asked<T>() -> PortAnswer<T> {
    PortAnswer::Unavailable(Unavailable::Missing)
}

/// The assessment input of one run: the basket and every collected answer, a port not asked as
/// `Unavailable(Missing)` (module doc).
#[must_use]
pub fn assemble(basket: &Basket, answers: Answers) -> AssessmentInput {
    let bindings = line_bindings(basket, &answers.catalog);
    let unsupported_terms = basket
        .lines
        .iter()
        .filter_map(|l| {
            let unsupported = basket.line_terms(l.line_id).intent == LineTermIntent::Unsupported;
            let mismatch = bindings.get(&l.line_id).is_some_and(|bound| {
                let expected = line_cycle(basket, l.line_id, bound);
                let snapshot_mismatch = answers
                    .billing_terms
                    .as_ref()
                    .and_then(PortAnswer::answered)
                    .and_then(|terms| terms.iter().find(|t| t.line_id == l.line_id))
                    .and_then(|terms| terms.snapshot.as_ref())
                    .is_some_and(|snapshot| Some(snapshot.cycle) != expected);
                snapshot_mismatch
                    || bound.iter().any(|b| {
                        b.kind == ChargeKind::Recurring && b.recurring_period.map(cycle) != expected
                    })
            });
            // D-193/D-236 (R5): answered terms the submit cannot freeze into a Pricing request
            // (no snapshot, or a snapshot Pricing cannot express) are unsupported terms, as
            // Pricing's own MISSING_BILLING_TERMS / UNSUPPORTED_TERMS refusals settle.
            let unusable_snapshot = answers
                .billing_terms
                .as_ref()
                .and_then(PortAnswer::answered)
                .and_then(|terms| terms.iter().find(|t| t.line_id == l.line_id))
                .is_some_and(|terms| {
                    terms
                        .snapshot
                        .as_ref()
                        .is_none_or(|s| super::commercial::billing_terms(s).is_err())
                });
            (unsupported || mismatch || unusable_snapshot).then_some(l.line_id)
        })
        .collect();
    let evaluation = answers.evaluation.unwrap_or_else(|| {
        let reason = if answers.identity.answered().is_none() {
            GatePort::IdentityProfile.unavailable_reason()
        } else if gate_after_identity(&answers.identity) == IdentityGate::Unconfirmed {
            if answers.identity.answered().is_some_and(|p| !p.axes_valid) {
                bss_orders_sdk::catalog::Reason::AxisInvalid
            } else {
                bss_orders_sdk::catalog::Reason::PayerRelationshipUnconfirmed
            }
        } else if basket
            .lines
            .iter()
            .any(|l| !bindings.contains_key(&l.line_id))
        {
            basket
                .lines
                .iter()
                .filter(|l| !bindings.contains_key(&l.line_id))
                .find_map(|l| {
                    answers.catalog.get(&l.line_id).and_then(|c| {
                        match map_line(&l.catalog, c, basket.resolve_date).pin {
                            PinOutcome::Unresolvable(r) | PinOutcome::Unevaluable(r) => Some(r),
                            PinOutcome::Composed { .. } => None,
                        }
                    })
                })
                .unwrap_or(bss_orders_sdk::catalog::Reason::CatalogPredicatesUnavailable)
        } else {
            bss_orders_sdk::catalog::Reason::CatalogPinCompositionUnavailable
        };
        PortAnswer::Skipped(reason)
    });
    AssessmentInput {
        occupancy_axes: Some(basket.axes),
        unsupported_terms,
        operation: basket.target.operation(),
        resolve_date: basket.resolve_date,
        lines: basket.lines.clone(),
        catalog: answers.catalog,
        identity: answers.identity,
        contract_id: basket.contract_id,
        contracts: answers.contracts.unwrap_or_else(not_asked),
        overlap_keys: answers.overlap_keys.unwrap_or_else(not_asked),
        occupancy: answers.occupancy.unwrap_or_else(not_asked),
        inflight_same_tuple: answers.inflight_same_tuple,
        inflight_other_orders: answers.inflight_other_orders,
        billing_terms: answers.billing_terms.unwrap_or_else(not_asked),
        acceptance_policy: answers.acceptance_policy,
        assessment: answers.assessment,
        evaluation,
        dates: basket.dates.clone(),
    }
}

/// Verify the evaluation envelope before any consumer uses it.
/// # Errors
/// Returns malformed when the answer does not describe the request.
pub fn verify_evaluation(
    request: &OrderEvaluationRequest,
    answer: &OrderEvaluation,
) -> Result<(), Unavailable> {
    let wanted: std::collections::BTreeSet<_> = request.lines.iter().map(|l| l.line_id).collect();
    let stated: std::collections::BTreeSet<_> = answer.lines.iter().map(|l| l.line_id).collect();
    let money_ok = |m: &super::answers::ExactMoney| {
        m.currency() == request.market.currency && m.currency_scale() == answer.currency_scale
    };
    let totals_ok = |t: &super::answers::Totals| {
        money_ok(&t.gross.exact)
            && money_ok(&t.net.exact)
            && match &t.discount {
                super::answers::Availability::Available(a) => money_ok(&a.exact),
                super::answers::Availability::Unavailable => true,
            }
    };
    // D-197, D-216: the answer is at the scale of every price Rating was asked to evaluate.
    let scale_ok = request
        .lines
        .iter()
        .flat_map(|l| &l.items)
        .flat_map(|i| &i.chains)
        .all(|c| c.binding.currency_scale == answer.currency_scale);
    if answer.assessment_id != request.assessment_id
        || wanted.len() != request.lines.len()
        || stated.len() != answer.lines.len()
        || wanted != stated
        || answer.currency != request.market.currency
        || answer.currency_scale > 28
        || !scale_ok
        || !totals_ok(&answer.order_totals)
        || answer.lines.iter().any(|l| !totals_ok(&l.totals))
        || answer.tcv.as_ref().is_some_and(|t| !money_ok(&t.exact))
    {
        return Err(Unavailable::Malformed);
    }
    Ok(())
}

/// Verify the acceptance-policy answer before any consumer uses it (D-191), as
/// [`verify_evaluation`] does for Rating: the version the Pricing hold policy is frozen from is
/// canonical positive-integer text (`7`, never `07`, `+7` or `v7`).
///
/// # Errors
/// `Malformed` for any other version text.
pub fn verify_policy(policy: &AcceptancePolicy) -> Result<(), Unavailable> {
    super::commercial::positive(&policy.version)
        .map(|_| ())
        .ok_or(Unavailable::Malformed)
}

/// Verify the billing-terms answer before any consumer uses it (D-193, D-197), as
/// [`verify_evaluation`] does for Rating: every entry names a line the run asked, and no line is
/// answered twice. The readers disagree on a duplicated line's winner (the assessment query and
/// the Rating request take the first entry, the frozen Pricing query the last), so the whole
/// answer is unusable. A line the answer omits stays per-line missing terms (`pin_row`).
///
/// # Errors
/// `Malformed` for an entry naming an unasked line or a second entry for one line.
pub fn verify_terms(
    queries: &[BillingTermsQuery],
    answer: &[ResolvedTerms],
) -> Result<(), Unavailable> {
    let asked: BTreeSet<Uuid> = queries.iter().map(|q| q.line_id).collect();
    let mut seen = BTreeSet::new();
    if answer
        .iter()
        .all(|t| asked.contains(&t.line_id) && seen.insert(t.line_id))
    {
        Ok(())
    } else {
        Err(Unavailable::Malformed)
    }
}
