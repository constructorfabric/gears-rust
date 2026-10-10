//! Payload-free progress, history and administrative inspection.
use crate::{ActivityId, ExecutionOwner, RunId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Unknown is used for legacy records whose completion time was not persisted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "at", rename_all = "snake_case")]
pub enum RecordedTime {
    Known(DateTime<Utc>),
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Running,
    RetryWait {
        due_at: Option<DateTime<Utc>>,
        error: String,
    },
    Failing {
        error: String,
    },
    Succeeded {
        completed_at: RecordedTime,
    },
    Failed {
        error: String,
        completed_at: RecordedTime,
    },
    Cancelling {
        reason: Option<String>,
    },
    Cancelled {
        reason: Option<String>,
        completed_at: RecordedTime,
    },
    Blocked {
        error: String,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    RetryWait,
    Failing,
    Succeeded,
    Failed,
    Cancelling,
    Cancelled,
    Blocked,
}
impl RunState {
    #[must_use]
    pub fn status(&self) -> RunStatus {
        match self {
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
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded { .. } | Self::Failed { .. } | Self::Cancelled { .. }
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepState {
    Pending,
    Running {
        started_at: RecordedTime,
    },
    RetryWait {
        error: String,
    },
    Succeeded {
        completed_at: RecordedTime,
        /// Absent when legacy metadata cannot establish the checkpoint origin.
        checkpoint_epoch: Option<u64>,
    },
    Failed {
        error: String,
        completed_at: RecordedTime,
    },
    Cancelled {
        completed_at: RecordedTime,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepProgress {
    pub id: ActivityId,
    pub stage: u32,
    pub state: StepState,
    pub attempts: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunProgress {
    pub id: RunId,
    pub definition: String,
    pub owner: ExecutionOwner,
    pub epoch: u64,
    pub cursor: u64,
    pub state: RunState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub steps: Vec<StepProgress>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttemptOutcome {
    Succeeded,
    Failed { error: String },
    Cancelled,
    Interrupted { error: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttemptState {
    Running {
        started_at: DateTime<Utc>,
    },
    Finished {
        started_at: DateTime<Utc>,
        finished_at: DateTime<Utc>,
        outcome: AttemptOutcome,
    },
    /// Incomplete historical metadata; no invented timestamps.
    Legacy {
        started_at: DateTime<Utc>,
        completed_at: RecordedTime,
        outcome: AttemptOutcome,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepAttempt {
    pub number: u32,
    pub epoch: u64,
    pub state: AttemptState,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepHistory {
    pub id: ActivityId,
    pub attempts: Vec<StepAttempt>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionEpoch {
    pub epoch: u64,
    pub state: RunState,
    pub steps: Vec<StepProgress>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunHistory {
    pub epochs: Vec<ExecutionEpoch>,
    pub steps: Vec<StepHistory>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowQuery {
    pub since: DateTime<Utc>,
    pub status: Option<RunStatus>,
    pub limit: u32,
    pub offset: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowPage {
    pub items: Vec<RunProgress>,
    pub total: u64,
}
