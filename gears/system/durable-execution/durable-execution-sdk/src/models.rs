use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub Uuid);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActivityId(pub String);

/// Stable identity only: never persist a bearer token or an authorization decision.
/// Access must be rechecked when performing domain work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionOwner {
    pub tenant_id: Uuid,
    pub subject_id: Uuid,
}

/// The epoch observed by the caller makes delayed or duplicated commands safe:
/// a retry for an earlier execution must never restart a newer failure.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ContinueOptions {
    pub expected_epoch: u64,
}

/// Explicit idempotency applies to one logical request. Coalescing merges
/// refreshes while work is active; these are distinct mechanisms.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StartOptions {
    #[serde(default)]
    pub expected_fingerprint: Option<String>,
    pub idempotency_key: Option<String>,
    pub coalescing_key: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartResult {
    pub run_id: RunId,
    /// Coalescing generation associated with this request. Requests merged into
    /// the same queued run or parked successor share its generation.
    pub requested_generation: u64,
    pub coalesced: bool,
}

/// Progress metadata only: excludes execution inputs, checkpoint payloads and credentials.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunEvent {
    pub sequence: u64,
    pub run_id: RunId,
    pub activity_id: Option<ActivityId>,
    pub kind: EventKind,
    pub epoch: u64,
    pub attempt: Option<u32>,
    pub at: DateTime<Utc>,
    pub error_code: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Queued,
    Started,
    Completed,
    RetryScheduled,
    Failed,
    StopRequested,
    Stopped,
    Resumed,
    RetryRequested,
    Blocked,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CancelOptions {
    pub expected_epoch: u64,
    pub reason: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CancelResult {
    Requested,
    Unchanged,
}
