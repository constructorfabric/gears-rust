//! Payload-free progress, history and administrative inspection.
use crate::{ActivityId, ExecutionOwner, RunId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Completion timing distinguishes recorded values from explicitly unrecorded values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "at", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RecordedTime {
    /// An exact timestamp recorded by the runtime.
    Known(DateTime<Utc>),
    /// No exact timestamp was recorded; never infer one from `updated_at`.
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
/// Payload-free execution phase with safe cause and timing metadata.
#[non_exhaustive]
pub enum RunState {
    /// Reverse execution is in progress.
    Compensating {
        /// Original definitive failure or Abort reason, retained across reverse retries.
        cause: CompensationCause,
    },
    /// A reverse handler exhausted its retry budget.
    CompensationFailed {
        /// Original definitive failure or Abort reason, retained across reverse retries.
        cause: CompensationCause,
        /// Safe machine-readable failure code.
        error: String,
    },
    /// All declared compensators completed.
    Compensated {
        /// Original definitive failure or Abort reason, retained across reverse retries.
        cause: CompensationCause,
        /// Exact recorded completion time, or explicitly unknown.
        completed_at: RecordedTime,
    },
    /// Admitted work waiting for an eligible claim.
    Queued,
    /// At least one invocation has a live persisted claim.
    Running,
    /// Waiting for the next automatic retry deadline.
    RetryWait {
        /// Next persisted automatic retry deadline.
        due_at: Option<DateTime<Utc>>,
        /// Safe machine-readable failure code.
        error: String,
    },
    /// A forward branch failed while siblings are still settling.
    Failing {
        /// Safe machine-readable failure code.
        error: String,
    },
    /// All forward instances completed successfully.
    Succeeded {
        /// Exact recorded completion time, or explicitly unknown.
        completed_at: RecordedTime,
    },
    /// Forward execution ended with a safe failure code.
    Failed {
        /// Safe machine-readable failure code.
        error: String,
        /// Exact recorded completion time, or explicitly unknown.
        completed_at: RecordedTime,
    },
    /// Forward work is stopping and cleanup is not yet confirmed.
    Cancelling {
        /// Optional safe business reason; exclude credentials and personal data.
        reason: Option<String>,
    },
    /// Confirmed cancellation of the invocation or forward execution.
    Cancelled {
        /// Optional safe business reason; exclude credentials and personal data.
        reason: Option<String>,
        /// Exact recorded completion time, or explicitly unknown.
        completed_at: RecordedTime,
    },
    /// Execution cannot proceed because its expected definition is unavailable or incompatible.
    Blocked {
        /// Safe machine-readable failure code.
        error: String,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Filterable run state discriminant without payloads.
#[non_exhaustive]
pub enum RunStatus {
    /// Sequential reversal of confirmed forward effects is active.
    Compensating,
    /// Reversal is stopped at an exhausted compensator; manual Retry may continue it.
    CompensationFailed,
    /// All declared reversals finished; forward continuation is prohibited.
    Compensated,
    /// Admitted work waiting for an eligible claim.
    Queued,
    /// At least one invocation has a live persisted claim.
    Running,
    /// Waiting for the next automatic retry deadline.
    RetryWait,
    /// A forward branch failed while siblings are still settling.
    Failing,
    /// All forward instances completed successfully.
    Succeeded,
    /// Forward execution ended with a safe failure code.
    Failed,
    /// Forward work is stopping and cleanup is not yet confirmed.
    Cancelling,
    /// Confirmed cancellation of the invocation or forward execution.
    Cancelled,
    /// Execution cannot proceed because its expected definition is unavailable or incompatible.
    Blocked,
}
impl RunState {
    #[must_use]
    /// Return the filterable state discriminant.
    pub fn status(&self) -> RunStatus {
        match self {
            Self::Compensating { .. } => RunStatus::Compensating,
            Self::CompensationFailed { .. } => RunStatus::CompensationFailed,
            Self::Compensated { .. } => RunStatus::Compensated,
            Self::Queued => RunStatus::Queued,
            Self::Running => RunStatus::Running,
            Self::RetryWait { .. } => RunStatus::RetryWait,
            Self::Failing { .. } => RunStatus::Failing,
            Self::Succeeded { .. } => RunStatus::Succeeded,
            Self::Failed { .. } => RunStatus::Failed,
            Self::Cancelling { .. } => RunStatus::Cancelling,
            Self::Cancelled { .. } => RunStatus::Cancelled,
            Self::Blocked { .. } => RunStatus::Blocked,
        }
    }
    /// Whether the run waits for an operator: compensation failed or the
    /// definition is unavailable. Explicit Retry or re-registration continues it.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        matches!(self, Self::CompensationFailed { .. } | Self::Blocked { .. })
    }
    /// Whether the run will not progress without a command: it is terminal or
    /// [needs attention](Self::needs_attention).
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.is_terminal() || self.needs_attention()
    }
    #[must_use]
    /// Whether forward execution or rollback has ended in the current phase.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded { .. }
                | Self::Failed { .. }
                | Self::Cancelled { .. }
                | Self::Compensated { .. }
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
/// Payload-free state of one independent durable activity instance.
#[non_exhaustive]
pub enum StepState {
    /// Instance has not yet acquired its next claim.
    Pending,
    /// At least one invocation has a live persisted claim.
    Running {
        /// Exact timestamp when this attempt acquired its claim.
        started_at: RecordedTime,
    },
    /// Waiting for the next automatic retry deadline.
    RetryWait {
        /// Safe machine-readable failure code.
        error: String,
    },
    /// All forward instances completed successfully.
    Succeeded {
        /// Exact recorded completion time, or explicitly unknown.
        completed_at: RecordedTime,
        /// Absent when no successful attempt establishes the checkpoint origin.
        checkpoint_epoch: Option<u64>,
    },
    /// Forward execution ended with a safe failure code.
    Failed {
        /// Safe machine-readable failure code.
        error: String,
        /// Exact recorded completion time, or explicitly unknown.
        completed_at: RecordedTime,
    },
    /// Confirmed cancellation of the invocation or forward execution.
    Cancelled {
        /// Exact recorded completion time, or explicitly unknown.
        completed_at: RecordedTime,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
/// Payload-free instance progress including checkpoint provenance.
pub struct StepProgress {
    /// Template, branch index and phase of this runtime instance.
    pub instance: crate::ActivityInstanceId,
    /// Stable identifier of this run or activity instance.
    pub id: ActivityId,
    /// Zero-based definition stage index, shared by its runtime branches.
    pub stage: u32,
    /// Current phase and safe transition metadata.
    pub state: StepState,
    /// Lifetime attempts or attempt metadata for this instance.
    pub attempts: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
/// Committed payload-free run projection and matching event cursor.
pub struct RunProgress {
    /// Stable identifier of this run or activity instance.
    pub id: RunId,
    /// Expected versioned definition name.
    pub definition: String,
    /// Original submitting identity; access still requires fresh authorization.
    pub owner: ExecutionOwner,
    /// Execution epoch associated with this metadata.
    pub epoch: u64,
    /// Last committed event sequence in this progress projection.
    pub cursor: u64,
    /// Current phase and safe transition metadata.
    pub state: RunState,
    /// Exact admission timestamp.
    pub created_at: DateTime<Utc>,
    /// Exact timestamp of the most recent committed transition.
    pub updated_at: DateTime<Utc>,
    /// Ordered activity instances or templates represented by this view.
    pub steps: Vec<StepProgress>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
/// Safe final outcome of one invocation.
#[non_exhaustive]
pub enum AttemptOutcome {
    /// All forward instances completed successfully.
    Succeeded,
    /// Forward execution ended with a safe failure code.
    Failed {
        /// Safe machine-readable failure code.
        error: String,
    },
    /// Confirmed cancellation of the invocation or forward execution.
    Cancelled,
    /// Unconfirmed handler outcome due to process, lease or shutdown interruption.
    Interrupted {
        /// Safe machine-readable failure code.
        error: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
/// Exact attempt timing and safe outcome without invocation payloads.
#[non_exhaustive]
pub enum AttemptState {
    /// At least one invocation has a live persisted claim.
    Running {
        /// Exact timestamp when this attempt acquired its claim.
        started_at: DateTime<Utc>,
    },
    /// A completed attempt with exact start and finish times.
    Finished {
        /// Exact timestamp when this attempt acquired its claim.
        started_at: DateTime<Utc>,
        /// Exact timestamp when the attempt completed.
        finished_at: DateTime<Utc>,
        /// Safe attempt outcome, without handler output.
        outcome: AttemptOutcome,
    },
    /// Incomplete historical metadata; no invented timestamps.
    Incomplete {
        /// Exact timestamp when this attempt acquired its claim.
        started_at: DateTime<Utc>,
        /// Exact recorded completion time, or explicitly unknown.
        completed_at: RecordedTime,
        /// Safe attempt outcome, without handler output.
        outcome: AttemptOutcome,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
/// Lifetime invocation number and its epoch/timing metadata.
pub struct StepAttempt {
    /// Lifetime attempt number, independent of explicit Retry epochs.
    pub number: u32,
    /// Execution epoch associated with this metadata.
    pub epoch: u64,
    /// Current phase and safe transition metadata.
    pub state: AttemptState,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
/// Payload-free attempt history for one durable instance.
pub struct StepHistory {
    /// Stable identifier of this run or activity instance.
    pub id: ActivityId,
    /// Lifetime attempts or attempt metadata for this instance.
    pub attempts: Vec<StepAttempt>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
/// Archived payload-free snapshot before explicit continuation.
pub struct ExecutionEpoch {
    /// Execution epoch associated with this metadata.
    pub epoch: u64,
    /// Current phase and safe transition metadata.
    pub state: RunState,
    /// Ordered activity instances or templates represented by this view.
    pub steps: Vec<StepProgress>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
/// Payload-free execution epochs and instance attempt histories.
pub struct RunHistory {
    /// Archived states preceding explicit Retry or Resume.
    pub epochs: Vec<ExecutionEpoch>,
    /// Ordered activity instances or templates represented by this view.
    pub steps: Vec<StepHistory>,
}
/// Scoped paginated progress query, evaluated without hydrating payloads.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WorkflowQuery {
    /// Inclusive admission timestamp lower bound.
    pub since: DateTime<Utc>,
    /// Only runs in this state.
    pub status: Option<RunStatus>,
    /// Requested page size, clamped to 1..=100.
    pub limit: u32,
    /// Number of matching rows to skip.
    pub offset: u32,
}
impl WorkflowQuery {
    /// First page of up to 100 runs admitted at or after `since`, in any state.
    #[must_use]
    pub fn new(since: DateTime<Utc>) -> Self {
        Self {
            since,
            status: None,
            limit: 100,
            offset: 0,
        }
    }
    /// Only runs in `status`.
    #[must_use]
    pub fn status(mut self, status: RunStatus) -> Self {
        self.status = Some(status);
        self
    }
    /// Page size, clamped to 1..=100 by the service.
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = limit;
        self
    }
    /// Number of matching runs to skip.
    #[must_use]
    pub fn offset(mut self, offset: u32) -> Self {
        self.offset = offset;
        self
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
/// One ordered page of scoped run projections and total matching count.
pub struct WorkflowPage {
    /// Ordered progress projections in the selected page.
    pub items: Vec<RunProgress>,
    /// Total matching runs before applying pagination.
    pub total: u64,
}

/// Original reason that triggered reversal, independent of a compensator error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CompensationCause {
    /// Original terminal forward failure that started reversal.
    Failure {
        /// Safe machine-readable failure code.
        error: String,
    },
    /// Explicit epoch-checked termination that started reversal.
    Abort {
        /// Optional safe business reason; exclude credentials and personal data.
        reason: Option<String>,
    },
}
