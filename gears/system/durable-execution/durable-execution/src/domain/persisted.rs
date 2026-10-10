//! Persisted journal representation. Keep its serde layout independent of the SDK.
use chrono::{DateTime, Utc};
use durable_execution_sdk::{ActivityId, ExecutionOwner, RunId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[toolkit_macros::domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    RetryWait,
    Succeeded,
    Failed,
    Cancelling,
    Cancelled,
    Blocked,
}
impl RunStatus {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}
#[toolkit_macros::domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityStatus {
    Pending,
    Running,
    RetryWait,
    Succeeded,
    Failed,
    Cancelled,
}

#[toolkit_macros::domain_model]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActivityRun {
    pub id: ActivityId,
    pub status: ActivityStatus,
    pub attempts: u32,
    #[serde(default)]
    pub attempt_history: Vec<ActivityAttempt>,
    /// Attempts used in the current automatic retry budget. Lifetime attempts
    /// never reset when a user explicitly retries or resumes a run.
    #[serde(default)]
    pub budget_attempts: u32,
    pub result: Option<Value>,
    pub error_code: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}
#[toolkit_macros::domain_model]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActivityAttempt {
    pub number: u32,
    pub epoch: u64,
    pub fence: u64,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: ActivityStatus,
    pub error_code: Option<String>,
}
#[toolkit_macros::domain_model]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub id: RunId,
    pub definition: String,
    pub owner: ExecutionOwner,
    pub status: RunStatus,
    pub activities: Vec<ActivityRun>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub error_code: Option<String>,
    /// Incremented only by an explicit retry/resume, not automatic retries.
    #[serde(default)]
    pub execution_epoch: u64,
    /// Stop sentence for `execution_epoch` only. A later epoch leaves this empty
    /// so a resumed run is not labeled with the previous stop.
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub previous_executions: Vec<ExecutionSnapshot>,
}

#[toolkit_macros::domain_model]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionSnapshot {
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    pub epoch: u64,
    pub status: RunStatus,
    pub ended_at: DateTime<Utc>,
    pub activities: Vec<ActivityRun>,
    /// Reason captured with this epoch's stop. Absent when the epoch ended another way.
    #[serde(default)]
    pub stop_reason: Option<String>,
}
