//! Worker calls: claim, batch heartbeat and the lease-fenced execution calls.

use chrono::{DateTime, Utc};
use gts::{GtsInstanceId, GtsTypeId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::{TaskState, WorkerResultCode};
use super::task::{CancelRequestInfo, Task, TaskError};
use super::wait::WaitResult;

/// JSON object merged into a task's `context` as an RFC 7386 JSON Merge Patch.
pub type Checkpoint = serde_json::Map<String, serde_json::Value>;

// ---------------------------------------------------------------------------
// Claim
// ---------------------------------------------------------------------------

/// Claim tasks (long-poll). A claim that takes a task opens or renews the
/// worker session; a `204` or `429` touches no session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ClaimRequest {
    /// Worker process identity within its subject (`[A-Za-z0-9._:-]{1,128}`).
    pub worker_id: String,
    /// The worker's live session to reuse; `None`, a session that is no longer
    /// live, or one stored with another `worker_id` opens a new one (only when
    /// the claim takes a task).
    #[serde(default)]
    pub session_id: Option<Uuid>,
    /// Queues of one task domain (`CROSS_DOMAIN`); at most `claim_max_queues`
    /// (16) distinct queues (`TOO_MANY_QUEUES`); missing or empty →
    /// `REQUIRED`. A `dead_letter` queue → `DEAD_LETTER_QUEUE`; an
    /// unregistered one → `404`.
    #[schema(value_type = Vec<String>)]
    pub queues: Vec<GtsInstanceId>,
    /// At most `claim_max_batch` (32); larger → `400` `OUT_OF_RANGE` (not
    /// clamped). Default 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tasks: Option<u32>,
    /// Long-poll wait in seconds, clamped to `claim_max_wait_secs` (50).
    /// Default 0: one attempt without waiting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Task domain of the claim; required only when a named queue is in two
    /// domains (p3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

impl ClaimRequest {
    /// Build a claim of one task without waiting.
    #[must_use]
    pub fn new(worker_id: impl Into<String>, queues: Vec<GtsInstanceId>) -> Self {
        Self {
            worker_id: worker_id.into(),
            session_id: None,
            queues,
            max_tasks: None,
            timeout_secs: None,
            domain: None,
        }
    }
}

/// Holder-bound lease of one claimed task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[allow(
    clippy::struct_field_names,
    reason = "field names are the wire names of openapi.yaml"
)]
#[non_exhaustive]
pub struct Lease {
    /// Fencing token; appears only in claim/accept responses and the holder's requests.
    pub lease_token: Uuid,
    /// Lease epoch (from 1).
    pub lease_epoch: u64,
    /// Attempt number (from 1).
    pub attempt: u32,
    /// Session lease length after this claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_secs: Option<u64>,
    /// Task domain of this task; worker calls go to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

impl Lease {
    /// Build a lease.
    #[must_use]
    pub const fn new(lease_token: Uuid, lease_epoch: u64, attempt: u32) -> Self {
        Self {
            lease_token,
            lease_epoch,
            attempt,
            lease_secs: None,
            domain: None,
        }
    }
}

/// How the task's creator authority was established.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthorityMode {
    /// Creator checks ran at creation.
    CreatorVerified,
    /// The worker's own authority applies.
    Worker,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// Verdict of one creator check.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthorityVerdict {
    /// The check passed.
    Verified,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// One creator check that ran at creation.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[non_exhaustive]
pub struct AuthorityCheck {
    /// `resource_refs` or an RFC 6901 pointer into the input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Checked resource type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub resource_type: Option<GtsTypeId>,
    /// Checked action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<AuthorityVerdict>,
}

/// Creator authority recorded at creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct Authority {
    /// Authority mode.
    pub mode: AuthorityMode,
    /// The creator checks that ran and their verdicts.
    #[serde(default)]
    pub checked: Vec<AuthorityCheck>,
    /// When the checks ran.
    #[serde(default)]
    pub checked_at: Option<DateTime<Utc>>,
}

impl Authority {
    /// Build an authority record.
    #[must_use]
    pub const fn new(mode: AuthorityMode) -> Self {
        Self {
            mode,
            checked: Vec::new(),
            checked_at: None,
        }
    }
}

/// One claimed task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct Claim {
    /// The task.
    pub task: Task,
    /// Its fresh lease.
    pub lease: Lease,
    /// Creator authority.
    pub authority: Authority,
    /// The wait result when the task resumes from a wait; `None` for a fresh
    /// claim. Returned on every claim while the task's current wait group is a
    /// resolved parked group, so a lost claim response does not lose it.
    #[serde(default)]
    pub resumed: Option<WaitResult>,
}

impl Claim {
    /// Build a fresh claim.
    #[must_use]
    pub const fn new(task: Task, lease: Lease, authority: Authority) -> Self {
        Self {
            task,
            lease,
            authority,
            resumed: None,
        }
    }
}

/// At least one task claimed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct ClaimResponse {
    /// The worker session.
    pub session_id: Uuid,
    /// Task domain of the session; `default` on a single-domain deployment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// `max(1, floor(session lease_secs / 3))`.
    pub heartbeat_interval_secs: u64,
    /// At least one claim.
    pub claims: Vec<Claim>,
}

impl ClaimResponse {
    /// Build a claim response.
    #[must_use]
    pub const fn new(session_id: Uuid, heartbeat_interval_secs: u64, claims: Vec<Claim>) -> Self {
        Self {
            session_id,
            domain: None,
            heartbeat_interval_secs,
            claims,
        }
    }
}

// ---------------------------------------------------------------------------
// Heartbeat
// ---------------------------------------------------------------------------

/// One lease checked by a heartbeat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct HeartbeatLease {
    /// The task.
    pub task_id: Uuid,
    /// Its lease token.
    pub lease_token: Uuid,
}

impl HeartbeatLease {
    /// Build a heartbeat lease entry.
    #[must_use]
    pub const fn new(task_id: Uuid, lease_token: Uuid) -> Self {
        Self {
            task_id,
            lease_token,
        }
    }
}

/// Renew the worker session and check every listed lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct HeartbeatRequest {
    /// The worker session.
    pub session_id: Uuid,
    /// At most `heartbeat_batch_max` (default 500); more → `400` `TOO_MANY` on
    /// `/leases` and the session is not renewed.
    pub leases: Vec<HeartbeatLease>,
}

impl HeartbeatRequest {
    /// Build a heartbeat.
    #[must_use]
    pub const fn new(session_id: Uuid, leases: Vec<HeartbeatLease>) -> Self {
        Self { session_id, leases }
    }
}

/// Per-lease heartbeat outcome.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HeartbeatOutcome {
    /// The lease is held.
    Ok,
    /// The lease is gone; stop working on the task.
    LeaseLost,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// Heartbeat result for one lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct HeartbeatResult {
    /// The task.
    pub task_id: Uuid,
    /// Outcome.
    pub outcome: HeartbeatOutcome,
    /// Task state; present when `outcome` is `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<TaskState>,
    /// Present when `outcome` is `ok`; how the worker learns of a cancel request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_requested: Option<bool>,
    /// Present when `outcome` is `ok` and a cancel is pending: its mode, for
    /// `graceful` its `grace_until`, and `superseded_by` when a supersession
    /// caused it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_request: Option<CancelRequestInfo>,
}

impl HeartbeatResult {
    /// The lease is lost.
    #[must_use]
    pub const fn lease_lost(task_id: Uuid) -> Self {
        Self {
            task_id,
            outcome: HeartbeatOutcome::LeaseLost,
            state: None,
            cancel_requested: None,
            cancel_request: None,
        }
    }

    /// The lease is held.
    #[must_use]
    pub const fn ok(task_id: Uuid, state: TaskState, cancel_requested: bool) -> Self {
        Self {
            task_id,
            outcome: HeartbeatOutcome::Ok,
            state: Some(state),
            cancel_requested: Some(cancel_requested),
            cancel_request: None,
        }
    }
}

/// One result per listed lease, in request order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct HeartbeatResponse {
    /// The worker session.
    pub session_id: Uuid,
    /// Heartbeat interval.
    pub heartbeat_interval_secs: u64,
    /// Per-lease results.
    pub results: Vec<HeartbeatResult>,
}

impl HeartbeatResponse {
    /// Build a heartbeat response.
    #[must_use]
    pub const fn new(
        session_id: Uuid,
        heartbeat_interval_secs: u64,
        results: Vec<HeartbeatResult>,
    ) -> Self {
        Self {
            session_id,
            heartbeat_interval_secs,
            results,
        }
    }
}

// ---------------------------------------------------------------------------
// Lease-fenced calls
// ---------------------------------------------------------------------------

/// A body carrying only the lease token (start).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct LeaseTokenRequest {
    /// The caller's lease token.
    pub lease_token: Uuid,
}

impl LeaseTokenRequest {
    /// Build the request.
    #[must_use]
    pub const fn new(lease_token: Uuid) -> Self {
        Self { lease_token }
    }
}

/// Report progress; buffered and saved at the next flush.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ProgressRequest {
    /// The caller's lease token.
    pub lease_token: Uuid,
    /// Percent done, 0–100.
    pub percent: u8,
    /// Milestone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone: Option<String>,
    /// Units done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_units: Option<u64>,
    /// Units in total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_units: Option<u64>,
    /// Current stage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_stage: Option<String>,
    /// Estimated completion time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta: Option<DateTime<Utc>>,
}

impl ProgressRequest {
    /// Build a progress report.
    #[must_use]
    pub const fn new(lease_token: Uuid, percent: u8) -> Self {
        Self {
            lease_token,
            percent,
            milestone: None,
            completed_units: None,
            total_units: None,
            current_stage: None,
            eta: None,
        }
    }
}

/// Merge a checkpoint into the task `context` (p3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CheckpointRequest {
    /// The caller's lease token.
    pub lease_token: Uuid,
    /// Merged into `context` as an RFC 7386 JSON Merge Patch: nested objects
    /// merge, `null` deletes a key, arrays are replaced whole.
    #[schema(value_type = Object)]
    pub checkpoint: Checkpoint,
}

impl CheckpointRequest {
    /// Build a checkpoint.
    #[must_use]
    pub const fn new(lease_token: Uuid, checkpoint: Checkpoint) -> Self {
        Self {
            lease_token,
            checkpoint,
        }
    }
}

/// The merged context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct CheckpointResponse {
    /// The merged context.
    #[schema(value_type = Object)]
    pub context: Checkpoint,
}

impl CheckpointResponse {
    /// Build a checkpoint response.
    #[must_use]
    pub const fn new(context: Checkpoint) -> Self {
        Self { context }
    }
}

/// Complete a task (success, warning, error or cancelled). The body is flat; a
/// failure is this call with `result_code: error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CompleteRequest {
    /// The caller's lease token.
    pub lease_token: Uuid,
    /// Reported result.
    pub result_code: WorkerResultCode,
    /// Output, validated against the type's output schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    /// Error of an `error` result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TaskError>,
    /// Warnings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<TaskError>,
}

impl CompleteRequest {
    /// A successful completion with an optional output.
    #[must_use]
    pub const fn success(lease_token: Uuid, output: Option<serde_json::Value>) -> Self {
        Self {
            lease_token,
            result_code: WorkerResultCode::Success,
            output,
            error: None,
            warnings: Vec::new(),
        }
    }

    /// A failed completion.
    #[must_use]
    pub const fn error(lease_token: Uuid, error: TaskError) -> Self {
        Self {
            lease_token,
            result_code: WorkerResultCode::Error,
            output: None,
            error: Some(error),
            warnings: Vec::new(),
        }
    }
}

/// Give the task back without failing it, from `claimed` or `running`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ReleaseRequest {
    /// The caller's lease token.
    pub lease_token: Uuid,
    /// Release to `scheduled` after this delay, 0 to `release_max_delay_secs`
    /// (86,400); `0` (the server default) is an immediate release, outside
    /// the range → `400` `OUT_OF_RANGE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_secs: Option<u64>,
    /// Merged into `context` first, as by checkpoint (RFC 7386).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub checkpoint: Option<Checkpoint>,
}

impl ReleaseRequest {
    /// Release at once.
    #[must_use]
    pub const fn new(lease_token: Uuid) -> Self {
        Self {
            lease_token,
            delay_secs: None,
            checkpoint: None,
        }
    }
}

/// Accept a reassigned task (p3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct AcceptRequest {
    /// Worker identity named by the reassignment.
    pub worker_id: String,
    /// Session to reuse.
    #[serde(default)]
    pub session_id: Option<Uuid>,
}

impl AcceptRequest {
    /// Build an accept request.
    #[must_use]
    pub fn new(worker_id: impl Into<String>) -> Self {
        Self {
            worker_id: worker_id.into(),
            session_id: None,
        }
    }
}

/// Session, task and new lease — one claim entry plus its session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct AcceptResponse {
    /// The worker session.
    pub session_id: Uuid,
    /// Task domain of the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// Heartbeat interval.
    pub heartbeat_interval_secs: u64,
    /// The task.
    pub task: Task,
    /// Its new lease.
    pub lease: Lease,
    /// Creator authority.
    pub authority: Authority,
}

impl AcceptResponse {
    /// Build an accept response.
    #[must_use]
    pub const fn new(
        session_id: Uuid,
        heartbeat_interval_secs: u64,
        task: Task,
        lease: Lease,
        authority: Authority,
    ) -> Self {
        Self {
            session_id,
            domain: None,
            heartbeat_interval_secs,
            task,
            lease,
            authority,
        }
    }
}
