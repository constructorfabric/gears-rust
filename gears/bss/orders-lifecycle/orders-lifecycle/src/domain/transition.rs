//! The engine's pure decision procedure after the authoritative idempotency gate (Foundation
//! §3.6 *Attempt Transition* steps 3.1 and 10-15; DESIGN §4.1 guard order, D-110, D-113, D-147).
//!
//! It decides, from the locked aggregate facts alone, whether a new execution is refused by the
//! engine (`not-admissible`, `version-conflict`, `resume-target-missing`), by a registered slice
//! guard, by an unevaluable input, or admitted. The same procedure serves the normal and the
//! early input-failure branch: an unresolvable input never masks an admissibility, version,
//! draft-revision or earlier-registered guard refusal, and a precluded input is never treated as
//! unresolvable. Guard *logic* is supplied per request by the owning slice and bound by name to
//! the registry declaration; the engine checks the binding is complete and the returned reason
//! registered.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bss_orders_lifecycle_sdk::catalog::{OrderState, Reason, Trigger};
use time::OffsetDateTime;

use super::audit::AdminChange;
use super::contributions::AggregateFacts;
use super::guards::{GuardRegistry, GuardSpec, InputPort};
use super::state_table::{Row, StateTable, Target, is_workflow_class};

/// Resolution state of one declared input (Foundation §3.6 steps 2-3; D-113).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputState {
    Resolved,
    /// Unavailable, denied or past its deadline, with the port's registered reason.
    Unresolvable(Reason),
    /// Deliberately not resolved because an earlier-registered guard already fails on its
    /// resolved inputs. Never unresolvable.
    Precluded,
}

/// The request's input resolutions, one per declared port of the row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inputs(BTreeMap<InputPort, InputState>);
impl Inputs {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    #[must_use]
    pub fn with(mut self, port: InputPort, state: InputState) -> Self {
        self.0.insert(port, state);
        self
    }
    #[must_use]
    pub fn get(&self, port: InputPort) -> Option<InputState> {
        self.0.get(&port).copied()
    }
    fn ports(&self) -> BTreeSet<InputPort> {
        self.0.keys().copied().collect()
    }
}

/// What a guard sees: the row, the locked aggregate (for create, the proposed initial facts:
/// no aggregate exists), the single transition timestamp and the resolved inputs. The slice
/// predicate closes over its own validated contribution.
pub struct GuardSubject<'a> {
    pub row: &'a Row,
    pub locked: &'a AggregateFacts,
    pub transition_time: OffsetDateTime,
    pub inputs: &'a Inputs,
    /// Row 3 only: the named administrative changes compared with the values stored at the
    /// engine's locked read, unchanged fields dropped (04 §3.6 step 2). The change guard refuses
    /// `administrative-edit-unchanged` when it is empty; empty on every other row.
    pub administrative: &'a [AdminChange],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardVerdict {
    Pass,
    Fail(Reason),
}

/// A slice-supplied predicate bound to one registered guard name.
pub trait GuardPredicate: Send + Sync {
    fn evaluate(&self, subject: &GuardSubject<'_>) -> GuardVerdict;
}
impl<F> GuardPredicate for F
where
    F: Fn(&GuardSubject<'_>) -> GuardVerdict + Send + Sync,
{
    fn evaluate(&self, subject: &GuardSubject<'_>) -> GuardVerdict {
        self(subject)
    }
}

/// The slice's predicates for this request, by registered guard name.
#[derive(Clone, Default)]
pub struct GuardBindings(BTreeMap<&'static str, Arc<dyn GuardPredicate>>);
impl GuardBindings {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    #[must_use]
    pub fn bind(mut self, name: &'static str, predicate: impl GuardPredicate + 'static) -> Self {
        self.0.insert(name, Arc::new(predicate));
        self
    }
    fn names(&self) -> BTreeSet<&'static str> {
        self.0.keys().copied().collect()
    }
}
impl std::fmt::Debug for GuardBindings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

/// Client and prepared draft revisions (OL-4, D-147). Draft writes and submit carry both;
/// every other trigger carries none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftRevisions {
    NotApplicable,
    Draft {
        /// `expected_draft_revision`; absent only on an optional-boundary `draft-mutate`.
        client: Option<i64>,
        /// The revision of the coherent snapshot the prepared inputs were resolved from.
        prepared: i64,
    },
}

/// An engine-owned refusal decided before the slice guards (steps 10-12, 15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineRefusal {
    /// Step 11: no row for `(state, trigger)`.
    NotAdmissible { state: OrderState, trigger: Trigger },
    /// Step 10.1 / 12: names the current version and, for draft operations, revision.
    VersionConflict {
        current_version: i32,
        draft_revision: Option<i64>,
    },
    /// Step 15: resume without a stored pre-hold state.
    ResumeTargetMissing,
}
impl EngineRefusal {
    #[must_use]
    pub fn reason(self) -> Reason {
        match self {
            Self::NotAdmissible { .. } => Reason::NotAdmissible,
            Self::VersionConflict { .. } => Reason::VersionConflict,
            Self::ResumeTargetMissing => Reason::ResumeTargetMissing,
        }
    }
}

/// Outcome of the decision procedure for an owned new execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision<'t> {
    Engine(EngineRefusal),
    /// A registered slice guard refused. `assessment` marks the composite gate guard, whose
    /// refusal carries the complete diagnostics (Foundation *Diagnostic settlement contract*).
    Guard {
        guard: &'static str,
        reason: Reason,
        assessment: bool,
    },
    /// No earlier guard failed and a declared input is unresolvable (step 3.1.2).
    Unevaluable {
        guard: &'static str,
        reason: Reason,
        assessment: bool,
    },
    /// Every check passed: proceed to steps 14-27 for this row.
    Admitted(&'t Row),
}

/// A request/registry contract violation: an infrastructure abort (fail closed).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecisionError {
    #[error("guard `{0}` has no bound predicate")]
    Unbound(&'static str),
    #[error("predicate `{0}` is not registered on this row")]
    Unregistered(&'static str),
    #[error("input {0:?} is not declared by this row")]
    UndeclaredInput(InputPort),
    #[error("declared input {0:?} has no resolution")]
    MissingInput(InputPort),
    #[error("input {0:?} carries an unregistered unevaluable reason")]
    InputReason(InputPort),
    #[error("guard `{0}` reached with a precluded input")]
    PrecludedReached(&'static str),
    #[error("guard `{0}` returned a reason it did not register")]
    GuardReason(&'static str),
    #[error("draft revisions are required exactly for draft-mutate and submit")]
    DraftRevisions,
    #[error("administrative changes belong to the administrative-edit row exactly")]
    Administrative,
}

/// Whether the trigger carries client/prepared draft revisions (OL-4).
#[must_use]
pub fn uses_draft_revisions(trigger: Trigger) -> bool {
    matches!(trigger, Trigger::DraftMutate | Trigger::Submit)
}

/// Which tenant axes a trigger may propose to change (08 §4.3; DESIGN 02 §2.2, §4.3): a commercial
/// draft edit may move the resource and payer axes, an amendment only the payer (the resource is
/// commercial-frozen from submit), and no other row changes an axis. The seller never changes
/// after create (D-119). A request outside this rule is an integration defect, never a proposal.
#[must_use]
pub fn axis_change_admitted(trigger: Trigger, resource: bool, payer: bool) -> bool {
    match trigger {
        Trigger::DraftMutate => resource || payer,
        Trigger::Amendment => payer && !resource,
        _ => false,
    }
}

/// Steps 10-12 in their total order. Workflow-class: version, then admissibility; every other
/// trigger: admissibility, then version and draft revisions (D-110, D-147).
///
/// # Errors
/// [`DecisionError::DraftRevisions`] when revisions are supplied to, or missing from, a trigger.
pub fn admit<'t>(
    table: &'t StateTable,
    trigger: Trigger,
    locked: &AggregateFacts,
    expected_version: i32,
    revisions: DraftRevisions,
) -> Result<Result<&'t Row, EngineRefusal>, DecisionError> {
    if uses_draft_revisions(trigger) != matches!(revisions, DraftRevisions::Draft { .. }) {
        return Err(DecisionError::DraftRevisions);
    }
    let version_conflict = EngineRefusal::VersionConflict {
        current_version: locked.current_version,
        draft_revision: uses_draft_revisions(trigger).then_some(locked.draft_revision),
    };
    if is_workflow_class(trigger) && expected_version != locked.current_version {
        return Ok(Err(version_conflict));
    }
    let Some(row) = table.lookup(locked.state, trigger) else {
        return Ok(Err(EngineRefusal::NotAdmissible {
            state: locked.state,
            trigger,
        }));
    };
    if expected_version != locked.current_version {
        return Ok(Err(version_conflict));
    }
    if let DraftRevisions::Draft { client, prepared } = revisions
        && (client != Some(locked.draft_revision) || prepared != locked.draft_revision)
    {
        return Ok(Err(version_conflict));
    }
    Ok(Ok(row))
}

/// The full post-gate decision: steps 10-12, the guards (steps 3.1.1/3.1.2 and 13) and the
/// step-15 stored-target check, for every row without administrative changes.
///
/// # Errors
/// Binding/input contract violations; the caller aborts the transaction.
#[allow(clippy::too_many_arguments)]
pub fn decide<'t>(
    table: &'t StateTable,
    registry: &GuardRegistry,
    trigger: Trigger,
    locked: &AggregateFacts,
    expected_version: i32,
    revisions: DraftRevisions,
    bindings: &GuardBindings,
    inputs: &Inputs,
    transition_time: OffsetDateTime,
) -> Result<Decision<'t>, DecisionError> {
    decide_with(
        table,
        registry,
        trigger,
        locked,
        expected_version,
        revisions,
        bindings,
        inputs,
        transition_time,
        &[],
    )
}

/// [`decide`] with row 3's administrative changes as reconciled at the engine's locked read.
///
/// # Errors
/// Binding/input contract violations, or administrative changes on another trigger.
#[allow(clippy::too_many_arguments)]
pub fn decide_with<'t>(
    table: &'t StateTable,
    registry: &GuardRegistry,
    trigger: Trigger,
    locked: &AggregateFacts,
    expected_version: i32,
    revisions: DraftRevisions,
    bindings: &GuardBindings,
    inputs: &Inputs,
    transition_time: OffsetDateTime,
    administrative: &[AdminChange],
) -> Result<Decision<'t>, DecisionError> {
    if trigger != Trigger::AdministrativeEdit && !administrative.is_empty() {
        return Err(DecisionError::Administrative);
    }
    let row = match admit(table, trigger, locked, expected_version, revisions)? {
        Ok(row) => row,
        Err(refusal) => return Ok(Decision::Engine(refusal)),
    };
    let guards = registry.row_guards(row.id);
    check_bindings(guards, bindings)?;
    check_inputs(guards, inputs)?;
    let subject = GuardSubject {
        row,
        locked,
        transition_time,
        inputs,
        administrative,
    };
    if let Some(decision) = evaluate(guards, bindings, &subject)? {
        return Ok(decision);
    }
    if row.target == Target::PreHold && locked.pre_hold_state.is_none() {
        return Ok(Decision::Engine(EngineRefusal::ResumeTargetMissing));
    }
    Ok(Decision::Admitted(row))
}

/// The create branch's guard decision (D-105): only the create row's registered guards, over the
/// proposed initial facts (no aggregate exists). `Some((reason, assessment))` refuses.
///
/// # Errors
/// Binding/input contract violations; the caller aborts the transaction.
pub fn decide_create(
    table: &StateTable,
    registry: &GuardRegistry,
    proposed: &AggregateFacts,
    bindings: &GuardBindings,
    inputs: &Inputs,
    transition_time: OffsetDateTime,
) -> Result<Option<(Reason, bool)>, DecisionError> {
    let row = table.create_row();
    let guards = registry.row_guards(row.id);
    check_bindings(guards, bindings)?;
    check_inputs(guards, inputs)?;
    let subject = GuardSubject {
        row,
        locked: proposed,
        transition_time,
        inputs,
        administrative: &[],
    };
    Ok(match evaluate(guards, bindings, &subject)? {
        Some(
            Decision::Guard {
                reason, assessment, ..
            }
            | Decision::Unevaluable {
                reason, assessment, ..
            },
        ) => Some((reason, assessment)),
        Some(Decision::Engine(refusal)) => Some((refusal.reason(), false)),
        Some(Decision::Admitted(_)) | None => None,
    })
}

fn check_bindings(
    guards: &[Arc<GuardSpec>],
    bindings: &GuardBindings,
) -> Result<(), DecisionError> {
    let registered: BTreeSet<&'static str> = guards.iter().map(|g| g.name).collect();
    let bound = bindings.names();
    if let Some(name) = registered.difference(&bound).next() {
        return Err(DecisionError::Unbound(name));
    }
    if let Some(name) = bound.difference(&registered).next() {
        return Err(DecisionError::Unregistered(name));
    }
    Ok(())
}

fn check_inputs(guards: &[Arc<GuardSpec>], inputs: &Inputs) -> Result<(), DecisionError> {
    let declared: BTreeSet<InputPort> = guards
        .iter()
        .flat_map(|g| g.inputs.iter().copied())
        .collect();
    if let Some(port) = inputs.ports().difference(&declared).next() {
        return Err(DecisionError::UndeclaredInput(*port));
    }
    for port in &declared {
        match inputs.get(*port) {
            None => return Err(DecisionError::MissingInput(*port)),
            Some(InputState::Unresolvable(reason))
                if !port.unevaluable_reasons().contains(&reason) =>
            {
                return Err(DecisionError::InputReason(*port));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// Evaluate in registration order. An unresolvable input stops evaluation at its guard, after
/// every earlier guard was evaluated (D-113); a precluded input must never be reached.
fn evaluate(
    guards: &[Arc<GuardSpec>],
    bindings: &GuardBindings,
    subject: &GuardSubject<'_>,
) -> Result<Option<Decision<'static>>, DecisionError> {
    for guard in guards {
        let unresolved = guard
            .inputs
            .iter()
            .find_map(|p| match subject.inputs.get(*p) {
                Some(InputState::Unresolvable(reason)) => Some(reason),
                _ => None,
            });
        if let Some(reason) = unresolved {
            return Ok(Some(Decision::Unevaluable {
                guard: guard.name,
                reason,
                assessment: guard.is_gate(),
            }));
        }
        if guard
            .inputs
            .iter()
            .any(|p| subject.inputs.get(*p) == Some(InputState::Precluded))
        {
            return Err(DecisionError::PrecludedReached(guard.name));
        }
        let predicate = bindings
            .0
            .get(guard.name)
            .ok_or(DecisionError::Unbound(guard.name))?;
        match predicate.evaluate(subject) {
            GuardVerdict::Pass => {}
            GuardVerdict::Fail(reason) if guard.admits_reason(reason) => {
                return Ok(Some(Decision::Guard {
                    guard: guard.name,
                    reason,
                    assessment: guard.is_gate(),
                }));
            }
            GuardVerdict::Fail(_) => return Err(DecisionError::GuardReason(guard.name)),
        }
    }
    Ok(None)
}

#[cfg(test)]
#[path = "transition_tests.rs"]
mod tests;
