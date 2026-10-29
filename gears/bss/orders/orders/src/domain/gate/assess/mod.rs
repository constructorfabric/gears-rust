//! The pure gate assessment (S3-09 skeleton; F03 §3.1–§3.4, §4.2; D-187, D-189, D-195, D-227):
//! typed port answers in, the complete ordered outcome vector, the deduplicated failure list,
//! the engine input resolutions and the per-line pins out. No call, no storage, no clock.
//!
//! Orchestration only: the catalog rows and the pin come from [`map_line`], the assessment rows
//! from [`compose`](super::compose) (submodule `assessed`), the overlap rows from submodule
//! `overlap`, ordering and the primary from [`Vector`]. Every row is `required` except the pin,
//! whose applicability follows the operation (D-189, D-227 item 6).
//!
//! Determinism: every lookup is keyed (`BTreeMap`) or order-independent, and the producer
//! subresults of one row are sorted by `(producer, profile, predicate)` before composition, so
//! the order of `lines` and of every answer vector never changes the vector or the failure list.
mod assessed;
mod overlap;

use super::MAPPING_VERSION;
use super::compose::local;
use super::evidence::UpstreamDetail;
use super::orchestrate::TenantAxes;
use super::ports::{GatePort, PortAnswer, Unavailable};
use super::predicates::PredicateId as P;
use super::pricing_map::{
    CatalogAnswer, LineCatalog, LineCatalogRequest, PinOutcome, ReadFailure, map_line, normalise,
};
use super::vector::{
    Applicability, Failure, Operation, Outcome, OutcomeKey, Vector, VectorError, Verdict,
    definitive_local,
};
use crate::domain::dates::DateBasis;
use crate::domain::gate::answers::{
    AcceptancePolicy, AssessmentAnswer, ContractFacts, ContractStatus, OccupancyObservation,
    OccupancyQuery, OrderEvaluation, OverlapKey, PayerProfile, ResolvedTerms,
};
use crate::domain::guards::InputPort;
use crate::domain::transition::{InputState, Inputs};
use assessed::{assessed_results, assessed_row};
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::gate_report::{CatalogSelection, GateFailure, GateOutcome, GateReport};
use overlap::Overlap;
use std::collections::{BTreeMap, BTreeSet};
use time::Date;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// One basket line as the gate sees it.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasketLine {
    /// The line. It must equal `catalog.line_id`: a mismatch is a caller (programming) error
    /// that is never repaired; the line's `ReferenceResolution` fails with
    /// `reference-unresolvable` and its pin is unevaluable with that reason.
    pub line_id: Uuid,
    /// What the line asks of the catalog; its `line_id` must equal the line's.
    pub catalog: LineCatalogRequest,
    /// The book currency the line names, if any.
    pub currency: Option<String>,
    /// The contract the line references, if any.
    pub contract_ref: Option<Uuid>,
}

/// Everything one assessment run reads, already resolved by the ports (`infra::gate`).
#[domain_model]
#[derive(Debug, Clone)]
pub struct AssessmentInput {
    /// Trusted tenant axes, of which the protected profile reads the payer and resource;
    /// required by the proposed protected profile.
    pub occupancy_axes: Option<TenantAxes>,
    /// Lines whose authored terms or accepted/evaluation cycles are unsupported.
    pub unsupported_terms: std::collections::BTreeSet<Uuid>,
    /// The operation gated.
    pub operation: Operation,
    /// The assessment date the catalog answers are evaluated at (the run's proposed UTC date).
    pub resolve_date: Date,
    /// The basket, in request order (the order never changes the result).
    pub lines: Vec<BasketLine>,
    /// The Pricing/Products answers per line; a line without an entry is a port outage.
    pub catalog: BTreeMap<Uuid, CatalogAnswer>,
    /// The payer commercial profile (D-199, D-215).
    pub identity: PortAnswer<PayerProfile>,
    /// The order contract, if any.
    pub contract_id: Option<Uuid>,
    /// Contract facts by contract id; consulted for order and line contract references.
    /// Ignored while the payer relationship is unconfirmed (D-215, D-228): not read then.
    pub contracts: PortAnswer<BTreeMap<Uuid, ContractFacts>>,
    /// The overlap key per line (SUB-G1); a line without an entry is answered-missing.
    /// Ignored while the payer relationship is unconfirmed (D-215, D-228): not read then.
    pub overlap_keys: PortAnswer<Vec<OverlapKey>>,
    /// Occupancy per overlap key (#5131 `getOverlapOccupancy`).
    /// Ignored while the payer relationship is unconfirmed (D-215, D-228): not read then.
    pub occupancy: PortAnswer<Vec<(OccupancyQuery, OccupancyObservation)>>,
    /// Keys held by other orders for the same payer and resource tenant.
    pub inflight_same_tuple: std::collections::BTreeSet<String>,
    /// Other in-flight orders per overlap key (D-180 interim addend, resolved locally).
    pub inflight_other_orders: BTreeMap<String, u32>,
    /// Pre-subscription billing terms per line (D-193).
    pub billing_terms: PortAnswer<Vec<ResolvedTerms>>,
    /// The seller's acceptance policy; `Answered(None)` is answered-missing (D-191).
    pub acceptance_policy: PortAnswer<Option<AcceptancePolicy>>,
    /// The nonbinding Pricing assessment (D-189).
    pub assessment: PortAnswer<AssessmentAnswer>,
    /// The Rating evaluation; feeds the engine input only.
    pub evaluation: PortAnswer<OrderEvaluation>,
    /// The run's frozen date basis; a line it does not cover fails its date row closed.
    pub dates: DateBasis,
}

/// The result of one assessment run.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    /// The complete ordered outcome vector.
    pub vector: Vector,
    /// Every non-passed row once per `(line, reason)`, in vector order.
    pub failures: Vec<Failure>,
    /// The engine input resolutions (D-227 item 1 per-input rule).
    pub inputs: Inputs,
    /// The pin outcome per line.
    pub pins: BTreeMap<Uuid, PinOutcome>,
    /// The primary Problem for the operation, if anything blocks (D-230, D-195).
    pub primary: Option<Outcome>,
}

impl Assessment {
    /// The complete authorized gate report a composite-gate refusal carries as its
    /// `context.data.assessment` member (DESIGN L5093, D-229): the vector's public fields
    /// (identity, verdict, reason, applicability) and the failure list mapped 1:1, every enum as
    /// its stored token, under [`MAPPING_VERSION`]. Never producer results or upstream evidence.
    #[must_use]
    pub fn report(&self, assessment_id: Uuid) -> GateReport {
        self.vector.report(assessment_id)
    }
}

impl Vector {
    /// Project this settled vector and its deduplicated failures into the authorized report.
    #[must_use]
    pub fn report(&self, assessment_id: Uuid) -> GateReport {
        let outcomes = self
            .outcomes()
            .iter()
            .map(|o| {
                let k = &o.key;
                let mut outcome = GateOutcome::new(
                    k.predicate.token(),
                    o.verdict.token(),
                    o.applicability.token(),
                )
                .with_selection(k.selection.clone())
                .with_reason(o.reason);
                if let Some(line) = k.line_id {
                    outcome = outcome.with_line(line);
                }
                if let Some(item) = k.item_id {
                    outcome = outcome.with_item(item);
                }
                outcome
            })
            .collect();
        let failures = self
            .failure_list()
            .into_iter()
            .map(|f| GateFailure::new(f.line_id, f.reason, f.predicate.token()))
            .collect();
        GateReport::new(assessment_id, MAPPING_VERSION, outcomes, failures)
    }
}

/// One evaluated verdict with its reason and evidence.
type Eval = (Verdict, Option<Reason>, Option<UpstreamDetail>);
const PASS: Eval = (Verdict::Passed, None, None);
const fn failed(r: Reason) -> Eval {
    (Verdict::Failed, Some(r), None)
}
const fn unevaluable(r: Reason) -> Eval {
    (Verdict::Unevaluable, Some(r), None)
}
/// An unevaluable verdict whose evidence names an Orders-authored `upstream_code` (D-195).
fn unevaluable_with(r: Reason, upstream_code: &str) -> Eval {
    (
        Verdict::Unevaluable,
        Some(r),
        Some(UpstreamDetail {
            upstream_code: Some(upstream_code.into()),
            ..UpstreamDetail::default()
        }),
    )
}
/// The upstream code of a basket line whose catalog request names another line.
const LINE_ID_MISMATCH: &str = "basket-line-id-mismatch";

/// The upstream code of a bundle line whose items state different quantities (D-192: one exact
/// quantity per line; D-227 item 4: an unsupported shape refuses `reference-unresolvable`).
const MIXED_ITEM_QUANTITIES: &str = "mixed-item-quantities";
/// The predicates the D-189 assessment answers (Task 6a Amendment A).
const ASSESSED: [P; 4] = [
    P::CatalogRevisionAvailable,
    P::CatalogDimensionMarket,
    P::CatalogEligibilityRules,
    P::MarketConsistency,
];

fn key(predicate: P, line_id: Option<Uuid>) -> OutcomeKey {
    OutcomeKey {
        predicate,
        line_id,
        item_id: None,
        selection: CatalogSelection::NotSelected,
    }
}
fn row(predicate: P, line_id: Option<Uuid>, (verdict, reason, detail): Eval) -> Outcome {
    let k = key(predicate, line_id);
    local(k, Applicability::Required, verdict, reason, detail)
}

/// Assess one basket over its port answers.
///
/// # Errors
/// The rows break a vector rule: a duplicated line id gives a duplicate identity.
pub fn assess(input: &AssessmentInput) -> Result<Assessment, VectorError> {
    // D-215/D-228: until the payer relationship is confirmed no payer fact is resolved or
    // disclosed. The payer-dependent rows carry predicate 1's reason (`axis-invalid` when the
    // axes are also invalid), never a port's: the contract, market, overlap and in-flight rows,
    // and each composed pin row, whose billing terms the orchestrator withholds (D-215 item 2).
    let withheld = relationship_unconfirmed(input).then(|| {
        let axes_valid = input.identity.answered().is_some_and(|p| p.axes_valid);
        unevaluable(if axes_valid {
            Reason::PayerRelationshipUnconfirmed
        } else {
            Reason::AxisInvalid
        })
    });
    let fallback = CatalogAnswer {
        current: Err(ReadFailure::Unavailable(Unavailable::Missing)),
        resolved: Err(ReadFailure::Unavailable(Unavailable::Missing)),
        skus: BTreeMap::new(),
    };
    // Each line is mapped once; a line without a catalog entry maps as a port outage. A
    // duplicated line id keeps one mapping, and its rows still break the vector below.
    let mapped: BTreeMap<Uuid, LineCatalog> = input
        .lines
        .iter()
        .map(|line| {
            let answer = input.catalog.get(&line.line_id).unwrap_or(&fallback);
            (
                line.line_id,
                map_basket_line(line, answer, input.resolve_date),
            )
        })
        .collect();
    let overlap = Overlap::new(input, &mapped);
    let overlap_required = input.lines.iter().any(|l| !overlap.is_keyless(l.line_id));
    let mut down = unavailable_ports(input, withheld.is_some(), overlap_required);
    // The first unavailable port a predicate reads, as that port's reason (rule b).
    let blocked = |p: P| {
        p.spec()
            .ports
            .iter()
            .find(|g| down.contains(g))
            .map(|g| unevaluable(g.unavailable_reason()))
    };
    let mut out = vec![
        row(
            P::AxisValidity,
            None,
            blocked(P::AxisValidity).unwrap_or_else(|| axis(input)),
        ),
        row(
            P::ContractActive,
            None,
            withheld.clone().unwrap_or_else(|| contract_active(input)),
        ),
        row(P::SingleCurrency, None, single_currency(&input.lines)),
    ];
    let date_failures: BTreeSet<Uuid> =
        input.dates.failures().into_iter().map(|(l, _)| l).collect();
    // Predicates 2, 7, 8: `map_line` emits the expected coverage, plus one 4-Orders row per line;
    // the assessment decides them.
    let expected: BTreeSet<OutcomeKey> = mapped
        .iter()
        .flat_map(|(&line_id, m)| {
            let market = key(P::MarketConsistency, Some(line_id));
            m.outcomes
                .iter()
                .filter(|o| ASSESSED.contains(&o.key.predicate))
                .map(|o| o.key.clone())
                .chain([market])
        })
        .collect();
    let assessed = assessed_results(input, &expected);
    let mut pins = BTreeMap::new();
    for line in &input.lines {
        let id = line.line_id;
        let some = Some(id);
        // Every input line is a key of `mapped`, built above from `input.lines`.
        let Some(mapped) = mapped.get(&id) else {
            continue;
        };
        for o in mapped.outcomes.iter().cloned() {
            if ASSESSED.contains(&o.key.predicate) {
                out.push(assessed_row(o.key, assessed.as_ref()));
            } else {
                out.push(o);
            }
        }
        out.push(row(P::QuantityFloor, some, quantity_floor(line)));
        // Predicate 4-Orders: the identity port outranks the assessment (spec port order).
        out.push(if down.contains(&GatePort::IdentityProfile) {
            row(
                P::MarketConsistency,
                some,
                unevaluable(Reason::IdentityPartyUnavailable),
            )
        } else if let Some(w) = &withheld {
            row(P::MarketConsistency, some, w.clone())
        } else {
            assessed_row(key(P::MarketConsistency, some), assessed.as_ref())
        });
        // A missing key decides the in-flight row even when occupancy did not answer.
        let (unique, in_flight) = if overlap.is_keyless(id) {
            overlap.rows(id, input)
        } else if let Some(w) = &withheld {
            (w.clone(), w.clone())
        } else {
            let (mut unique, in_flight) = match blocked(P::InFlightPerKey) {
                Some(e) => (e.clone(), e),
                None => overlap.rows(id, input),
            };
            if let Some(e) = blocked(P::OverlapUniqueness) {
                unique = e;
            }
            (unique, in_flight)
        };
        out.push(row(P::OverlapUniqueness, some, unique));
        out.push(row(P::InFlightPerKey, some, in_flight));
        // A line the basis does not cover is a caller error: it fails closed.
        let dates = if date_failures.contains(&id) {
            failed(Reason::DateCascadeInvalid)
        } else if input.dates.lines.contains_key(&id) {
            PASS
        } else {
            unevaluable(Reason::DateCascadeInvalid)
        };
        out.push(row(P::DatesResolvable, some, dates));
        out.push(pin_row(input, id, &mapped.pin, withheld.as_ref()));
        pins.insert(id, mapped.pin.clone());
    }
    let vector = Vector::build(out)?;
    let failures = vector.failure_list();
    let primary = vector.primary(input.operation).cloned();
    let definitive = primary.as_ref().is_some_and(definitive_local);
    // An answered but invalid assessment cannot resolve the engine's catalog input.
    // Use the same input path as an unavailable port, including definitive-refusal
    // and payer-withholding precedence, while keeping the complete diagnostic vector.
    if vector.outcomes().iter().any(|outcome| {
        ASSESSED.contains(&outcome.key.predicate)
            && outcome.verdict == Verdict::Unevaluable
            && outcome.reason == Some(Reason::CatalogPredicatesUnavailable)
    }) {
        down.insert(GatePort::Assessment);
    }
    // Answered occupancy may still be incomplete or fail protected evidence validation.
    // Keep its diagnostic rows, but normalize the owning engine input through the same
    // unavailable path and precedence overrides as a missing occupancy answer.
    if vector.outcomes().iter().any(|outcome| {
        outcome.key.predicate == P::OverlapUniqueness
            && outcome.verdict == Verdict::Unevaluable
            && outcome.reason == Some(Reason::OverlapPresenceUnevaluable)
    }) {
        down.insert(GatePort::Occupancy);
    }
    let mut resolved_inputs = inputs(&down, withheld.is_some(), definitive);
    if !definitive
        && withheld.is_none()
        && let PortAnswer::Skipped(reason) = &input.evaluation
    {
        resolved_inputs =
            resolved_inputs.with(InputPort::Evaluation, InputState::Unresolvable(*reason));
    }
    Ok(Assessment {
        vector,
        failures,
        inputs: resolved_inputs,
        pins,
        primary,
    })
}

/// One line's catalog mapping. A line whose request names another line, or whose items state
/// different quantities (an unsupported purchase shape, D-192/D-227 item 4), is never repaired:
/// its rows stay keyed by `line.line_id` so the vector stays complete, but `ReferenceResolution`
/// fails with `reference-unresolvable` and the pin carries that reason.
fn map_basket_line(line: &BasketLine, answer: &CatalogAnswer, resolve_date: Date) -> LineCatalog {
    let id = line.line_id;
    let code = if line.catalog.line_id != id {
        LINE_ID_MISMATCH
    } else if mixed_quantities(&line.catalog) {
        MIXED_ITEM_QUANTITIES
    } else {
        return map_line(&line.catalog, answer, resolve_date);
    };
    let keyed = LineCatalogRequest {
        line_id: id,
        ..line.catalog.clone()
    };
    let mut mapped = map_line(&keyed, answer, resolve_date);
    let r = Reason::ReferenceUnresolvable;
    for o in &mut mapped.outcomes {
        if o.key.predicate == P::ReferenceResolution {
            o.verdict = Verdict::Failed;
            o.reason = Some(r);
            o.upstream_detail = Some(UpstreamDetail {
                upstream_code: Some(code.to_owned()),
                ..UpstreamDetail::default()
            });
        }
    }
    mapped.pin = PinOutcome::Unevaluable(r);
    mapped
}

/// Whether the line's items state more than one quantity value. Text that does not parse is the
/// quantity floor's to fail, not this rule's.
fn mixed_quantities(request: &LineCatalogRequest) -> bool {
    request
        .items
        .iter()
        .filter_map(|i| bss_money::parse_decimal(&i.quantity).ok())
        .collect::<BTreeSet<_>>()
        .len()
        > 1
}

/// The ports that did not answer. A Pricing port is down when it did not answer for any basket
/// line after [`normalise`] (the mapping's own view: a malformed answer, e.g. a `resolve` of
/// another revision, is unavailable here too); a line without a catalog entry counts for both
/// Pricing ports. `IndicativeTax` is not an input of this run. With the payer relationship
/// unconfirmed (`withheld`) billing terms are withheld, not down: the orchestrator does not ask
/// them (D-215 item 2) and the pin rows carry predicate 1's reason instead (D-228 item 6).
fn unavailable_ports(
    input: &AssessmentInput,
    withheld: bool,
    overlap_required: bool,
) -> BTreeSet<GatePort> {
    let mut down = BTreeSet::new();
    for line in &input.lines {
        let Some(c) = input.catalog.get(&line.line_id) else {
            down.extend([GatePort::CurrentRevision, GatePort::Resolve]);
            continue;
        };
        let n = normalise(&line.catalog, c);
        if n.current_unavailable() {
            down.insert(GatePort::CurrentRevision);
        }
        if n.resolve_unavailable() {
            down.insert(GatePort::Resolve);
        }
        if c.skus.values().any(|s| s.answered().is_none()) {
            down.insert(GatePort::SkuRead);
        }
    }
    let other = [
        (GatePort::Assessment, input.assessment.answered().is_none()),
        (
            GatePort::BillingTerms,
            !withheld && input.billing_terms.answered().is_none(),
        ),
        (
            GatePort::AcceptancePolicy,
            input.acceptance_policy.answered().is_none(),
        ),
        (
            GatePort::IdentityProfile,
            input.identity.answered().is_none(),
        ),
        (
            GatePort::ContractResolution,
            input.contracts.answered().is_none(),
        ),
        (
            GatePort::OverlapKey,
            overlap_required && input.overlap_keys.answered().is_none(),
        ),
        (
            GatePort::Occupancy,
            overlap_required && input.occupancy.answered().is_none(),
        ),
        (GatePort::Evaluation, input.evaluation.answered().is_none()),
    ];
    down.extend(other.into_iter().filter(|(_, d)| *d).map(|(p, _)| p));
    down
}

/// The payer profile answered and the payer↔seller or payer↔resource relationship is not
/// confirmed (D-215 item 2, D-228).
fn relationship_unconfirmed(input: &AssessmentInput) -> bool {
    input
        .identity
        .answered()
        .is_some_and(|p| !(p.in_relationship_with_seller && p.in_relationship_with_resource))
}

/// The engine inputs the composite gate resolves by withholding while the payer relationship is
/// unconfirmed (D-215, D-228 item 6). Predicate 1 lives inside the composite gate, which is the
/// last guard registered on its rows, so no earlier-registered guard makes these inputs moot and
/// D-113 `Precluded` does not apply (the engine would refuse to reach the gate,
/// `PrecludedReached`): they are `Resolved`, and the gate's own predicate carries predicate 1's
/// failure. `PinComposition` and `Evaluation` need the billing terms, whose query carries the
/// payer and the contract (D-215 item 2), so the orchestrator does not ask them. The override
/// wins over a down `Resolve`, `AcceptancePolicy` or evaluation port, which would otherwise mark
/// the input `Unresolvable` and settle a 503 before the gate's 400.
const PAYER_WITHHELD_INPUTS: [InputPort; 5] = [
    InputPort::ContractResolution,
    InputPort::OverlapKey,
    InputPort::Occupancy,
    InputPort::PinComposition,
    InputPort::Evaluation,
];

/// Engine inputs from `GatePort::input_reasons` (D-227 item 1): a down port marks each input it
/// feeds unresolvable with that input's reason; an input every feeding port answered resolves.
///
/// Two overrides, both `Resolved` and both winning over `Unresolvable`:
/// - `withheld` (D-228 item 6): the payer relationship is unconfirmed; the
///   [`PAYER_WITHHELD_INPUTS`] are resolved by the gate's withholding.
/// - `definitive` (D-230 engine presentation): the primary is a definitive local refusal, so
///   every composite-gate input (the eight this function produces) is `Resolved`. The engine
///   checks a guard's unresolvable inputs before its predicate (D-113), so an unresolvable
///   catalog or pricing input would otherwise settle the gate as `Unevaluable` (503) and mask
///   the refusal D-230 ranks first; presented resolved, the bound gate predicate returns
///   `Fail(primary.reason)` and the engine settles the 400. D-113 order still applies between
///   guards; inside the gate D-230 applies. A non-definitive primary keeps the per-port marking.
fn inputs(down: &BTreeSet<GatePort>, withheld: bool, definitive: bool) -> Inputs {
    let mut states = BTreeMap::new();
    for port in GatePort::ALL {
        if !down.contains(&port) {
            for p in port.feeds() {
                states.entry(*p).or_insert(InputState::Resolved);
            }
        }
    }
    // A down port marks its inputs unresolvable whatever another feeding port answered.
    for port in GatePort::ALL.into_iter().filter(|p| down.contains(p)) {
        for (p, r) in port.input_reasons() {
            states.insert(p, InputState::Unresolvable(r));
        }
    }
    for (p, state) in &mut states {
        if definitive || (withheld && PAYER_WITHHELD_INPUTS.contains(p)) {
            *state = InputState::Resolved;
        }
    }
    states
        .into_iter()
        .fold(Inputs::new(), |acc, (p, s)| acc.with(p, s))
}

/// Rule (e): the payer profile's axes and relationship (D-215: the caller withholds payer facts
/// until the relationship is confirmed).
fn axis(input: &AssessmentInput) -> Eval {
    let Some(p) = input.identity.answered() else {
        return unevaluable(GatePort::IdentityProfile.unavailable_reason());
    };
    if !p.axes_valid {
        return failed(Reason::AxisInvalid);
    }
    if !(p.in_relationship_with_seller && p.in_relationship_with_resource) {
        // D-228: a gate predicate result (FailedPrecondition, 400), D-215 item 2.
        return failed(Reason::PayerRelationshipUnconfirmed);
    }
    PASS
}

/// Rule (f): passed without an order or line contract reference; otherwise every contract is active
/// and its party eligible, in contract-id order. An answer without the contract is not active.
fn contract_active(input: &AssessmentInput) -> Eval {
    let refs: BTreeSet<Uuid> = input
        .contract_id
        .into_iter()
        .chain(input.lines.iter().filter_map(|l| l.contract_ref))
        .collect();
    if refs.is_empty() {
        return PASS;
    }
    let Some(contracts) = input.contracts.answered() else {
        return unevaluable(GatePort::ContractResolution.unavailable_reason());
    };
    for c in refs {
        match contracts.get(&c) {
            Some(f) if f.status == ContractStatus::Active && f.payer_eligible => {}
            Some(f) if f.status == ContractStatus::Active => {
                return failed(Reason::ContractPartyIneligible);
            }
            _ => return failed(Reason::ContractNotActive),
        }
    }
    PASS
}

/// Rule (g): at most one named currency across the basket.
fn single_currency(lines: &[BasketLine]) -> Eval {
    let named: BTreeSet<&str> = lines.iter().filter_map(|l| l.currency.as_deref()).collect();
    if named.len() > 1 {
        failed(Reason::CurrencyMixed)
    } else {
        PASS
    }
}

/// Rule (h): every quantity is positive `bss_money` canonical decimal text (D-170, D-216).
fn quantity_floor(line: &BasketLine) -> Eval {
    let positive_canonical = |text: &str| {
        bss_money::parse_decimal(text).is_ok_and(|d| {
            !d.is_zero() && !d.is_sign_negative() && bss_money::canonical_decimal(d) == text
        })
    };
    if line
        .catalog
        .items
        .iter()
        .all(|i| positive_canonical(&i.quantity))
    {
        PASS
    } else {
        failed(Reason::QuantityBelowFloor)
    }
}

/// Rule (j): the pin row from the line's `PinOutcome`, the billing terms and the policy.
///
/// `payer_withheld` is predicate 1's reason while the payer relationship is unconfirmed: the
/// terms are then withheld, not missing (D-215 item 2), so a composed pin is unevaluable with
/// that reason and `required` (D-228 item 6), whatever the terms or the policy answered. A pin
/// that is itself unresolvable or unevaluable keeps its catalog reason.
fn pin_row(
    input: &AssessmentInput,
    line: Uuid,
    pin: &PinOutcome,
    payer_withheld: Option<&Eval>,
) -> Outcome {
    let terms_missing = input
        .billing_terms
        .answered()
        .is_none_or(|t| !t.iter().any(|t| t.line_id == line));
    let preview = input.operation == Operation::Preview;
    let base = Applicability::Required;
    // D-189, D-195: only the optional-context withholding path (a composed pin whose terms are
    // missing) is tcv-only on a Preview; a pin that is itself unresolvable or unevaluable stays
    // required.
    let withheld = if preview {
        Applicability::TcvOnly
    } else {
        Applicability::Required
    };
    let unavailable = Reason::CatalogPinCompositionUnavailable;
    let (applicability, (verdict, reason, detail)) = match (pin, &input.acceptance_policy) {
        _ if input.unsupported_terms.contains(&line) => {
            (base, failed(Reason::BillingTermsUnsupported))
        }
        (PinOutcome::Unresolvable(r), _) => (base, failed(*r)),
        (PinOutcome::Unevaluable(r), _) => (base, unevaluable(*r)),
        // D-228 item 6: withheld terms are predicate 1's reason, never a port's (not tcv-only).
        (PinOutcome::Composed { .. }, _) if let Some(w) = payer_withheld => (base, w.clone()),
        (PinOutcome::Composed { .. }, PortAnswer::Unavailable(_) | PortAnswer::Skipped(_)) => {
            (base, unevaluable(unavailable))
        }
        (PinOutcome::Composed { .. }, _) if terms_missing => (withheld, unevaluable(unavailable)),
        (PinOutcome::Composed { .. }, PortAnswer::Answered(None)) if preview => (
            Applicability::PreviewForecast,
            unevaluable(Reason::OrderBindingPolicyMissing),
        ),
        (PinOutcome::Composed { .. }, PortAnswer::Answered(None)) => {
            (base, failed(Reason::OrderBindingPolicyMissing))
        }
        (PinOutcome::Composed { .. }, PortAnswer::Answered(Some(_))) => (base, PASS),
    };
    local(
        key(P::PinComposition, Some(line)),
        applicability,
        verdict,
        reason,
        detail,
    )
}
