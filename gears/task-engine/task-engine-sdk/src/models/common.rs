//! Enums and small shared types used across the Task Engine models.
//!
//! **Open enums.** The API treats every enum as an open set: a newer engine may
//! send a value this SDK does not know. Every enum the engine sends therefore
//! has an `Unknown` variant (`#[serde(other)]`) that absorbs such a value
//! instead of failing the whole response; show it as "Unknown". Enums a client
//! only *sends* (request-only, like [`WorkerResultCode`]) are closed.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Task state. Non-terminal: `scheduled`, `queued`, `blocked`, `claimed`,
/// `running`, `waiting`; terminal: `completed`, `failed`, `cancelled`,
/// `dead_lettered`.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaskState {
    /// Waits for `run_after` (a delayed create, retry backoff or release delay).
    Scheduled,
    /// Eligible for claim.
    Queued,
    /// Waits for unmet predecessors (p2).
    Blocked,
    /// Claimed by a worker, not started.
    Claimed,
    /// Started by its worker.
    Running,
    /// Parked on a wait group.
    Waiting,
    /// Terminal: finished with `success` or `warning`.
    Completed,
    /// Terminal: finished with `error` or `timed_out`.
    Failed,
    /// Terminal: cancelled.
    Cancelled,
    /// Terminal: moved to a dead-letter queue.
    DeadLettered,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

impl TaskState {
    /// Whether the state is terminal. `Unknown` is treated as non-terminal.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::DeadLettered
        )
    }
}

/// Task result code. `completed`: `success`, `warning`; `failed` /
/// `dead_lettered`: `error`, `timed_out`; `cancelled`: `cancelled`,
/// `forcibly_cancelled`, `superseded`, `dependency_failed`. `timed_out`,
/// `forcibly_cancelled`, `superseded` and `dependency_failed` are set only by
/// the engine.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ResultCode {
    /// Succeeded.
    Success,
    /// Succeeded with warnings.
    Warning,
    /// Failed.
    Error,
    /// Cancelled cooperatively.
    Cancelled,
    /// A timeout ended the task (engine only).
    TimedOut,
    /// Cancelled with `forced` (engine only).
    ForciblyCancelled,
    /// Replaced by a newer task (engine only, p2).
    Superseded,
    /// A predecessor failed (engine only, p2).
    DependencyFailed,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// Result codes a worker may report on completion. `cancelled` only after a
/// cancel request; anything else is refused `400`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkerResultCode {
    /// Succeeded.
    Success,
    /// Succeeded with warnings.
    Warning,
    /// Failed (retried when retryable and budget is left).
    Error,
    /// Cancelled after a cancel request.
    Cancelled,
}

/// Urgency class. `critical` is served before `normal` and needs the
/// `expedite` action.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Urgency {
    /// The default class.
    #[default]
    Normal,
    /// Served first; needs `expedite`.
    Critical,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// Cancellation mode.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CancelMode {
    /// Ask the worker to stop (learned on its next heartbeat).
    #[default]
    Requested,
    /// Ask the worker to stop; force after the grace timer.
    Graceful,
    /// End the task at once.
    Forced,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// How predecessors unblock a task (p2). Default `all`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DependencyMode {
    /// Every predecessor must succeed.
    All,
    /// The first success unblocks.
    Any,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// What a predecessor ending `failed`, `cancelled` or `dead_lettered` does to
/// the task.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OnPredecessorFailure {
    /// Cancel the task with `dependency_failed`.
    #[default]
    Cancel,
    /// Count the predecessor as satisfied.
    Ignore,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// Wait-group mode.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WaitMode {
    /// Every condition must be satisfied.
    All,
    /// The first satisfied condition resumes the task.
    Any,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// Kind of a wait condition. `input`, `children` and `event` are p2.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WaitConditionKind {
    /// Satisfied at `resume_at`.
    Timer,
    /// Satisfied by a resolve call (p2).
    Input,
    /// Satisfied when the task has no unfinished children (p2).
    Children,
    /// Satisfied by a matching broker event (p2).
    Event,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// State of one wait condition.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WaitConditionState {
    /// Not yet satisfied.
    Pending,
    /// Satisfied.
    Satisfied,
    /// Its timeout elapsed.
    TimedOut,
    /// Dropped (the group was released or the task ended).
    Cancelled,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// What a condition timeout does.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OnTimeout {
    /// End the task `timed_out`.
    Fail,
    /// Resume the task with a timeout note.
    Resume,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// Who caused a reported error; affects only worker blacklisting.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorFault {
    /// The task itself (default).
    #[default]
    Task,
    /// A dependency of the task.
    Dependency,
    /// The worker.
    Worker,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// Queue kind.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum QueueKind {
    /// A queue workers claim from.
    Dispatch,
    /// A dead-letter queue (never claimed).
    DeadLetter,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// Retry backoff strategy.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RetryStrategy {
    /// The same interval every time.
    Fixed,
    /// The interval grows linearly.
    Linear,
    /// The interval doubles.
    Exponential,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// How tasks of different tenants are ordered within one urgency class.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TenantFairness {
    /// Tenants take turns (default).
    RoundRobin,
    /// All tenants ordered together by priority, then age.
    Fifo,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// Who runs a queue's workers.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OperatedBy {
    /// Tenant workers (default).
    Tenant,
    /// Platform workers.
    Platform,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// Progress event mode.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProgressEvents {
    /// No progress events.
    None,
    /// Events only for milestones.
    Milestones,
    /// An event for every flushed update.
    All,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// Cursors for the next and previous page, and the page size used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct PageInfo {
    /// Opaque, versioned; pass back as `cursor`. `None` on the last page.
    pub next_cursor: Option<String>,
    /// Cursor of the previous page.
    pub prev_cursor: Option<String>,
    /// Page size used (after clamping).
    pub limit: u32,
}

impl PageInfo {
    /// Build page info.
    #[must_use]
    pub const fn new(next_cursor: Option<String>, prev_cursor: Option<String>, limit: u32) -> Self {
        Self {
            next_cursor,
            prev_cursor,
            limit,
        }
    }
}

/// A tenant-local label. Keys up to 128 and values up to 256 characters; an
/// empty `value` works as a plain label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Tag {
    /// Tag key, unique per task.
    pub key: String,
    /// Tag value.
    pub value: String,
}

impl Tag {
    /// Build a tag.
    #[must_use]
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

/// Timeout settings in seconds. On create any subset may be given; on the task
/// resource these are the effective (resolved) values. `None` means "no
/// timeout of that kind", except for `heartbeat_timeout_secs` and
/// `execution_timeout_secs`, where it means "the engine default".
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "field names are the wire names of openapi.yaml"
)]
#[non_exhaustive]
pub struct Timeouts {
    /// Maximum time in `queued` before the task ends timed out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_timeout_secs: Option<u64>,
    /// Maximum time in `claimed` before start (a liveness failure).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment_timeout_secs: Option<u64>,
    /// Maximum running time per attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_timeout_secs: Option<u64>,
    /// Lease duration (maximum interval between heartbeats).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_timeout_secs: Option<u64>,
    /// Wall-clock budget from creation to a terminal state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifetime_timeout_secs: Option<u64>,
}

/// Retry policy, same shape as the queue registration. A policy on create is
/// taken whole, not merged per field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetryPolicy {
    /// Retries after retryable failures reported by a worker.
    pub max_retries: u32,
    /// Liveness requeues before the task ends `timed_out`; 0 = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_liveness_requeues: Option<u32>,
    /// Backoff strategy.
    pub strategy: RetryStrategy,
    /// Base backoff interval in seconds.
    pub base_interval_secs: u64,
    /// Maximum backoff interval in seconds (`>= base_interval_secs`).
    pub max_interval_secs: u64,
    /// Fractional jitter (0.0–1.0); only shortens a wait.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jitter_factor: Option<f64>,
    /// Error codes that are always terminal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub non_retryable_codes: Vec<String>,
}

impl RetryPolicy {
    /// Build a retry policy from its required fields.
    #[must_use]
    pub const fn new(
        max_retries: u32,
        strategy: RetryStrategy,
        base_interval_secs: u64,
        max_interval_secs: u64,
    ) -> Self {
        Self {
            max_retries,
            max_liveness_requeues: None,
            strategy,
            base_interval_secs,
            max_interval_secs,
            jitter_factor: None,
            non_retryable_codes: Vec::new(),
        }
    }
}
