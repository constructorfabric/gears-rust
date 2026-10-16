//! Structured reconciliation events and completion summaries.
//! Log error codes only: messages may contain document content. Full errors are returned to callers.

use toolkit_canonical_errors::{CanonicalError, InvalidArgument};
use uuid::Uuid;

use super::{Outcomes, ReconcileOutcome, ReconcilePendingCause, ReconcileStop};
use crate::gts::OPERATION_RESOURCE_TYPE;
use crate::item_failure::{AdmissionFailure, context};

/// An error's machine-readable fields.
pub struct ErrorFields {
    pub category: &'static str,
    /// The admission reason, the violations' reason codes, or the canonical `reason`.
    pub reason: Option<String>,
    /// The request fields an `InvalidArgument` names.
    pub violation_fields: Option<String>,
    pub dependency_id: Option<String>,
    pub dependency_kind: Option<String>,
    pub diagnostic_code: Option<String>,
}

pub fn error_fields(error: &CanonicalError) -> ErrorFields {
    let mut fields = ErrorFields {
        category: category(error),
        reason: None,
        violation_fields: None,
        dependency_id: None,
        dependency_kind: None,
        diagnostic_code: None,
    };
    if let Some(failure) = AdmissionFailure::from_canonical(error) {
        fields.reason = Some(failure.reason.as_wire().to_owned());
        fields.dependency_id = failure.context(context::DEPENDENCY_ID).map(str::to_owned);
        fields.dependency_kind = failure.context(context::DEPENDENCY_KIND).map(str::to_owned);
        fields.diagnostic_code = failure.context(context::DIAGNOSTIC_CODE).map(str::to_owned);
        return fields;
    }
    match error {
        CanonicalError::InvalidArgument {
            ctx: InvalidArgument::FieldViolations { field_violations },
            ..
        } => {
            let join = |pick: fn(&toolkit_canonical_errors::FieldViolation) -> &str| {
                field_violations
                    .iter()
                    .map(pick)
                    .collect::<Vec<_>>()
                    .join(",")
            };
            fields.violation_fields = Some(join(|v| v.field.as_str()));
            fields.reason = Some(join(|v| v.reason.as_str()));
        }
        // Policy and parent refusals: the violations' type codes, never their descriptions.
        CanonicalError::FailedPrecondition { ctx, .. } if !ctx.violations.is_empty() => {
            fields.reason = Some(
                ctx.violations
                    .iter()
                    .map(|v| v.type_.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        CanonicalError::Aborted { ctx, .. } => fields.reason = Some(ctx.reason.clone()),
        CanonicalError::PermissionDenied { ctx, .. } => fields.reason = Some(ctx.reason.clone()),
        CanonicalError::Unauthenticated { ctx, .. } => fields.reason.clone_from(&ctx.reason),
        _ => {}
    }
    fields
}

/// The category's stable short name.
pub fn category(error: &CanonicalError) -> &'static str {
    match error {
        CanonicalError::Cancelled { .. } => "cancelled",
        CanonicalError::Unknown { .. } => "unknown",
        CanonicalError::InvalidArgument { .. } => "invalid_argument",
        CanonicalError::DeadlineExceeded { .. } => "deadline_exceeded",
        CanonicalError::NotFound { .. } => "not_found",
        CanonicalError::AlreadyExists { .. } => "already_exists",
        CanonicalError::PermissionDenied { .. } => "permission_denied",
        CanonicalError::ResourceExhausted { .. } => "resource_exhausted",
        CanonicalError::FailedPrecondition { .. } => "failed_precondition",
        CanonicalError::Aborted { .. } => "aborted",
        CanonicalError::OutOfRange { .. } => "out_of_range",
        CanonicalError::Unimplemented { .. } => "unimplemented",
        CanonicalError::Internal { .. } => "internal",
        CanonicalError::ServiceUnavailable { .. } => "service_unavailable",
        CanonicalError::DataLoss { .. } => "data_loss",
        CanonicalError::Unauthenticated { .. } => "unauthenticated",
        _ => "other",
    }
}

/// The operation ID named by a scoped error.
pub fn operation_id_of(error: &CanonicalError) -> Option<Uuid> {
    if error.resource_type() != Some(OPERATION_RESOURCE_TYPE) {
        return None;
    }
    Uuid::parse_str(error.resource_name()?).ok()
}

/// Every transport attempt of a payload failed; its candidates stay pending.
pub fn transport_exhausted(attempts: u32, fields: &ErrorFields) {
    tracing::warn!(
        log_kind = "transport_exhausted",
        attempts,
        category = fields.category,
        reason = fields.reason.as_deref(),
        "a submission failed every transport attempt; it stays pending"
    );
}

/// A transport failure retried under the same key.
pub fn transport_retry(fields: &ErrorFields, backoff_ms: u64) {
    tracing::debug!(
        log_kind = "transport_retry",
        category = fields.category,
        reason = fields.reason.as_deref(),
        backoff_ms,
        "retrying a submission under its key"
    );
}

/// Log a validation stop naming one candidate.
pub fn candidate_refused(gts_id: &gts::GtsId, remaining: usize, fields: &ErrorFields) {
    tracing::warn!(
        log_kind = "candidate_refused",
        gts_id = %gts_id,
        remaining,
        category = fields.category,
        reason = fields.reason.as_deref(),
        violation_fields = fields.violation_fields.as_deref(),
        "registry refused a candidate's input; nothing more is submitted"
    );
}

/// Log a validation stop for the whole request.
pub fn batch_refused(fields: &ErrorFields) {
    tracing::warn!(
        log_kind = "batch_refused",
        category = fields.category,
        reason = fields.reason.as_deref(),
        violation_fields = fields.violation_fields.as_deref(),
        "registry refused the request; its candidates stay pending, nothing more is submitted"
    );
}

/// Why a call stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    UpToDate,
    Completed,
    PassesExhausted,
    Deadline,
    ReadFailed,
    InvalidInput,
    Cancelled,
    InvalidDeadline,
}

impl From<ReconcileStop> for StopReason {
    fn from(stop: ReconcileStop) -> Self {
        match stop {
            ReconcileStop::Completed => Self::Completed,
            ReconcileStop::PassesExhausted => Self::PassesExhausted,
            ReconcileStop::Deadline => Self::Deadline,
            ReconcileStop::ReadFailed => Self::ReadFailed,
            ReconcileStop::InvalidInput => Self::InvalidInput,
        }
    }
}

impl StopReason {
    const fn label(self) -> &'static str {
        match self {
            Self::UpToDate => "up_to_date",
            Self::Completed => "completed",
            Self::PassesExhausted => "passes_exhausted",
            Self::Deadline => "deadline",
            Self::ReadFailed => "read_failed",
            Self::InvalidInput => "invalid_input",
            Self::Cancelled => "cancelled",
            Self::InvalidDeadline => "invalid_deadline",
        }
    }
}

/// Reports a call once, including progress observed before cancellation or drop.
pub struct Run {
    started: tokio::time::Instant,
    /// Distinct identifiers desired: duplicates in the input count once.
    desired: usize,
    pub outcomes: Outcomes,
    pub passes: u32,
    stop: Option<StopReason>,
    reported: bool,
}

impl Run {
    pub fn new(started: tokio::time::Instant, desired: usize, outcomes: Outcomes) -> Self {
        Self {
            started,
            desired,
            outcomes,
            passes: 0,
            stop: None,
            reported: false,
        }
    }

    /// The call is returning early; `Drop` reports it.
    pub fn stop(&mut self, reason: StopReason) {
        self.stop = Some(reason);
    }

    /// Report a normal return — every identifier not admitted, then the summary — and hand
    /// the outcomes over.
    pub fn finish(&mut self, reason: StopReason) -> Outcomes {
        self.stop = Some(reason);
        for (id, outcome) in self.outcomes.values() {
            unsettled(id, outcome);
        }
        self.summarize();
        std::mem::take(&mut self.outcomes)
    }

    fn summarize(&mut self) {
        self.reported = true;
        let (admitted, rejected, pending) = self.tally();
        let undecided = self.desired.saturating_sub(self.outcomes.len());
        let stop = self.stop_label();
        let elapsed_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let quiet = match self.stop {
            Some(StopReason::UpToDate | StopReason::Cancelled) => true,
            Some(StopReason::Completed) => rejected == 0 && pending == 0 && undecided == 0,
            _ => false,
        };
        macro_rules! summary {
            ($level:ident) => {
                tracing::$level!(
                    log_kind = "reconcile_finished",
                    stop,
                    elapsed_ms,
                    desired = self.desired,
                    admitted,
                    rejected,
                    pending,
                    undecided,
                    passes = self.passes,
                    "reconciliation finished"
                )
            };
        }
        if quiet {
            summary!(info);
        } else {
            summary!(warn);
        }
    }
}

impl Run {
    /// Admitted, rejected and pending outcomes so far.
    fn tally(&self) -> (usize, usize, usize) {
        self.outcomes
            .values()
            .fold((0, 0, 0), |(a, r, p), (_, outcome)| match outcome {
                ReconcileOutcome::Admitted => (a + 1, r, p),
                ReconcileOutcome::Rejected(_) => (a, r + 1, p),
                ReconcileOutcome::Pending(_) => (a, r, p + 1),
            })
    }

    fn stop_label(&self) -> &'static str {
        match self.stop {
            Some(reason) => reason.label(),
            None if std::thread::panicking() => "panicked",
            None => "dropped",
        }
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if !self.reported {
            self.summarize();
        }
    }
}

/// One identifier's outcome other than admission, as its codes.
fn unsettled(id: &gts::GtsId, outcome: &ReconcileOutcome) {
    let (outcome, cause, error) = match outcome {
        ReconcileOutcome::Admitted => return,
        ReconcileOutcome::Rejected(error) => ("rejected", None, Some(error)),
        ReconcileOutcome::Pending(cause) => {
            let (cause, error) = match cause {
                ReconcilePendingCause::Dependency(e) => ("dependency", Some(e)),
                ReconcilePendingCause::Conflict(e) => ("conflict", Some(e)),
                ReconcilePendingCause::Unavailable(e) => ("unavailable", Some(e)),
                ReconcilePendingCause::Refused(e) => ("refused", Some(e)),
                ReconcilePendingCause::Skipped => ("skipped", None),
            };
            ("pending", Some(cause), error)
        }
    };
    let fields = error.map(error_fields);
    let fields = fields.as_ref();
    tracing::warn!(
        log_kind = "entity_unsettled",
        gts_id = %id,
        outcome,
        cause,
        category = fields.map(|f| f.category),
        reason = fields.and_then(|f| f.reason.as_deref()),
        violation_fields = fields.and_then(|f| f.violation_fields.as_deref()),
        dependency_id = fields.and_then(|f| f.dependency_id.as_deref()),
        dependency_kind = fields.and_then(|f| f.dependency_kind.as_deref()),
        diagnostic_code = fields.and_then(|f| f.diagnostic_code.as_deref()),
        "an identifier was not admitted"
    );
}
