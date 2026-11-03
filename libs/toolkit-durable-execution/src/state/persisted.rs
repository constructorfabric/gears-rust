//! Persisted journal representation. Keep its serde layout independent of the SDK.
use crate::{ActivityId, ExecutionOwner, RunId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
/// Lifecycle status of a durable run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Waiting for an attempt that is already due.
    Queued,
    /// At least one activity attempt is in flight.
    Running,
    /// Waiting until `next_attempt_at` before the next attempt.
    RetryWait,
    /// Every stage completed; terminal.
    Succeeded,
    /// Stopped with no further attempt scheduled; terminal.
    Failed,
    /// Cancellation requested while attempts are still in flight.
    Cancelling,
    /// Cancelled with no attempt in flight; terminal.
    Cancelled,
    /// Halted because the registered definition no longer matches the run; not claimable.
    Blocked,
    /// Running compensation activities in reverse order.
    Compensating,
    /// Compensation stopped on a failed compensation activity.
    CompensationFailed,
    /// Compensation finished; terminal.
    Compensated,
}
impl RunStatus {
    /// Whether the status is final: succeeded, failed, cancelled or compensated.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Compensated
        )
    }
}
/// Lifecycle status of a single activity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityStatus {
    /// Not yet attempted.
    Pending,
    /// An attempt is in flight under a lease.
    Running,
    /// The last attempt failed and a retry is scheduled.
    RetryWait,
    /// Completed with a checkpointed result.
    Succeeded,
    /// Failed permanently or exhausted its retry budget.
    Failed,
    /// Cancelled before it completed.
    Cancelled,
}

/// Persisted state of one activity within a run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActivityRun {
    /// Activity id within the run.
    pub id: ActivityId,
    /// Current activity status.
    pub status: ActivityStatus,
    /// Lifetime number of attempts started; never reset.
    pub attempts: u32,
    /// Every attempt started, oldest first.
    #[serde(default)]
    pub attempt_history: Vec<ActivityAttempt>,
    /// Attempts used in the current automatic retry budget. Lifetime attempts
    /// never reset when a user explicitly retries or resumes a run.
    #[serde(default)]
    pub budget_attempts: u32,
    /// Checkpointed JSON result; `None` until the activity succeeds.
    pub result: Option<Value>,
    /// Error code of the last failed attempt; `None` when it has not failed.
    pub error_code: Option<String>,
    /// Start time of the latest attempt; `None` before the first attempt.
    pub started_at: Option<DateTime<Utc>>,
    /// End time of the latest attempt; `None` while running or not started.
    pub finished_at: Option<DateTime<Utc>>,
}
/// Record of a single activity attempt.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActivityAttempt {
    /// 1-based attempt number, counted over the activity lifetime.
    pub number: u32,
    /// Run execution epoch in which the attempt started.
    pub epoch: u64,
    /// Fencing token that guarded the attempt's lease.
    pub fence: u64,
    /// Time the attempt started.
    pub started_at: DateTime<Utc>,
    /// Time the attempt ended; `None` while it is in flight.
    pub finished_at: Option<DateTime<Utc>>,
    /// Outcome of the attempt, or `Running` while in flight.
    pub status: ActivityStatus,
    /// Error code if the attempt failed; otherwise `None`.
    pub error_code: Option<String>,
}
/// Persisted state of a durable run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    /// Unique run id.
    pub id: RunId,
    /// Name of the execution definition the run executes.
    pub definition: String,
    /// Owner the run is scoped to for authorization.
    pub owner: ExecutionOwner,
    /// Current run status.
    pub status: RunStatus,
    /// Per-activity state for the current epoch.
    pub activities: Vec<ActivityRun>,
    /// Time the run was created.
    pub created_at: DateTime<Utc>,
    /// Time of the last state change.
    pub updated_at: DateTime<Utc>,
    /// Time the next attempt is due; `None` when nothing is scheduled.
    pub next_attempt_at: Option<DateTime<Utc>>,
    /// Error code that failed or blocked the run; `None` otherwise.
    pub error_code: Option<String>,
    /// Incremented only by an explicit retry/resume, not automatic retries.
    #[serde(default)]
    pub execution_epoch: u64,
    /// Stop sentence for `execution_epoch` only. A later epoch leaves this empty
    /// so a resumed run is not labeled with the previous stop.
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// Archived snapshots of earlier execution epochs, oldest first.
    #[serde(default)]
    pub previous_executions: Vec<ExecutionSnapshot>,
}

/// Archived state of one execution epoch, taken before an explicit retry or resume.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionSnapshot {
    /// Cause of the epoch's stop or compensation; `None` if it ran forward only.
    pub compensation_cause: Option<super::execution::Cause>,
    /// Run error code at the end of the epoch.
    pub error_code: Option<String>,
    /// Activity instances of the epoch, each with its stage index.
    pub instances: Vec<(crate::ActivityInstanceId, u32)>,
    /// Time the epoch reached a terminal state with no attempt in flight.
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    /// Execution epoch this snapshot archives.
    pub epoch: u64,
    /// Run status at the end of the epoch.
    pub status: RunStatus,
    /// Time of the epoch's last state change.
    pub ended_at: DateTime<Utc>,
    /// Per-activity state at the end of the epoch.
    pub activities: Vec<ActivityRun>,
    /// Reason captured with this epoch's stop. Absent when the epoch ended another way.
    #[serde(default)]
    pub stop_reason: Option<String>,
}
