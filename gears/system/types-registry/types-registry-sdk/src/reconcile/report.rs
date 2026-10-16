use std::collections::HashMap;
use std::fmt;

use toolkit_canonical_errors::{CanonicalError, Problem};

use super::{ReconcileOutcome, ReconcilePendingCause, ReconcileStop};
use crate::GtsId;
use crate::gts::{OPERATION_RESOURCE_TYPE, OperationResource};
use crate::precondition::RECONCILIATION_INCOMPLETE;

const OUTCOME_PREFIX: &str = "reconciliation.";

/// Outcomes and stop reason decoded from a reconciliation error.
/// Violations carry stop/ID/status; error descriptions contain standard `Problem` JSON.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ReconcileReport {
    /// Why reconciliation stopped.
    pub stop: ReconcileStop,
    /// All desired identifiers, including admitted ones.
    pub outcomes: HashMap<GtsId, ReconcileOutcome>,
}

impl ReconcileReport {
    /// Decode the report; unrelated, malformed or unknown formats return `None`.
    #[must_use]
    pub fn from_canonical(error: &CanonicalError) -> Option<Self> {
        let CanonicalError::FailedPrecondition {
            ctx,
            resource_type,
            resource_name,
            ..
        } = error
        else {
            return None;
        };
        if resource_type.as_deref()? != OPERATION_RESOURCE_TYPE || resource_name.is_some() {
            return None;
        }
        let (header, items) = ctx.violations.split_first()?;
        if header.type_ != RECONCILIATION_INCOMPLETE {
            return None;
        }
        let stop = ReconcileStop::from_wire(&header.subject)?;
        let mut outcomes = HashMap::with_capacity(items.len());
        for item in items {
            let id = GtsId::try_new(&item.subject).ok()?;
            if id.id() != item.subject {
                return None;
            }
            let outcome =
                decode_outcome(item.type_.strip_prefix(OUTCOME_PREFIX)?, &item.description)?;
            if outcomes.insert(id, outcome).is_some() {
                return None;
            }
        }
        if outcomes
            .values()
            .all(|o| matches!(o, ReconcileOutcome::Admitted))
        {
            return None;
        }
        Some(Self { stop, outcomes })
    }

    /// Encode using the existing precondition-violation schema.
    pub(crate) fn into_canonical(self) -> Result<CanonicalError, CanonicalError> {
        let summary = self.to_string();
        let mut builder = OperationResource::failed_precondition().with_precondition_violation(
            self.stop.as_str(),
            summary,
            RECONCILIATION_INCOMPLETE,
        );
        let mut items: Vec<_> = self.outcomes.iter().collect();
        items.sort_unstable_by(|(left, _), (right, _)| left.id().cmp(right.id()));
        for (id, outcome) in items {
            let (code, error) = outcome_parts(outcome);
            let description = error.map(encode_error).transpose()?.unwrap_or_default();
            builder = builder.with_precondition_violation(
                id.id(),
                description,
                format!("{OUTCOME_PREFIX}{code}"),
            );
        }
        Ok(builder.create())
    }
}

impl fmt::Display for ReconcileReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (rejected, pending) = self.outcomes.values().fold((0, 0), |(r, p), o| match o {
            ReconcileOutcome::Admitted => (r, p),
            ReconcileOutcome::Rejected(_) => (r + 1, p),
            ReconcileOutcome::Pending(_) => (r, p + 1),
        });
        write!(
            f,
            "reconciliation stopped ({}) with {rejected} rejected and {pending} pending of {} identifiers",
            self.stop.as_str(),
            self.outcomes.len()
        )
    }
}

fn outcome_parts(outcome: &ReconcileOutcome) -> (&'static str, Option<&CanonicalError>) {
    match outcome {
        ReconcileOutcome::Admitted => ("admitted", None),
        ReconcileOutcome::Rejected(error) => ("rejected", Some(error)),
        ReconcileOutcome::Pending(cause) => match cause {
            ReconcilePendingCause::Dependency(error) => ("pending.dependency", Some(error)),
            ReconcilePendingCause::Conflict(error) => ("pending.conflict", Some(error)),
            ReconcilePendingCause::Unavailable(error) => ("pending.unavailable", Some(error)),
            ReconcilePendingCause::Refused(error) => ("pending.refused", Some(error)),
            ReconcilePendingCause::Skipped => ("pending.skipped", None),
        },
    }
}

fn encode_error(error: &CanonicalError) -> Result<String, CanonicalError> {
    // Use the transport codec so private diagnostics stay redacted.
    Problem::from_error(error)
        .and_then(|problem| serde_json::to_string(&problem))
        .map_err(|_| CanonicalError::internal("cannot encode reconciliation error").create())
}

fn decode_outcome(code: &str, description: &str) -> Option<ReconcileOutcome> {
    let error = || {
        let problem = serde_json::from_str::<Problem>(description).ok()?;
        CanonicalError::try_from(problem).ok()
    };
    Some(match code {
        "admitted" if description.is_empty() => ReconcileOutcome::Admitted,
        "pending.skipped" if description.is_empty() => {
            ReconcileOutcome::Pending(ReconcilePendingCause::Skipped)
        }
        "rejected" => ReconcileOutcome::Rejected(error()?),
        "pending.dependency" => {
            ReconcileOutcome::Pending(ReconcilePendingCause::Dependency(error()?))
        }
        "pending.conflict" => ReconcileOutcome::Pending(ReconcilePendingCause::Conflict(error()?)),
        "pending.unavailable" => {
            ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(error()?))
        }
        "pending.refused" => ReconcileOutcome::Pending(ReconcilePendingCause::Refused(error()?)),
        _ => return None,
    })
}
