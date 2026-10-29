//! D-187 typed Pricing/Products read mapping (DESIGN `#contract-03-4-1` table and
//! `#contract-03-pricing-read-mapping`): one line's `current_revision`, `resolve` and SKU answers
//! into the adopted catalog predicates 1–8, Orders' `ReferenceResolution` and the line's pin.
//! Pure: no call, no storage, no money arithmetic (D-216). An absent field never passes.
//!
//! Reason vocabulary of a non-passing row:
//! - `catalog-predicates-unavailable`: a port gave **no answer** (outage, denied, deadline,
//!   malformed envelope, missing provider: [`ReadFailure::Unavailable`]). Predicates 2, 7 and 8
//!   carry it here because their port (the D-189 assessment) is not delivered; the gate overrides
//!   them from the assessment answer.
//! - `catalog-predicate-unevaluable`: a port **answered**, but the answer cannot evaluate the
//!   predicate (an unknown `PriceState`, an answered SKU of another id, the slot predicates after
//!   `resolve` answered `REVISION_NOT_PUBLISHED`, `REVISION_NOT_YET_AVAILABLE` or
//!   `INCOMPLETE_COMMERCIAL_INPUTS`, and the dated/sellable predicates of an uncovered slot).
//! - `pricing-revision-unavailable` / `pricing-revision-absent`: predicate 1's own port reasons,
//!   which the dependents of an absent revision share.
//! - `catalog-predicate-failed`: the answer proves the predicate false.
use super::compose::local;
use super::evidence::UpstreamDetail;
use super::ports::{PortAnswer, Unavailable};
use super::predicates::PredicateId as P;
use super::vector::{Applicability, Outcome, OutcomeKey, Verdict};
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::gate_report::CatalogSelection;
use bss_pricing_sdk::read::{
    AcceptedBinding, PriceState, ResolvedBindings, ResolvedCell, RevisionRef,
};
use bss_products_sdk::models::{Lifecycle, Sku};
use std::collections::BTreeMap;
use std::sync::Arc;
use time::Date;
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// Pricing's plan resource type: a `NotFound` of it naming the revision is "no such revision".
const PLAN_RESOURCE: &str = toolkit_gts::gts_id!("cf.bss.pricing.plan.v1~");
/// The resource name Pricing's `plan_missing` gives a missing revision (`pricing_reads.rs`).
const REVISION_RESOURCE: &str = "plan_revision";
/// Pricing's precondition violation type for a cell without complete commercial inputs.
const INCOMPLETE: &str = "INCOMPLETE_COMMERCIAL_INPUTS";
/// The `upstream_code` of a selected slot the resolved matrix does not contain.
const DIM_VALUE_UNKNOWN: &str = "dim-value-unknown";

/// One selected catalog slot of a line.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SelectedItem {
    /// The plan item selected.
    pub item_id: Uuid,
    /// The quantity, as `bss_money` canonical decimal text (D-216).
    pub quantity: String,
    /// The selected dimension value; `None` is a selected NULL (the default slot).
    pub selected_dim_value: Option<String>,
}

/// What one line asks of the catalog.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineCatalogRequest {
    /// The line.
    pub line_id: Uuid,
    /// The pricing plan.
    pub plan_id: Uuid,
    /// The pricing revision the line requested.
    pub revision_id: Uuid,
    /// The selected slots, in request order.
    pub items: Vec<SelectedItem>,
}

/// A typed failure of a Pricing read: `current_revision` or `resolve` (F03 steps 2–3).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadFailure {
    /// `resolve` answered that the revision does not exist.
    RevisionAbsent,
    /// `resolve` answered `REVISION_NOT_PUBLISHED`.
    RevisionNotPublished,
    /// `resolve` answered `REVISION_NOT_YET_AVAILABLE`.
    RevisionNotYetAvailable,
    /// `current_revision` answered `PLAN_UNPUBLISHED`: the plan has no published revision. Only
    /// `current_revision` raises it (Pricing `api/pricing_read.rs`), never `resolve`.
    PlanUnpublished,
    /// `resolve` answered `INCOMPLETE_COMMERCIAL_INPUTS` for the named field.
    IncompleteCommercialInputs(String),
    /// The read did not answer: the port outage.
    Unavailable(Unavailable),
}

impl ReadFailure {
    /// The Pricing code of an answered `resolve` "revision not usable" failure (rule 4).
    fn revision_code(&self) -> Option<&'static str> {
        match self {
            Self::RevisionNotPublished => Some("REVISION_NOT_PUBLISHED"),
            Self::RevisionNotYetAvailable => Some("REVISION_NOT_YET_AVAILABLE"),
            _ => None,
        }
    }
    /// The reason the slot predicates and `ReferenceResolution` carry when this `resolve`
    /// failure left them without input (rules 3–6): no answer is unavailable, an answered
    /// failure is unevaluable (module doc).
    fn dependent_reason(&self) -> Reason {
        match self {
            Self::RevisionAbsent => Reason::PricingRevisionAbsent,
            Self::Unavailable(_) => Reason::CatalogPredicatesUnavailable,
            _ => Reason::CatalogPredicateUnevaluable,
        }
    }
}

/// Classify a Pricing `current_revision` or `resolve` error (rule 13); the adapter calls it for
/// both ports. Only a revision-level `NotFound` of the plan resource is "absent"; any other
/// unrecognised answer is an outage, never a pass.
#[must_use]
pub fn classify_read_error(e: &CanonicalError) -> ReadFailure {
    match e {
        CanonicalError::NotFound { .. }
            if e.resource_type() == Some(PLAN_RESOURCE)
                && e.resource_name() == Some(REVISION_RESOURCE) =>
        {
            ReadFailure::RevisionAbsent
        }
        CanonicalError::Aborted { ctx, .. } => match ctx.reason.as_str() {
            "REVISION_NOT_PUBLISHED" => ReadFailure::RevisionNotPublished,
            "REVISION_NOT_YET_AVAILABLE" => ReadFailure::RevisionNotYetAvailable,
            "PLAN_UNPUBLISHED" => ReadFailure::PlanUnpublished,
            _ => ReadFailure::Unavailable(Unavailable::Outage),
        },
        CanonicalError::FailedPrecondition { ctx, .. } => ctx
            .violations
            .iter()
            .find(|v| v.type_ == INCOMPLETE)
            .map_or(ReadFailure::Unavailable(Unavailable::Outage), |v| {
                ReadFailure::IncompleteCommercialInputs(v.subject.clone())
            }),
        CanonicalError::PermissionDenied { .. } => ReadFailure::Unavailable(Unavailable::Denied),
        CanonicalError::DeadlineExceeded { .. } => ReadFailure::Unavailable(Unavailable::Deadline),
        CanonicalError::InvalidArgument { .. } => ReadFailure::Unavailable(Unavailable::Malformed),
        _ => ReadFailure::Unavailable(Unavailable::Outage),
    }
}

/// The catalog ports' answers for one line.
#[domain_model]
#[derive(Debug, Clone)]
pub struct CatalogAnswer {
    /// Pricing `current_revision` for the line's plan; `Err(Unavailable(_))` is the port outage,
    /// `Err(PlanUnpublished)` its answered business failure.
    pub current: Result<RevisionRef, ReadFailure>,
    /// Pricing `resolve` of the requested revision; `Err(Unavailable(_))` is the port outage.
    pub resolved: Result<Arc<ResolvedBindings>, ReadFailure>,
    /// Products SKU reads, by SKU id, for the bound slots.
    pub skus: BTreeMap<Uuid, PortAnswer<Sku>>,
}

/// The line's pin outcome (F03 step 10).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinOutcome {
    /// Every selected slot is bound on the confirmed current revision.
    Composed {
        /// The accepted bindings, in request order.
        bindings: Vec<AcceptedBinding>,
        /// The confirmed revision.
        revision: RevisionRef,
    },
    /// The answers prove the pin cannot be composed.
    Unresolvable(Reason),
    /// An input is missing or already failed; the reason is that input's.
    Unevaluable(Reason),
}

/// The mapped line: predicate outcomes and the pin.
#[domain_model]
#[derive(Debug, Clone)]
pub struct LineCatalog {
    /// Adopted predicates 1–8 and `ReferenceResolution`, one row per evaluation identity.
    pub outcomes: Vec<Outcome>,
    /// The line's pin outcome.
    pub pin: PinOutcome,
}

/// One `(item, selected dimension value)` slot of the request.
pub(crate) type Slot<'a> = (Uuid, Option<&'a str>);
/// One evaluated verdict with its reason and evidence.
type Eval = (Verdict, Option<Reason>, Option<UpstreamDetail>);
const PASS: Eval = (Verdict::Passed, None, None);
const UNAVAILABLE: Eval = unevaluable(Reason::CatalogPredicatesUnavailable);
const UNEVALUABLE: Eval = unevaluable(Reason::CatalogPredicateUnevaluable);
const MALFORMED: ReadFailure = ReadFailure::Unavailable(Unavailable::Malformed);
const fn unevaluable(r: Reason) -> Eval {
    (Verdict::Unevaluable, Some(r), None)
}
const fn failed(r: Reason, d: Option<UpstreamDetail>) -> Eval {
    (Verdict::Failed, Some(r), d)
}

/// The outcome rows of one line.
struct Rows {
    line: Uuid,
    out: Vec<Outcome>,
}
impl Rows {
    fn push(&mut self, p: P, slot: Option<Slot<'_>>, (verdict, reason, detail): Eval) {
        let key = OutcomeKey {
            predicate: p,
            line_id: Some(self.line),
            item_id: slot.map(|s| s.0),
            selection: slot.map_or(CatalogSelection::NotSelected, |s| {
                s.1.map_or(CatalogSelection::SelectedNull, |k| {
                    CatalogSelection::Selected(k.to_owned())
                })
            }),
        };
        let o = local(key, Applicability::Required, verdict, reason, detail);
        self.out.push(o);
    }
}

/// One line's Pricing answers after normalisation: what [`map_line`] maps and what the
/// assessment's engine inputs read, so a port the mapping treats as unavailable is also an
/// unresolvable input (D-227 item 1).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedCatalog<'a> {
    /// `current_revision`: the answer, `PlanUnpublished`, or `Unavailable(_)`.
    pub current: Result<&'a RevisionRef, ReadFailure>,
    /// `resolve` of the requested plan and revision: the answer, an answered `resolve` failure,
    /// or `Unavailable(_)`.
    pub resolved: Result<&'a ResolvedBindings, ReadFailure>,
}

impl NormalisedCatalog<'_> {
    /// Whether `current_revision` gave no usable answer.
    #[must_use]
    pub fn current_unavailable(&self) -> bool {
        matches!(self.current, Err(ReadFailure::Unavailable(_)))
    }
    /// Whether `resolve` gave no usable answer.
    #[must_use]
    pub fn resolve_unavailable(&self) -> bool {
        matches!(self.resolved, Err(ReadFailure::Unavailable(_)))
    }
}

/// Normalise one line's Pricing answers. Each port's failures are the ones Pricing raises from
/// it: `current_revision` only `PLAN_UNPUBLISHED` (`api/pricing_read.rs`), `resolve` only the
/// revision codes and `INCOMPLETE_COMMERCIAL_INPUTS` (`infra/pricing_reads.rs`). Anything else,
/// and a resolve of another plan or revision, is a malformed answer
/// (`Unavailable(Malformed)`), never a pass.
#[must_use]
pub fn normalise<'a>(req: &LineCatalogRequest, ans: &'a CatalogAnswer) -> NormalisedCatalog<'a> {
    let current = match &ans.current {
        Ok(c) => Ok(c),
        Err(f @ (ReadFailure::PlanUnpublished | ReadFailure::Unavailable(_))) => Err(f.clone()),
        Err(_) => Err(MALFORMED),
    };
    let resolved = match &ans.resolved {
        Ok(rb) if rb.plan_id == req.plan_id && rb.revision_id == req.revision_id => Ok(&**rb),
        Ok(_) | Err(ReadFailure::PlanUnpublished) => Err(MALFORMED),
        Err(f) => Err(f.clone()),
    };
    NormalisedCatalog { current, resolved }
}

/// Map one line's catalog answers into predicate outcomes and its pin (rules 1–12).
#[must_use]
pub fn map_line(req: &LineCatalogRequest, ans: &CatalogAnswer, resolve_date: Date) -> LineCatalog {
    let base = UpstreamDetail {
        plan_id: Some(req.plan_id),
        revision_id: Some(req.revision_id),
        ..UpstreamDetail::default()
    };
    let NormalisedCatalog { current, resolved } = normalise(req, ans);
    let confirmed = current
        .as_ref()
        .ok()
        .copied()
        .filter(|c| c.plan_id == req.plan_id && c.revision_id == req.revision_id);
    let mut slots: Vec<Slot<'_>> = Vec::new();
    for i in &req.items {
        let s = (i.item_id, i.selected_dim_value.as_deref());
        if !slots.contains(&s) {
            slots.push(s);
        }
    }
    let mut rows = Rows {
        line: req.line_id,
        out: Vec::new(),
    };
    let p1 = revision_current(&current, &resolved, confirmed.is_some(), &base);
    rows.push(P::CatalogRevisionCurrent, None, p1);
    // Predicates 2, 7, 8 (rule 10): no D-189 assessment is delivered.
    rows.push(P::CatalogRevisionAvailable, None, UNAVAILABLE);
    rows.push(P::CatalogEligibilityRules, None, UNAVAILABLE);
    let mut bindings = Vec::new();
    for &slot in &slots {
        rows.push(P::CatalogDimensionMarket, Some(slot), UNAVAILABLE);
        match &resolved {
            Ok(rb) => bindings.extend(slot_rows(&mut rows, rb, slot, ans, resolve_date, &base)),
            Err(f) => {
                for p in [
                    P::CatalogItemMembership,
                    P::CatalogSlotCoverage,
                    P::CatalogSkuVersionDated,
                    P::CatalogSkuSellable,
                ] {
                    rows.push(p, Some(slot), unevaluable(f.dependent_reason()));
                }
            }
        }
    }
    // ReferenceResolution (rule 11): duplicates are local, roster membership needs the resolve.
    let in_roster = |rb: &ResolvedBindings| {
        !req.items.is_empty()
            && req.items.iter().all(|i| {
                let id = i.item_id;
                rb.cells.iter().any(|c| c.selection.item_id == id)
            })
    };
    let reference = match &resolved {
        _ if slots.len() != req.items.len() => failed(Reason::ReferenceDuplicated, None),
        Err(f) => unevaluable(f.dependent_reason()),
        Ok(rb) if !in_roster(rb) => failed(Reason::ReferenceUnresolvable, None),
        Ok(_) => PASS,
    };
    let reference_failed = (reference.0 == Verdict::Failed)
        .then_some(reference.1)
        .flatten();
    rows.push(P::ReferenceResolution, None, reference);
    // Pin (rule 12): an input already failed or missing carries that input's reason once.
    let pin = match (&current, &resolved, confirmed) {
        (Err(ReadFailure::Unavailable(_)), _, _) => {
            PinOutcome::Unevaluable(Reason::PricingRevisionUnavailable)
        }
        (_, Err(ReadFailure::RevisionAbsent), _) => {
            PinOutcome::Unevaluable(Reason::PricingRevisionAbsent)
        }
        (_, Err(_), _) => PinOutcome::Unevaluable(Reason::CatalogPinCompositionUnavailable),
        // Predicate 1 failed (a current mismatch or `PLAN_UNPUBLISHED`): the pin carries
        // predicate 1's reason, so the failure list, deduplicated by `(line, reason)`, reports
        // it once.
        (_, Ok(_), None) => PinOutcome::Unevaluable(Reason::CatalogPredicateFailed),
        (_, Ok(_), Some(revision)) => match reference_failed {
            Some(r) => PinOutcome::Unevaluable(r),
            None if bindings.len() < slots.len() => {
                PinOutcome::Unresolvable(Reason::PinUnresolvable)
            }
            None => PinOutcome::Composed {
                bindings: bindings.into_iter().cloned().collect(),
                revision: revision.clone(),
            },
        },
    };
    LineCatalog {
        outcomes: rows.out,
        pin,
    }
}

/// Predicate 1 (rules 1–4): the current-revision port decides; an answered resolve failure about
/// the revision itself fails it; the current revision is never substituted.
fn revision_current(
    current: &Result<&RevisionRef, ReadFailure>,
    resolved: &Result<&ResolvedBindings, ReadFailure>,
    confirmed: bool,
    base: &UpstreamDetail,
) -> Eval {
    let coded = |code: Option<&str>| {
        failed(
            Reason::CatalogPredicateFailed,
            Some(UpstreamDetail {
                upstream_code: code.map(str::to_owned),
                ..base.clone()
            }),
        )
    };
    match (current, resolved) {
        // No current answer outranks any resolve answer, an absent revision included: P1 reads
        // both ports and unevaluable wins (D-195). The dependents keep the resolve's reason
        // (`pricing-revision-absent` when it answered absent).
        (Err(ReadFailure::Unavailable(_)), _) => unevaluable(Reason::PricingRevisionUnavailable),
        (_, Err(ReadFailure::RevisionAbsent)) => {
            failed(Reason::PricingRevisionAbsent, Some(base.clone()))
        }
        (_, Err(f)) if f.revision_code().is_some() => coded(f.revision_code()),
        // After normalisation the only answered `current_revision` failure is `PLAN_UNPUBLISHED`.
        (Err(_), _) => coded(Some("PLAN_UNPUBLISHED")),
        (Ok(c), _) if !confirmed => failed(
            Reason::CatalogPredicateFailed,
            Some(UpstreamDetail {
                current_revision_id: Some(c.revision_id),
                ..base.clone()
            }),
        ),
        _ => PASS,
    }
}

/// Predicates 3–6 of one slot over an answered resolve (rules 7–9); the binding when covered.
fn slot_rows<'a>(
    rows: &mut Rows,
    rb: &'a ResolvedBindings,
    slot: Slot<'_>,
    ans: &CatalogAnswer,
    date: Date,
    base: &UpstreamDetail,
) -> Option<&'a AcceptedBinding> {
    let member = rb.cells.iter().any(|c| c.selection.item_id == slot.0);
    let membership = if member {
        PASS
    } else {
        failed(Reason::CatalogPredicateFailed, Some(base.clone()))
    };
    rows.push(P::CatalogItemMembership, Some(slot), membership);
    let b = match coverage(&rb.cells, slot) {
        Ok(b) => b,
        Err(code) => {
            let d = UpstreamDetail {
                upstream_code: code.map(str::to_owned),
                ..base.clone()
            };
            rows.push(
                P::CatalogSlotCoverage,
                Some(slot),
                failed(Reason::CatalogPredicateFailed, Some(d)),
            );
            // No binding to read: the dated and sellable rows are unevaluable, never absent, so
            // the slot keeps every applicable predicate (F03 §5.1).
            rows.push(P::CatalogSkuVersionDated, Some(slot), UNEVALUABLE);
            rows.push(P::CatalogSkuSellable, Some(slot), UNEVALUABLE);
            return None;
        }
    };
    rows.push(P::CatalogSlotCoverage, Some(slot), PASS);
    let detail = |code: Option<&str>| {
        Some(UpstreamDetail {
            sku_id: Some(b.sku_id),
            sku_version: Some(b.sku_version),
            upstream_code: code.map(str::to_owned),
            ..base.clone()
        })
    };
    let (v, r) = dated(b, date);
    rows.push(P::CatalogSkuVersionDated, Some(slot), (v, r, detail(None)));
    let (v, r, code) = sellable(b, ans.skus.get(&b.sku_id));
    rows.push(P::CatalogSkuSellable, Some(slot), (v, r, detail(code)));
    Some(b)
}

/// Predicate 4 for one slot (rule 7): the bound cell, or the failure's `upstream_code`. A NULL
/// selection on a dimensioned item passes only when its default slot (the NULL cell) is bound.
/// `via_default` is evidence only: Pricing sets it for a dimension value that fell back to the
/// default chain, so it is always false on the NULL cell (`api/pricing_read.rs`).
fn coverage<'a>(
    cells: &'a [ResolvedCell],
    slot: Slot<'_>,
) -> Result<&'a AcceptedBinding, Option<&'static str>> {
    let cell = slot_cell(cells, slot).ok_or(Some(DIM_VALUE_UNKNOWN))?;
    cell_binding(cell, slot.0).ok_or(None)
}

/// The resolved cell of the selected slot `(item, dimension value)`, if Pricing returned one.
fn slot_cell<'a>(cells: &'a [ResolvedCell], (item, dim): Slot<'_>) -> Option<&'a ResolvedCell> {
    cells
        .iter()
        .find(|c| c.selection.item_id == item && c.selection.dimension_value.as_deref() == dim)
}

/// The cell's binding when it belongs to `item`.
fn cell_binding(cell: &ResolvedCell, item: Uuid) -> Option<&AcceptedBinding> {
    cell.binding.as_ref().filter(|b| b.item_id == item)
}

/// The binding of the selected slot: the one matcher `coverage` and the adapter's SKU reads share,
/// so the SKUs read are exactly the bindings the mapping judges.
pub(crate) fn selected_slot_binding<'a>(
    cells: &'a [ResolvedCell],
    slot: Slot<'_>,
) -> Option<&'a AcceptedBinding> {
    slot_cell(cells, slot).and_then(|c| cell_binding(c, slot.0))
}

/// Predicate 5 for a bound slot (rule 8): an approved price in force on the date. The window is
/// Pricing's `[effective_from, ends_on)`: `ends_on` is exclusive (`domain/price.rs`).
fn dated(b: &AcceptedBinding, date: Date) -> (Verdict, Option<Reason>) {
    let p = &b.price;
    match p.state {
        PriceState::Approved if p.effective_from <= date && p.ends_on.is_none_or(|e| date < e) => {
            (Verdict::Passed, None)
        }
        PriceState::Approved | PriceState::Cancelled => {
            (Verdict::Failed, Some(Reason::CatalogPredicateFailed))
        }
        _ => (
            Verdict::Unevaluable,
            Some(Reason::CatalogPredicateUnevaluable),
        ),
    }
}

/// Predicate 6 for a bound slot (rule 9): verdict, reason and the `upstream_code`.
fn sellable(
    b: &AcceptedBinding,
    sku: Option<&PortAnswer<Sku>>,
) -> (Verdict, Option<Reason>, Option<&'static str>) {
    let failed = |code| {
        (
            Verdict::Failed,
            Some(Reason::CatalogPredicateFailed),
            Some(code),
        )
    };
    match sku {
        Some(PortAnswer::Answered(s)) if s.id == b.sku_id => match (s.lifecycle, s.sellable) {
            (Lifecycle::Published, true) => (Verdict::Passed, None, None),
            (Lifecycle::Published, false) => failed("sku-not-sellable"),
            (Lifecycle::Deprecated, _) => failed("sku-deprecated"),
            (Lifecycle::Retired, _) => failed("sku-retired"),
            (Lifecycle::Draft, _) => failed("sku-draft"),
        },
        Some(PortAnswer::Answered(_)) => (
            Verdict::Unevaluable,
            Some(Reason::CatalogPredicateUnevaluable),
            None,
        ),
        Some(PortAnswer::Unavailable(_) | PortAnswer::Skipped(_)) | None => (
            Verdict::Unevaluable,
            Some(Reason::CatalogPredicatesUnavailable),
            None,
        ),
    }
}
