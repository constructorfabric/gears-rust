//! Engine-owned contribution mapping (Foundation §3.6 steps 14-21 and *Step 19 aggregate
//! contribution mapping*) and the authored-field classification (02 §3.1 / DESIGN 02 §4.3).
//!
//! Slices supply validated contributions; only the engine turns them into aggregate writes. The
//! planner is pure: it derives the complete post-state of the aggregate from the locked facts,
//! the row and the contribution, and refuses a contribution the row does not declare. The
//! transaction adapter persists exactly the planned post-state under the aggregate lock.
use std::collections::{BTreeMap, BTreeSet};
use toolkit_macros::domain_model;

use bss_orders_sdk::catalog::{OrderState, Trigger};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use super::audit::{AdminChange, AdminField};
use super::state_table::{ContributionKind, Row, RowId, is_terminal};

// ---------------------------------------------------------------------------------------------
// Field classification (D-62, D-118, D-119, D-145)

/// The class of one authored field. Every authored field has exactly one.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FieldClass {
    /// Immutable from `submitted`; a change appends a version and re-runs the gate.
    Commercial,
    /// Never changes after `submitted`, even by amendment; the seller from creation.
    CommercialFrozen,
    /// Editable in any non-terminal state, audited per field, never versioned.
    Administrative,
}

/// Where an authored field lives.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FieldScope {
    /// Order-header field.
    Order,
    /// Order-line field.
    Line,
}

/// The authored field inventory of the order and line request models (`snake_case` wire names,
/// `models.json` `field_classes`). `membership` (line insert/removal) and
/// `selected_items[].quantity` (carried inside `selected_items`) are classified content without a
/// PATCH name of their own. Read-through renewal terms are never authored and have no entry
/// (02 §4.3).
pub const AUTHORED_FIELDS: &[(FieldScope, &str)] = &[
    (FieldScope::Order, "category"),
    (FieldScope::Order, "contract_id"),
    (FieldScope::Order, "payer_tenant_id"),
    (FieldScope::Order, "resource_tenant_id"),
    (FieldScope::Order, "seller_tenant_id"),
    (FieldScope::Order, "external_reference"),
    (FieldScope::Order, "display_label"),
    (FieldScope::Order, "internal_notes"),
    (FieldScope::Line, "membership"),
    (FieldScope::Line, "plan_id"),
    (FieldScope::Line, "plan_revision_id"),
    (FieldScope::Line, "selected_items"),
    (FieldScope::Line, "selected_items[].quantity"),
    (FieldScope::Line, "currency"),
    (FieldScope::Line, "contract_effective_date"),
    (FieldScope::Line, "service_activation_date"),
    (FieldScope::Line, "acceptance_due_date"),
    (FieldScope::Line, "term_duration"),
    (FieldScope::Line, "billing_cycle"),
    (FieldScope::Line, "external_reference"),
    (FieldScope::Line, "display_label"),
    (FieldScope::Line, "internal_notes"),
];

use FieldClass::{Administrative, Commercial, CommercialFrozen};

/// The one declaration read by Capture and Versioning (02 §4.3: "neither maintains its own
/// list").
pub const FIELD_CLASSES: &[(FieldScope, &str, FieldClass)] = &[
    (FieldScope::Order, "category", Commercial),
    (FieldScope::Order, "contract_id", Commercial),
    (FieldScope::Order, "payer_tenant_id", Commercial),
    (FieldScope::Order, "resource_tenant_id", CommercialFrozen),
    (FieldScope::Order, "seller_tenant_id", CommercialFrozen),
    (FieldScope::Order, "external_reference", Administrative),
    (FieldScope::Order, "display_label", Administrative),
    (FieldScope::Order, "internal_notes", Administrative),
    (FieldScope::Line, "membership", Commercial),
    (FieldScope::Line, "plan_id", Commercial),
    (FieldScope::Line, "plan_revision_id", Commercial),
    (FieldScope::Line, "selected_items", Commercial),
    (FieldScope::Line, "selected_items[].quantity", Commercial),
    (FieldScope::Line, "currency", Commercial),
    (FieldScope::Line, "contract_effective_date", Commercial),
    (FieldScope::Line, "service_activation_date", Commercial),
    (FieldScope::Line, "acceptance_due_date", Commercial),
    (FieldScope::Line, "term_duration", Commercial),
    (FieldScope::Line, "billing_cycle", Commercial),
    (FieldScope::Line, "external_reference", Administrative),
    (FieldScope::Line, "display_label", Administrative),
    (FieldScope::Line, "internal_notes", Administrative),
];

/// Inconsistency between the authored field inventory and the class declaration; the gear
/// refuses to start.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FieldClassError {
    /// An authored field has no class.
    #[error("authored field {0:?}/{1} is unclassified")]
    Unclassified(FieldScope, &'static str),
    /// An authored field is classified more than once.
    #[error("authored field {0:?}/{1} is classified more than once")]
    Duplicate(FieldScope, &'static str),
    /// A classified field is not in the authored inventory.
    #[error("classified field {0:?}/{1} is not an authored field")]
    NotAuthored(FieldScope, &'static str),
    /// The authored inventory lists a field twice.
    #[error("authored field {0:?}/{1} is listed twice")]
    DuplicateField(FieldScope, &'static str),
}

/// The compiled classification.
#[domain_model]
#[derive(Debug, Clone)]
pub struct FieldClassification {
    classes: BTreeMap<(FieldScope, &'static str), FieldClass>,
}

/// What a request's named fields select (one request, one trigger).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// The trigger the named fields select: `draft-mutate` or `administrative-edit`.
    pub trigger: Trigger,
    /// Commercial and administrative fields together: the `mixed-field-classes` guard input.
    pub mixed: bool,
}

impl FieldClassification {
    /// Compile the shipped declarations.
    ///
    /// # Errors
    /// Never for the shipped declarations (a unit test proves it).
    pub fn registered() -> Result<Self, FieldClassError> {
        Self::compile(AUTHORED_FIELDS, FIELD_CLASSES)
    }

    /// Every authored field classified exactly once; nothing classified that is not authored.
    ///
    /// # Errors
    /// Any [`FieldClassError`]; the gear refuses to start.
    pub fn compile(
        authored: &[(FieldScope, &'static str)],
        declared: &[(FieldScope, &'static str, FieldClass)],
    ) -> Result<Self, FieldClassError> {
        let mut inventory = BTreeSet::new();
        for (scope, name) in authored {
            if !inventory.insert((*scope, *name)) {
                return Err(FieldClassError::DuplicateField(*scope, name));
            }
        }
        let mut classes = BTreeMap::new();
        for (scope, name, class) in declared {
            if !inventory.contains(&(*scope, *name)) {
                return Err(FieldClassError::NotAuthored(*scope, name));
            }
            if classes.insert((*scope, *name), *class).is_some() {
                return Err(FieldClassError::Duplicate(*scope, name));
            }
        }
        if let Some((scope, name)) = inventory.iter().find(|k| !classes.contains_key(*k)) {
            return Err(FieldClassError::Unclassified(*scope, name));
        }
        Ok(Self { classes })
    }

    /// The class of the named field in `scope`, or `None` when it is not classified.
    #[must_use]
    pub fn class(&self, scope: FieldScope, name: &str) -> Option<FieldClass> {
        self.classes
            .iter()
            .find(|((s, n), _)| *s == scope && *n == name)
            .map(|(_, c)| *c)
    }

    /// Select the trigger from the named fields' classes alone, without reading state (D-145).
    /// Any commercial or commercial-frozen field selects `draft-mutate`; administrative-only
    /// fields select `administrative-edit`. `None` for an empty or unknown field set, which is
    /// boundary `request-invalid`.
    #[must_use]
    pub fn select(&self, fields: &[(FieldScope, &str)]) -> Option<Selection> {
        if fields.is_empty() {
            return None;
        }
        let classes = fields
            .iter()
            .map(|(scope, name)| self.class(*scope, name))
            .collect::<Option<Vec<_>>>()?;
        let administrative = classes.contains(&Administrative);
        let commercial = classes.iter().any(|c| *c != Administrative);
        Some(Selection {
            trigger: if commercial {
                Trigger::DraftMutate
            } else {
                Trigger::AdministrativeEdit
            },
            mixed: administrative && commercial,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Locked aggregate facts and contributions

/// The aggregate's business facts as read under the row lock (never caller input).
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct AggregateFacts {
    /// The order's identifier.
    pub order_id: Uuid,
    /// The order's current lifecycle state.
    pub state: OrderState,
    /// When the order entered its current state.
    pub state_entered_at: OffsetDateTime,
    /// The current commercial version number.
    pub current_version: i32,
    /// The highest version number allocated so far (D-188 reservations).
    pub version_allocation_high_water: i32,
    /// The draft revision counter, incremented by each row-2 draft edit.
    pub draft_revision: i64,
    /// The state the order held before entering `on_hold`; `None` when not on hold.
    pub pre_hold_state: Option<OrderState>,
    /// How many times the order has been resumed from hold.
    pub resume_count: i32,
    /// How many amendments the order has taken.
    pub amendment_count: i32,
    /// The D-198 fulfillment-control dispatch generation.
    pub fulfillment_control_generation: i64,
    /// The pending fulfillment control, if any; a generation change requires none.
    pub fulfillment_control_pending: Option<Uuid>,
    /// When the write-once spawn signal was set; `None` before row 12.
    pub spawn_signal_at: Option<OffsetDateTime>,
    /// When a failed authorization was tolerated; set once, never cleared.
    pub authorization_failure_tolerated_at: Option<OffsetDateTime>,
    /// The recorded compensation evidence, if any.
    pub compensation_evidence: Option<Value>,
    /// The resource tenant.
    pub resource_tenant_id: Uuid,
    /// The seller tenant; never changes (D-119).
    pub seller_tenant_id: Uuid,
    /// The payer tenant.
    pub payer_tenant_id: Uuid,
    /// The order category.
    pub category: String,
    /// The contract reference, if any.
    pub contract_id: Option<Uuid>,
}

/// The proposed commercial version of a versioning row (D-188 reserved candidate).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionContribution {
    /// The candidate reserved by the D-188 attempt; never `current + 1` by assumption.
    pub candidate: i32,
    /// The payer tenant of the proposed version.
    pub payer_tenant_id: Uuid,
    /// The category of the proposed version.
    pub category: String,
    /// The contract reference of the proposed version, if any.
    pub contract_id: Option<Uuid>,
    /// The market currency of the proposed version, if any.
    pub market_currency: Option<String>,
    /// The market region of the proposed version, if any.
    pub market_region: Option<String>,
    /// Amendment explanation (04 §3.1 step 8); `None` on submit.
    pub amendment_reason: Option<String>,
}

/// Row 2's aggregate-held commercial header changes (02 §3.6 *Edit Order* step 4: "the engine
/// writes them to the aggregate under its lock"). The resource/payer axes travel separately as the
/// PEP-authorized proposed arrangement (08 §4.3) and the seller never changes (D-119). `None`
/// leaves a value as locked; `Some(None)` clears the nullable contract reference.
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DraftHeaderEdit {
    /// The new category; `None` keeps the locked value.
    pub category: Option<String>,
    /// The new contract reference: `None` keeps it, `Some(None)` clears it, `Some(Some(_))`
    /// replaces it.
    #[expect(
        clippy::option_option,
        reason = "omitted, cleared and replaced are three distinct edits (02 §3.6)"
    )]
    pub contract_id: Option<Option<Uuid>>,
}
impl DraftHeaderEdit {
    /// Apply the named header values to the planned post-state (the engine's step-19 write).
    pub fn apply_to(&self, after: &mut AggregateFacts) {
        if let Some(category) = &self.category {
            after.category.clone_from(category);
        }
        if let Some(contract_id) = self.contract_id {
            after.contract_id = contract_id;
        }
    }
}

/// The aggregate contribution a row declares (Foundation *Step 19 aggregate contribution
/// mapping*). Child documents (lines, totals, acceptance, reflections, grants, gate outcomes,
/// admin rows) travel separately through the engine's child-document writer.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum AggregateContribution {
    /// Rows without an aggregate field contribution.
    None,
    /// Row 2: increment `draft_revision` once and write any named commercial header value.
    DraftEdit(DraftHeaderEdit),
    /// Rows 4, 18-20: append the reserved candidate version and move the pointer.
    Version(VersionContribution),
    /// Row 11: `tolerated` only when the registered tolerance guard admitted a failed
    /// authorization (05 §3.3 step 5); otherwise the flag is preserved, never cleared.
    BeginFulfillment {
        /// Whether the registered tolerance guard admitted a failed authorization.
        tolerated: bool,
    },
    /// Row 12: write-once `spawn_signal_at` and a checked generation increment; the D-198 grant
    /// itself is a child document.
    SpawnSignal,
    /// Rows 14, 16, 26, 27: validated ordinary compensation evidence.
    CompensationEvidence(Value),
    /// Rows 28, 29: the forced evidence variant with `unknown` assertions and attestation.
    ForcedFailure(Value),
}

impl AggregateContribution {
    /// The kind of this contribution, compared against the row's declared kind.
    #[must_use]
    pub fn kind(&self) -> ContributionKind {
        match self {
            Self::None => ContributionKind::None,
            Self::DraftEdit(_) => ContributionKind::DraftEdit,
            Self::Version(_) => ContributionKind::Version,
            Self::BeginFulfillment { .. } => ContributionKind::BeginFulfillment,
            Self::SpawnSignal => ContributionKind::SpawnSignal,
            Self::CompensationEvidence(_) => ContributionKind::CompensationEvidence,
            Self::ForcedFailure(_) => ContributionKind::ForcedFailure,
        }
    }
}

/// Contract violation between a row and its contribution: an infrastructure abort, never a
/// business refusal.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContributionError {
    /// The contribution does not match the kind the row declares.
    #[error("{0} requires a different aggregate contribution")]
    RowMismatch(RowId),
    /// The row has no effective target from the outgoing state.
    #[error("{0} has no effective target")]
    NoTarget(RowId),
    /// The version candidate is not above the current version or exceeds the allocation high water.
    #[error("version candidate must be above the current version and within the allocation")]
    Candidate,
    /// A resume, amendment, draft-revision or generation counter would overflow.
    #[error("{0}: a counter is exhausted")]
    Counter(RowId),
    /// The spawn signal is already set.
    #[error("the spawn signal is write-once")]
    SpawnRewrite,
    /// A generation change was planned while a fulfillment control is pending.
    #[error("a D-198 generation change requires no pending fulfillment control")]
    ControlPending,
}

/// How the transition treats `pre_hold_state` (§3.6 step 20.2-20.3).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreHold {
    /// Leave `pre_hold_state` unchanged (the state does not change).
    Keep,
    /// Store the outgoing state on entering `on_hold`.
    Store(OrderState),
    /// Clear it on any other state change.
    Clear,
}

/// The version a versioning row appends (step 18): the reserved candidate and the current
/// version it supersedes.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionStep {
    /// The reserved candidate version the row appends.
    pub candidate: i32,
    /// The outgoing current version the candidate supersedes.
    pub supersedes: i32,
}

/// The complete planned aggregate change (steps 14-21; step 19's field mapping).
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "One flag per independent step-19/20/21 aggregate effect the contract enumerates"
)]
pub struct Effects {
    /// The state before the transition.
    pub outgoing: OrderState,
    /// The effective target state.
    pub target: OrderState,
    /// Whether the target differs from the outgoing state.
    pub state_changed: bool,
    /// The `pre_hold_state` treatment.
    pub pre_hold: PreHold,
    /// Increment `resume_count` (a state-changing resume).
    pub resume_increment: bool,
    /// Increment `amendment_count` (an amendment row).
    pub amendment_increment: bool,
    /// Increment `draft_revision` (a row-2 draft edit).
    pub draft_revision_increment: bool,
    /// The appended version, for a versioning row.
    pub version: Option<VersionStep>,
    /// Write `spawn_signal_at` at the transition timestamp.
    pub spawn_signal: bool,
    /// Set `authorization_failure_tolerated_at` if not already set.
    pub tolerated: bool,
    /// Compensation evidence to record, if any.
    pub compensation_evidence: Option<Value>,
    /// D-198 dispatch generation increment (row 12; row 22 resume after a post-spawn hold).
    pub generation_increment: bool,
}
impl Effects {
    /// Whether this transition issues D-198 dispatch authority (a grant child document).
    #[must_use]
    pub fn issues_grant(&self) -> bool {
        self.generation_increment
    }
}

/// Whether a resume restores a post-spawn fulfillment, which issues a successor grant (D-198).
#[must_use]
pub fn is_post_spawn_resume(row: &Row, facts: &AggregateFacts) -> bool {
    row.trigger == Trigger::Resume
        && facts.pre_hold_state == Some(OrderState::InFulfillment)
        && facts.spawn_signal_at.is_some()
}

/// Plan the aggregate effects of an admitted row (after guards and the step-15 target check).
///
/// # Errors
/// A contribution the row does not declare, a missing target, an invalid candidate or an
/// exhausted counter.
pub fn plan(
    row: &Row,
    facts: &AggregateFacts,
    contribution: &AggregateContribution,
) -> Result<Effects, ContributionError> {
    use AggregateContribution as C;
    // Create (row 1) has its own branch and never reaches this planner.
    if row.id.number() == 1 || row.contribution != contribution.kind() {
        return Err(ContributionError::RowMismatch(row.id));
    }
    let outgoing = facts.state;
    let target = row
        .effective_target(outgoing, facts.pre_hold_state)
        .ok_or(ContributionError::NoTarget(row.id))?;
    let state_changed = target != outgoing;
    let pre_hold = if !state_changed {
        PreHold::Keep
    } else if target == OrderState::OnHold {
        PreHold::Store(outgoing)
    } else {
        PreHold::Clear
    };
    let resume_increment = state_changed && row.trigger == Trigger::Resume;
    if resume_increment && facts.resume_count.checked_add(1).is_none() {
        return Err(ContributionError::Counter(row.id));
    }
    let amendment_increment = row.trigger == Trigger::Amendment;
    if amendment_increment && facts.amendment_count.checked_add(1).is_none() {
        return Err(ContributionError::Counter(row.id));
    }
    let draft_revision_increment = matches!(contribution, C::DraftEdit(_));
    if draft_revision_increment && facts.draft_revision.checked_add(1).is_none() {
        return Err(ContributionError::Counter(row.id));
    }
    let version = match contribution {
        C::Version(v) => {
            if v.candidate <= facts.current_version
                || v.candidate > facts.version_allocation_high_water
            {
                return Err(ContributionError::Candidate);
            }
            Some(VersionStep {
                candidate: v.candidate,
                supersedes: facts.current_version,
            })
        }
        _ => None,
    };
    let spawn_signal = matches!(contribution, C::SpawnSignal);
    if spawn_signal && facts.spawn_signal_at.is_some() {
        return Err(ContributionError::SpawnRewrite);
    }
    let generation_increment = spawn_signal || is_post_spawn_resume(row, facts);
    if generation_increment {
        if facts.fulfillment_control_pending.is_some() {
            return Err(ContributionError::ControlPending);
        }
        if facts
            .fulfillment_control_generation
            .checked_add(1)
            .is_none()
        {
            return Err(ContributionError::Counter(row.id));
        }
    }
    let compensation_evidence = match contribution {
        C::CompensationEvidence(v) | C::ForcedFailure(v) => Some(v.clone()),
        _ => None,
    };
    Ok(Effects {
        outgoing,
        target,
        state_changed,
        pre_hold,
        resume_increment,
        amendment_increment,
        draft_revision_increment,
        version,
        spawn_signal,
        tolerated: matches!(contribution, C::BeginFulfillment { tolerated: true }),
        compensation_evidence,
        generation_increment,
    })
}

/// The post-state the engine writes (steps 18-21) at transition timestamp `t`.
#[must_use]
pub fn apply(facts: &AggregateFacts, effects: &Effects, t: OffsetDateTime) -> AggregateFacts {
    let mut next = facts.clone();
    if effects.state_changed {
        next.state = effects.target;
        next.state_entered_at = t;
    }
    match effects.pre_hold {
        PreHold::Keep => {}
        PreHold::Store(state) => next.pre_hold_state = Some(state),
        PreHold::Clear => next.pre_hold_state = None,
    }
    if effects.resume_increment {
        next.resume_count += 1;
    }
    if effects.amendment_increment {
        next.amendment_count += 1;
    }
    if effects.draft_revision_increment {
        next.draft_revision += 1;
    }
    if let Some(step) = effects.version {
        next.current_version = step.candidate;
    }
    if effects.spawn_signal {
        next.spawn_signal_at = Some(t);
    }
    // Set only when the tolerance guard admitted it; never cleared (Foundation step 19).
    if effects.tolerated && next.authorization_failure_tolerated_at.is_none() {
        next.authorization_failure_tolerated_at = Some(t);
    }
    if let Some(evidence) = &effects.compensation_evidence {
        next.compensation_evidence = Some(evidence.clone());
    }
    if effects.generation_increment {
        next.fulfillment_control_generation += 1;
    }
    debug_assert!(!is_terminal(facts.state));
    next
}

/// Row 3's named changes against the values stored at the engine's locked read (04 §3.6 step 2,
/// D-142, D-149): each surviving change carries the locked stored value as its prior value, and
/// a named field whose new value equals the stored one is dropped. A preparation snapshot's
/// prior value is never trusted, so a concurrent edit can neither falsify an audited prior value
/// nor turn a checked change into a no-op. Order is preserved; empty means nothing changes.
#[must_use]
pub fn reconcile_administrative(
    named: &[AdminChange],
    stored: impl Fn(AdminField) -> Option<String>,
) -> Vec<AdminChange> {
    named
        .iter()
        .filter_map(|change| {
            let prior = stored(change.field);
            (prior != change.new).then(|| AdminChange {
                field: change.field,
                prior,
                new: change.new.clone(),
            })
        })
        .collect()
}

/// Rows whose terminal/hold effect after the spawn signal must run the D-198 staged
/// receiver-control subflow (Foundation step 2; 06 §3.4 step 3; 07 §2.1 step 2). D-182's forced
/// exit is its explicit exception.
#[must_use]
pub fn requires_receiver_control(row: &Row, facts: &AggregateFacts) -> bool {
    let effective = match (facts.state, facts.pre_hold_state) {
        (OrderState::OnHold, Some(pre)) => pre,
        (state, _) => state,
    };
    let post_spawn = facts.spawn_signal_at.is_some() && effective == OrderState::InFulfillment;
    post_spawn
        && match row.trigger {
            Trigger::Hold => facts.state == OrderState::InFulfillment,
            Trigger::Cancel | Trigger::CancelWorkflowMediated | Trigger::AcknowledgeFailed => true,
            _ => false,
        }
}

/// Whether the row commits through the D-188 commercial-attempt subflow.
#[must_use]
pub fn requires_commercial_attempt(row: &Row) -> bool {
    row.is_versioning() && row.trigger != Trigger::Create
}

#[cfg(test)]
#[path = "contributions_tests.rs"]
pub mod tests;
