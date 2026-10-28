//! Admission: the deterministic code that decides what of a plan is stored.
//! The model only proposes. Admission makes no model call and writes nothing
//! to graph storage; it has neither a model client nor a graph storage client.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::hash::Hash;
use std::ops::Range;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::error::DomainError;
use super::plan::{
    Origin, PieceMut, Plan, PropertyRef, Step, ValueRef, text_chars_up_to, text_piece_mut,
};
use super::record_intake::{DropEvent, DropReason, DropStage, IntakeEvents};
use super::sensitive_data::{Action, SensitiveDataKind, Verdict};
use super::subject_settings::{SubjectKey, SubjectSettingsReader};

/// Which members of a set a rule lets through.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Allowed<T: Eq + Hash> {
    /// Every member.
    Any,
    /// Only these members.
    Only(HashSet<T>),
}

impl<T: Eq + Hash> Allowed<T> {
    pub fn permits(&self, member: &T) -> bool {
        match self {
            Self::Any => true,
            Self::Only(members) => members.contains(member),
        }
    }
}

/// A tenant's admission rules, tenant settings in the settings service.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct AdmissionRules {
    /// The connectors whose records the tenant trusts, by connector identity.
    pub trusted_connectors: Allowed<String>,
    /// The lowest confidence, from 0 to 1, a model's step may carry.
    pub confidence_floor: f32,
    /// The entity kinds a plan may store.
    pub entity_kinds: Allowed<String>,
    /// The most steps one plan may hold.
    pub max_steps: usize,
    /// The longest value a plan may store, in characters of its text: every
    /// string and number in it together, never the keys.
    pub max_value_chars: usize,
}

/// Where a tenant's admission rules come from. Whether the settings service
/// holds them as one setting or several is decided with Construct's setting
/// declarations; the domain sees one set of rules.
#[async_trait]
pub trait AdmissionRulesSource: Send + Sync {
    /// # Errors
    ///
    /// When the rules cannot be read. Admission then decides nothing.
    async fn rules(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<AdmissionRules, DomainError>;
}

/// A content-free event for one sensitive-data kind enforced on one value.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SensitiveDataEvent {
    pub tenant_id: Uuid,
    pub subject_id: Uuid,
    pub origin: Origin,
    pub kind: SensitiveDataKind,
}

/// Where Admission's block and redaction events go. The Audit gear takes them
/// once it ships.
pub trait AdmissionEvents: Send + Sync {
    fn blocked(&self, event: SensitiveDataEvent);
    fn redacted(&self, event: SensitiveDataEvent);
}

/// One sensitive-data kind enforced on one text piece. `at` names the piece in
/// the plan as the checks saw it.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Enforced {
    pub at: ValueRef,
    pub kind: SensitiveDataKind,
}

/// The plan Admission lets through, and what it took out.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct AdmittedPlan {
    /// The steps to store. It can be empty when every value was blocked.
    pub plan: Plan,
    /// Text pieces whose value was dropped: blocked, or not redactable.
    pub blocked: Vec<Enforced>,
    /// Text pieces stored with the found content removed.
    pub redacted: Vec<Enforced>,
    /// Steps dropped because a value they store was blocked, by their place
    /// in the plan as the checks saw it. A replace whose new value is blocked
    /// is dropped whole, so the old value stays.
    pub dropped_steps: Vec<usize>,
}

/// What Admission decided.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum AdmissionOutcome {
    Admitted(Box<AdmittedPlan>),
    /// Nothing of the plan is stored. The drop event is written for a plan
    /// built from a record; for any other plan the caller audits the reason.
    Dropped(DropReason),
}

/// The collaborators of Admission.
#[domain_model]
pub struct AdmissionPorts {
    pub settings: Arc<dyn SubjectSettingsReader>,
    pub rules: Arc<dyn AdmissionRulesSource>,
    pub events: Arc<dyn AdmissionEvents>,
    pub drops: Arc<dyn IntakeEvents>,
}

/// @cpt-dod:cpt-cf-construct-dod-admission-rules:p1
#[domain_model]
pub struct Admission {
    ports: AdmissionPorts,
}

impl Admission {
    #[must_use]
    pub fn new(ports: AdmissionPorts) -> Self {
        Self { ports }
    }

    /// Decide what of `plan` is stored, given the verdicts of the checks.
    ///
    /// `ctx` and `scope` are the ones processing builds for the plan's tenant,
    /// never a connector's: its token can expire before processing runs.
    ///
    /// # Errors
    ///
    /// When the subject's settings or the tenant's rules cannot be read.
    /// Nothing is decided, and the caller drops the record.
    #[tracing::instrument(skip_all, fields(tenant_id = %plan.tenant_id, subject_id = %plan.subject_id, origin = plan.origin.kind()))]
    pub async fn admit(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        plan: Plan,
        verdicts: &[Verdict],
    ) -> Result<AdmissionOutcome, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-admission-admit:p1:inst-admit-subject
        let settings = self
            .ports
            .settings
            .read(
                ctx,
                scope,
                SubjectKey {
                    tenant_id: plan.tenant_id,
                    subject_id: plan.subject_id,
                },
            )
            .await?;
        if settings.erasure_in_progress {
            return Ok(self.drop(&plan, DropReason::ErasureInProgress));
        }
        if !settings.personalization_enabled {
            return Ok(self.drop(&plan, DropReason::PersonalizationOff));
        }
        // @cpt-end:cpt-cf-construct-algo-admission-admit:p1:inst-admit-subject

        // @cpt-begin:cpt-cf-construct-algo-admission-admit:p1:inst-admit-rules
        let rules = self.ports.rules.rules(ctx, plan.tenant_id).await?;
        if let Some(cause) = broken_rule(&plan, &rules) {
            return Ok(self.drop(&plan, cause));
        }
        // @cpt-end:cpt-cf-construct-algo-admission-admit:p1:inst-admit-rules

        // @cpt-begin:cpt-cf-construct-algo-admission-admit:p1:inst-admit-verdicts
        let admitted = enforce(plan, verdicts);
        self.write_events(&admitted);
        Ok(AdmissionOutcome::Admitted(Box::new(admitted)))
        // @cpt-end:cpt-cf-construct-algo-admission-admit:p1:inst-admit-verdicts
    }

    fn drop(&self, plan: &Plan, cause: DropReason) -> AdmissionOutcome {
        // @cpt-begin:cpt-cf-construct-algo-admission-admit:p1:inst-admit-drop
        if let Some(event) = DropEvent::for_plan(plan, DropStage::Admission, cause) {
            self.ports.drops.dropped(event);
        }
        AdmissionOutcome::Dropped(cause)
        // @cpt-end:cpt-cf-construct-algo-admission-admit:p1:inst-admit-drop
    }

    fn write_events(&self, admitted: &AdmittedPlan) {
        // @cpt-begin:cpt-cf-construct-algo-admission-enforce:p1:inst-enforce-events
        let event = |kind| SensitiveDataEvent {
            tenant_id: admitted.plan.tenant_id,
            subject_id: admitted.plan.subject_id,
            origin: admitted.plan.origin.clone(),
            kind,
        };
        // One event per value and kind, not per piece: two pieces of one value
        // that a kind blocks are one blocked value for that kind.
        let per_value = |enforced: &[Enforced]| {
            enforced
                .iter()
                .map(|e| (e.at.property_ref(), e.kind))
                .collect::<BTreeSet<_>>()
        };
        for (_, kind) in per_value(&admitted.blocked) {
            self.ports.events.blocked(event(kind));
        }
        for (_, kind) in per_value(&admitted.redacted) {
            self.ports.events.redacted(event(kind));
        }
        // @cpt-end:cpt-cf-construct-algo-admission-enforce:p1:inst-enforce-events
    }
}

/// The first admission rule the plan breaks, in the order the rules are
/// checked. A plan a person wrote skips source trust and the confidence floor:
/// no model proposed it and no connector sent it.
fn broken_rule(plan: &Plan, rules: &AdmissionRules) -> Option<DropReason> {
    let by_a_person = plan.origin.written_by_a_person();

    // @cpt-begin:cpt-cf-construct-algo-admission-admit:p1:inst-admit-trust
    if let Origin::Connector { connector, .. } = &plan.origin
        && !rules.trusted_connectors.permits(connector)
    {
        return Some(DropReason::UntrustedSource);
    }
    // @cpt-end:cpt-cf-construct-algo-admission-admit:p1:inst-admit-trust

    // @cpt-begin:cpt-cf-construct-algo-admission-admit:p1:inst-admit-confidence
    if !by_a_person
        && !plan
            .steps
            .iter()
            .all(|step| meets_floor(step, rules.confidence_floor))
    {
        return Some(DropReason::BelowConfidenceFloor);
    }
    // @cpt-end:cpt-cf-construct-algo-admission-admit:p1:inst-admit-confidence

    // @cpt-begin:cpt-cf-construct-algo-admission-admit:p1:inst-admit-kinds
    if plan
        .steps
        .iter()
        .any(|step| step.change.stores_values() && !rules.entity_kinds.permits(&step.entity_kind))
    {
        return Some(DropReason::EntityKindNotAllowed);
    }
    // @cpt-end:cpt-cf-construct-algo-admission-admit:p1:inst-admit-kinds

    // @cpt-begin:cpt-cf-construct-algo-admission-admit:p1:inst-admit-caps
    // The step count is cheap, so it goes first. A value is measured as a
    // whole, so an object of many short pieces cannot pass a cap that each
    // piece alone would; the count stops once it passes the cap.
    if plan.steps.len() > rules.max_steps {
        return Some(DropReason::WriteCapExceeded);
    }
    let too_long = plan.steps.iter().any(|step| {
        step.change.properties().iter().any(|property| {
            text_chars_up_to(&property.value, rules.max_value_chars) > rules.max_value_chars
        })
    });
    if too_long {
        return Some(DropReason::WriteCapExceeded);
    }
    // @cpt-end:cpt-cf-construct-algo-admission-admit:p1:inst-admit-caps

    None
}

/// A model's step meets the floor only with a confidence it gave; a step
/// without one, or with one that is not a number, does not.
fn meets_floor(step: &Step, floor: f32) -> bool {
    step.confidence
        .is_some_and(|confidence| confidence.is_finite() && confidence >= floor)
}

/// What the verdicts do to one text piece.
#[derive(Default)]
struct Treatment {
    block: BTreeSet<SensitiveDataKind>,
    /// The ranges of each redacting kind, or `None` once any of its findings
    /// on the piece named no range: such a kind cannot be redacted, and its
    /// other findings' ranges must not hide that.
    redact: BTreeMap<SensitiveDataKind, Option<Vec<Range<usize>>>>,
}

impl Treatment {
    /// Every kind that named the piece, blocking or redacting.
    fn kinds(&self) -> impl Iterator<Item = SensitiveDataKind> + '_ {
        self.block.iter().chain(self.redact.keys()).copied()
    }
}

/// Apply the verdicts to the text pieces they name. Block drops the whole value
/// the piece belongs to and keeps the rest of the plan; redact removes the
/// found content from the piece, so the value stays valid JSON. A piece that
/// cannot be redacted as the check asks (a number, or a range that does not
/// fit), or is empty after it, drops its value like a block. A step that keeps
/// no value, or a replace that loses any value, is dropped whole, so a replace
/// never removes the old value without storing the new one.
///
/// @cpt-dod:cpt-cf-construct-dod-admission-verdicts:p1
fn enforce(mut plan: Plan, verdicts: &[Verdict]) -> AdmittedPlan {
    let mut taken = Taken::default();
    for (at, treatment) in collect(verdicts) {
        apply(&mut plan, at, &treatment, &mut taken);
    }
    let dropped_steps = drop_blocked_values(&mut plan, &taken.blocked_values);

    let Taken {
        mut blocked,
        mut redacted,
        blocked_values,
    } = taken;
    // A redaction in a value that another piece blocked, or in a step dropped
    // whole, stored nothing. `dropped_steps` is in ascending order.
    redacted.retain(|enforced| {
        !blocked_values.contains(&enforced.at.property_ref())
            && dropped_steps.binary_search(&enforced.at.step).is_err()
    });
    blocked.sort();
    redacted.sort();
    AdmittedPlan {
        plan,
        blocked,
        redacted,
        dropped_steps,
    }
}

/// What enforcing the verdicts took out of the plan so far.
#[derive(Default)]
struct Taken {
    blocked: Vec<Enforced>,
    redacted: Vec<Enforced>,
    blocked_values: BTreeSet<PropertyRef>,
}

impl Taken {
    fn block(&mut self, at: ValueRef, kinds: impl Iterator<Item = SensitiveDataKind>) {
        self.blocked.extend(kinds.map(|kind| Enforced { at, kind }));
        self.blocked_values.insert(at.property_ref());
    }
}

/// What the verdicts do to each text piece they name.
fn collect(verdicts: &[Verdict]) -> BTreeMap<ValueRef, Treatment> {
    // @cpt-begin:cpt-cf-construct-algo-admission-enforce:p1:inst-enforce-collect
    let mut treatments: BTreeMap<ValueRef, Treatment> = BTreeMap::new();
    for verdict in verdicts {
        for found in &verdict.found {
            let treatment = treatments.entry(found.at).or_default();
            match verdict.kind.action() {
                Action::Block => {
                    treatment.block.insert(verdict.kind);
                }
                Action::Redact => {
                    let ranges = treatment
                        .redact
                        .entry(verdict.kind)
                        .or_insert_with(|| Some(Vec::new()));
                    if found.spans.is_empty() {
                        *ranges = None;
                    } else if let Some(ranges) = ranges {
                        ranges.extend(found.spans.iter().cloned());
                    }
                }
            }
        }
    }
    treatments
    // @cpt-end:cpt-cf-construct-algo-admission-enforce:p1:inst-enforce-collect
}

/// Apply the verdicts on one text piece: block its value, or redact the piece.
fn apply(plan: &mut Plan, at: ValueRef, treatment: &Treatment, taken: &mut Taken) {
    let Some(value) = value_mut(plan, at) else {
        // No such value in the plan, so nothing of it can be stored.
        tracing::warn!(
            step = at.step,
            property = at.property,
            "a verdict names a value the plan does not hold"
        );
        return;
    };
    // @cpt-begin:cpt-cf-construct-algo-admission-enforce:p1:inst-enforce-block
    // A verdict on a piece the value does not have fails closed: the whole
    // value is blocked for every kind that named it.
    let Some(piece) = text_piece_mut(value, at.piece) else {
        taken.block(at, treatment.kinds());
        return;
    };
    if !treatment.block.is_empty() {
        taken.block(at, treatment.block.iter().copied());
        return;
    }
    // @cpt-end:cpt-cf-construct-algo-admission-enforce:p1:inst-enforce-block
    // @cpt-begin:cpt-cf-construct-algo-admission-enforce:p1:inst-enforce-redact
    let kinds = treatment.redact.keys().copied();
    if redact_piece(piece, treatment) {
        taken
            .redacted
            .extend(kinds.map(|kind| Enforced { at, kind }));
    } else {
        taken.block(at, kinds);
    }
    // @cpt-end:cpt-cf-construct-algo-admission-enforce:p1:inst-enforce-redact
}

/// Remove every redacting kind's ranges from a string piece. `false` when the
/// piece cannot be redacted as asked: it is a number, a redacting kind named
/// no range (another kind's ranges must not hide that), a range does not fit,
/// or only white space would be left.
fn redact_piece(piece: PieceMut<'_>, treatment: &Treatment) -> bool {
    let PieceMut::Text(text) = piece else {
        return false;
    };
    let Some(spans) = treatment
        .redact
        .values()
        .map(Option::as_deref)
        .collect::<Option<Vec<&[Range<usize>]>>>()
    else {
        return false;
    };
    let spans: Vec<Range<usize>> = spans.concat();
    match redact(text, &spans).filter(|kept| !kept.trim().is_empty()) {
        Some(kept) => {
            *text = kept;
            true
        }
        None => false,
    }
}

/// Drop each blocked value from its step. A replace that loses any value, or
/// a step that keeps none, is dropped whole. Returns the dropped steps, in
/// ascending order.
fn drop_blocked_values(plan: &mut Plan, blocked_values: &BTreeSet<PropertyRef>) -> Vec<usize> {
    // @cpt-begin:cpt-cf-construct-algo-admission-enforce:p1:inst-enforce-steps
    let mut dropped_steps = Vec::new();
    let steps = std::mem::take(&mut plan.steps);
    for (index, mut step) in steps.into_iter().enumerate() {
        let lost: Vec<usize> = blocked_values
            .iter()
            .filter(|at| at.step == index)
            .map(|at| at.property)
            .collect();
        if lost.is_empty() {
            plan.steps.push(step);
            continue;
        }
        if !step.change.replaces()
            && let Some(properties) = step.change.properties_mut()
        {
            for &property in lost.iter().rev() {
                properties.remove(property);
            }
            if !properties.is_empty() {
                plan.steps.push(step);
                continue;
            }
        }
        dropped_steps.push(index);
    }
    dropped_steps
    // @cpt-end:cpt-cf-construct-algo-admission-enforce:p1:inst-enforce-steps
}

fn value_mut(plan: &mut Plan, at: ValueRef) -> Option<&mut Value> {
    plan.steps.get_mut(at.step)?.change.value_mut(at.property)
}

/// The value without the content in `spans`, byte ranges into it. `None` when
/// there is no span, or a span is empty, out of the value or not on a
/// character boundary: the found content cannot be removed as asked.
fn redact(value: &str, spans: &[Range<usize>]) -> Option<String> {
    if spans.is_empty() {
        return None;
    }
    let mut spans = spans.to_vec();
    spans.sort_by_key(|span| span.start);
    let mut kept = String::with_capacity(value.len());
    let mut from = 0;
    for span in spans {
        if span.is_empty() || value.get(span.clone()).is_none() {
            return None;
        }
        if span.start > from {
            kept.push_str(value.get(from..span.start)?);
        }
        from = from.max(span.end);
    }
    kept.push_str(value.get(from..)?);
    Some(kept)
}
