//! The task resource and the API-consumer requests on it (create, cancel, tags).

use chrono::{DateTime, Utc};
use gts::{GtsInstanceId, GtsTypeId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::{
    CancelMode, DependencyMode, ErrorFault, OnPredecessorFailure, PredecessorOutcome, ResultCode,
    RetryPolicy, Tag, TaskState, Timeouts, Urgency,
};
use super::wait::InitialWaitGroup;

// ---------------------------------------------------------------------------
// Task parts
// ---------------------------------------------------------------------------

/// A platform resource the task works on (p2), as given on create.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ResourceRefInput {
    /// GTS type ID of the resource.
    #[schema(value_type = String)]
    pub resource_type: GtsTypeId,
    /// Resource ID, at most 256 characters (`TOO_LONG` on
    /// `/resource_refs/i/resource_id`).
    pub resource_id: String,
}

impl ResourceRefInput {
    /// Build a resource reference.
    #[must_use]
    pub fn new(resource_type: GtsTypeId, resource_id: impl Into<String>) -> Self {
        Self {
            resource_type,
            resource_id: resource_id.into(),
        }
    }
}

/// A platform resource the task works on, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct ResourceRef {
    /// GTS type ID of the resource.
    #[schema(value_type = String)]
    pub resource_type: GtsTypeId,
    /// Resource ID.
    pub resource_id: String,
    /// `true` only when a creator-authority check confirmed it at creation.
    pub verified: bool,
}

impl ResourceRef {
    /// Build a stored resource reference.
    #[must_use]
    pub fn new(resource_type: GtsTypeId, resource_id: impl Into<String>, verified: bool) -> Self {
        Self {
            resource_type,
            resource_id: resource_id.into(),
            verified,
        }
    }
}

/// A predecessor as given on create (p2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct PredecessorInput {
    /// Predecessor task; the caller needs `read` on it. Looked up only in the
    /// new task's domain: one that is missing, unreadable or in another domain
    /// → `404`. Listing the same task twice → `400` `DUPLICATE` on the second.
    pub task_id: Uuid,
    /// What the predecessor failing does to this task (default `cancel`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_predecessor_failure: Option<OnPredecessorFailure>,
}

impl PredecessorInput {
    /// Build a predecessor with the default failure handling.
    #[must_use]
    pub const fn new(task_id: Uuid) -> Self {
        Self {
            task_id,
            on_predecessor_failure: None,
        }
    }
}

/// A predecessor as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct Predecessor {
    /// Predecessor task.
    pub task_id: Uuid,
    /// What the predecessor failing does to this task.
    pub on_predecessor_failure: OnPredecessorFailure,
    /// What this predecessor did for the task; see
    /// [`PredecessorOutcome::is_satisfied`].
    pub outcome: PredecessorOutcome,
}

impl Predecessor {
    /// Build a stored predecessor.
    #[must_use]
    pub const fn new(
        task_id: Uuid,
        on_predecessor_failure: OnPredecessorFailure,
        outcome: PredecessorOutcome,
    ) -> Self {
        Self {
            task_id,
            on_predecessor_failure,
            outcome,
        }
    }
}

/// A structured error (or warning) reported by a worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct TaskError {
    /// Error domain.
    pub domain: String,
    /// Error code; matched against the retry policy's `non_retryable_codes`.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Optional structured detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    /// Who caused the error (default `task`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fault: Option<ErrorFault>,
    /// Whether a retry may help (default `false`).
    #[serde(default)]
    pub retryable: bool,
}

impl TaskError {
    /// Build a non-retryable task error.
    #[must_use]
    pub fn new(
        domain: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            domain: domain.into(),
            code: code.into(),
            message: message.into(),
            context: None,
            fault: None,
            retryable: false,
        }
    }
}

/// The stored result of a task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct TaskResult {
    /// Result code.
    pub result_code: ResultCode,
    /// Output JSON, validated against the type's output schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    /// The error of a failed task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TaskError>,
    /// Warnings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<TaskError>,
}

impl TaskResult {
    /// Build a result with only a result code.
    #[must_use]
    pub const fn new(result_code: ResultCode) -> Self {
        Self {
            result_code,
            output: None,
            error: None,
            warnings: Vec::new(),
        }
    }
}

/// The last saved progress of a task.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[non_exhaustive]
pub struct ProgressInfo {
    /// Percent done, 0–100.
    #[serde(default)]
    pub percent: Option<u8>,
    /// Last milestone.
    #[serde(default)]
    pub milestone: Option<String>,
    /// Units done.
    #[serde(default)]
    pub completed_units: Option<u64>,
    /// Units in total.
    #[serde(default)]
    pub total_units: Option<u64>,
    /// Current stage.
    #[serde(default)]
    pub current_stage: Option<String>,
    /// Estimated completion time.
    #[serde(default)]
    pub eta: Option<DateTime<Utc>>,
}

/// Current holder of a `claimed`/`running` task. Never carries the lease token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct Assignment {
    /// Subject holding the task.
    pub assignee_subject: String,
    /// Worker process identity within the subject.
    pub worker_id: String,
    /// When the task was claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_at: Option<DateTime<Utc>>,
    /// Attempt number (from 1).
    pub attempt: u32,
    /// Lease epoch (from 1).
    pub lease_epoch: u64,
    /// Set by the claim (claim time + effective execution timeout) and never
    /// moved by `start`; the attempt ends `timed_out` then, whether `claimed` or
    /// `running`.
    #[serde(default)]
    pub execution_deadline: Option<DateTime<Utc>>,
}

impl Assignment {
    /// Build an assignment.
    #[must_use]
    pub fn new(
        assignee_subject: impl Into<String>,
        worker_id: impl Into<String>,
        attempt: u32,
        lease_epoch: u64,
    ) -> Self {
        Self {
            assignee_subject: assignee_subject.into(),
            worker_id: worker_id.into(),
            claimed_at: None,
            attempt,
            lease_epoch,
            execution_deadline: None,
        }
    }
}

/// The pending cancel request of a leased task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct CancelRequestInfo {
    /// Requested mode.
    pub mode: CancelMode,
    /// Subject that requested the cancel.
    pub requested_by: String,
    /// When it was requested.
    pub requested_at: DateTime<Utc>,
    /// Free-text reason, at most 1024 characters.
    #[serde(default)]
    pub reason: Option<String>,
    /// `graceful` only: when the cancel becomes forced.
    #[serde(default)]
    pub grace_until: Option<DateTime<Utc>>,
    /// Set when the request comes from supersession (p2); the heartbeat passes
    /// it on in [`HeartbeatResult::cancel_request`](super::HeartbeatResult::cancel_request).
    #[serde(default)]
    pub superseded_by: Option<Uuid>,
}

impl CancelRequestInfo {
    /// Build a cancel request record.
    #[must_use]
    pub fn new(
        mode: CancelMode,
        requested_by: impl Into<String>,
        requested_at: DateTime<Utc>,
    ) -> Self {
        Self {
            mode,
            requested_by: requested_by.into(),
            requested_at,
            reason: None,
            grace_until: None,
            superseded_by: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Task resource
// ---------------------------------------------------------------------------

/// The task resource, returned by create, read, claim and the worker and admin
/// calls. It never carries the `Idempotency-Key` or a lease token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[allow(
    clippy::struct_field_names,
    reason = "field names are the wire names of openapi.yaml"
)]
#[non_exhaustive]
pub struct Task {
    /// UUID v7 (creation order is ID order).
    pub id: Uuid,
    /// GTS type ID of the task type (`gts.cf.core.tasks.task.v1~…~`).
    #[serde(rename = "type")]
    #[schema(value_type = String)]
    pub task_type: GtsTypeId,
    /// GTS queue ID (`gts.cf.core.tasks.queue.v1~…`).
    #[schema(value_type = String)]
    pub queue: GtsInstanceId,
    /// Owning tenant.
    pub owner_tenant_id: Uuid,
    /// Subject the task is done for (gear-mediated creation).
    #[serde(default)]
    pub owner_id: Option<String>,
    /// Creating subject.
    pub created_by: String,
    /// Current state.
    pub state: TaskState,
    /// Urgency class.
    pub urgency: Urgency,
    /// Tenant-local priority; 0 highest, 1000 lowest.
    pub priority: u16,
    /// Incremented on each claim only.
    pub attempt: u32,
    /// Retries used after reported failures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<u32>,
    /// Requeues after lease expiry or assignment timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liveness_requeues: Option<u32>,
    /// Whether `requested`/`graceful` cancels are accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellable: Option<bool>,
    /// Whether a cancel was requested on the leased task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_requested: Option<bool>,
    /// Task input, valid against the type's input schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    /// Checkpoint data, merged by checkpoints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    /// Result, once there is one.
    #[serde(default)]
    pub result: Option<TaskResult>,
    /// Last saved progress.
    #[serde(default)]
    pub progress: Option<ProgressInfo>,
    /// Tags, sorted by key.
    #[serde(default)]
    pub tags: Vec<Tag>,
    /// Resources the task works on (p2).
    #[serde(default)]
    pub resource_refs: Vec<ResourceRef>,
    /// Parent task (p2).
    #[serde(default)]
    pub parent_task_id: Option<Uuid>,
    /// Whether successful completion waits for children (p2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_for_children: Option<bool>,
    /// When `true`, a cancel reaches the unfinished children in the same mode,
    /// and a parent that ends in any other non-success state cancels them
    /// `graceful` (p2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub propagate_cancel: Option<bool>,
    /// Predecessors (p2), at most `max_predecessors_per_task` (64); each with
    /// its `outcome`.
    #[serde(default)]
    pub predecessors: Vec<Predecessor>,
    /// Dependency mode (p2).
    #[serde(default)]
    pub dependency_mode: Option<DependencyMode>,
    /// Effective timeouts, resolved and stored at create.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeouts: Option<Timeouts>,
    /// Effective retry policy, resolved and stored at create. Not nullable:
    /// the engine always sends it on the full resource; `None` only when an
    /// abbreviated body omits it (the schema does not list it as required).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_policy: Option<RetryPolicy>,
    /// Earliest run time.
    #[serde(default)]
    pub run_after: Option<DateTime<Utc>>,
    /// SLA deadline. A passed deadline is cleared by re-enqueue.
    #[serde(default)]
    pub sla_deadline: Option<DateTime<Utc>>,
    /// Current holder of a `claimed`/`running` task.
    #[serde(default)]
    pub assignment: Option<Assignment>,
    /// Source queue of a dead-lettered task; cleared by re-enqueue. Filterable
    /// in search and part of the `short` view.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub dead_lettered_from: Option<GtsInstanceId>,
    /// Replacing task of a superseded task (p2).
    #[serde(default)]
    pub superseded_by: Option<Uuid>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last change.
    pub updated_at: DateTime<Utc>,
    /// When the task reached a terminal state. Cleared by re-enqueue.
    #[serde(default)]
    pub terminal_at: Option<DateTime<Utc>>,
    /// Task domain the task was created in; fixed for its life.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// Set while `cancel_requested` is true.
    #[serde(default)]
    pub cancel_request: Option<CancelRequestInfo>,
}

impl Task {
    /// Build a task from its required fields; every optional field is empty.
    #[must_use]
    #[allow(
        clippy::too_many_arguments,
        reason = "one argument per required field of the wire resource"
    )]
    pub fn new(
        id: Uuid,
        task_type: GtsTypeId,
        queue: GtsInstanceId,
        owner_tenant_id: Uuid,
        created_by: impl Into<String>,
        state: TaskState,
        urgency: Urgency,
        priority: u16,
        attempt: u32,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            task_type,
            queue,
            owner_tenant_id,
            owner_id: None,
            created_by: created_by.into(),
            state,
            urgency,
            priority,
            attempt,
            retry_count: None,
            liveness_requeues: None,
            cancellable: None,
            cancel_requested: None,
            input: None,
            context: None,
            result: None,
            progress: None,
            tags: Vec::new(),
            resource_refs: Vec::new(),
            parent_task_id: None,
            wait_for_children: None,
            propagate_cancel: None,
            predecessors: Vec::new(),
            dependency_mode: None,
            timeouts: None,
            retry_policy: None,
            run_after: None,
            sla_deadline: None,
            assignment: None,
            dead_lettered_from: None,
            superseded_by: None,
            created_at,
            updated_at,
            terminal_at: None,
            domain: None,
            cancel_request: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// Create a task — the `CreateTaskRequest` body.
///
/// Also carries [`idempotency_key`](Self::idempotency_key), which `openapi.yaml`
/// sends as the `Idempotency-Key` header, and never `created_by` (taken from
/// the caller's security context).
///
/// **Wire gap.** `#[toolkit::rest_contract]` cannot bind a parameter to a
/// request header yet (`#[header]` is deferred in the contract-binding
/// DESIGN), so the generated REST client sends `idempotency_key` as a body
/// field instead of the header. `create_task` is `#[server_manual]`, so the
/// gear's handler accepts both until header binding lands; then this field moves
/// to the header and the body matches the document's schema exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CreateTaskRequest {
    /// Platform idempotency key, unique per tenant and creating subject: 1–255
    /// printable ASCII characters without spaces (`!`–`~`), case-sensitive
    /// (empty or malformed → `400` `INVALID_FORMAT` on `Idempotency-Key`; the
    /// SDK does not validate it). Required for every task type without a
    /// derived dedup identity (missing → `428`); ignored for types that declare
    /// the `dedup` trait (p2). Never returned on the task resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// GTS type ID of the task type.
    #[serde(rename = "type")]
    #[schema(value_type = String)]
    pub task_type: GtsTypeId,
    /// GTS queue ID. Absent → the type's `default_queue`, else `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub queue: Option<GtsInstanceId>,
    /// Priority 0–1000; default is the queue's `default_priority`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u16>,
    /// Urgency (default `normal`; `critical` needs `expedite`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgency: Option<Urgency>,
    /// Input, validated against the type's input schema.
    pub input: serde_json::Value,
    /// Initial context (part of the 64 KiB payload).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    /// Tenant that owns the task; default the caller's tenant. The `create`
    /// check carries it and the PDP decides (subtree and barrier rules); a
    /// tenant barrier is crossed only for a queue that is `operated_by:
    /// platform`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_tenant_id: Option<Uuid>,
    /// Subject the task is done for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
    /// Timeouts (any subset; a value above the type's trust limit → `400`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeouts: Option<Timeouts>,
    /// Retry policy (required fields required, not merged with other levels).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_policy: Option<RetryPolicy>,
    /// Earliest claim time; a future value creates the task `scheduled`. A past
    /// value is stored as sent and the task is `queued`; a value later than
    /// `created_at` + the lifetime timeout → `400` `OUT_OF_RANGE`. Not with an
    /// initial `wait` group when in the future.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_after: Option<DateTime<Utc>>,
    /// SLA deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sla_deadline: Option<DateTime<Utc>>,
    /// Whether `requested`/`graceful` cancels are accepted (default `true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellable: Option<bool>,
    /// Tags, at most `max_tags_per_task` (16; more → `TOO_MANY_TAGS`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<Tag>,
    /// Resources the task works on (p2), at most 16.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resource_refs: Vec<ResourceRefInput>,
    /// Parent task (p2; refused in p1 with `FEATURE_NOT_AVAILABLE` on
    /// `/parent_task_id`). Requires `parent_lease_token`. Looked up only in
    /// the new task's domain (another domain → `404`); the hierarchy may be at
    /// most `max_hierarchy_depth` (10) deep (deeper → `TOO_DEEP`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<Uuid>,
    /// The parent's lease token held by the calling worker. Required with
    /// `parent_task_id` (missing → `REQUIRED`); a wrong, stale or foreign token
    /// → the same `404` as a missing parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_lease_token: Option<Uuid>,
    /// Whether successful completion waits for children (default `false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_for_children: Option<bool>,
    /// Whether cancellation reaches children (default `true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub propagate_cancel: Option<bool>,
    /// Predecessors (p2), at most `max_predecessors_per_task` (64; more →
    /// `DEPENDENCY_GRAPH_TOO_LARGE`); the same task twice → `DUPLICATE`. Not
    /// with an initial `wait` group. Fixed after creation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub predecessors: Vec<PredecessorInput>,
    /// Dependency mode (default `all`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency_mode: Option<DependencyMode>,
    /// Initial wait group: the task starts `waiting`. Not with `predecessors`
    /// or a future `run_after`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<InitialWaitGroup>,
}

impl CreateTaskRequest {
    /// Build a create request with a type, an input and an idempotency key.
    #[must_use]
    pub fn new(
        task_type: GtsTypeId,
        input: serde_json::Value,
        idempotency_key: impl Into<String>,
    ) -> Self {
        Self {
            idempotency_key: Some(idempotency_key.into()),
            task_type,
            queue: None,
            priority: None,
            urgency: None,
            input,
            context: None,
            owner_tenant_id: None,
            owner_id: None,
            timeouts: None,
            retry_policy: None,
            run_after: None,
            sla_deadline: None,
            cancellable: None,
            tags: Vec::new(),
            resource_refs: Vec::new(),
            parent_task_id: None,
            parent_lease_token: None,
            wait_for_children: None,
            propagate_cancel: None,
            predecessors: Vec::new(),
            dependency_mode: None,
            wait: None,
        }
    }
}

/// Cancel a task. The body is optional on the wire (`CancelRequest::default()`
/// sends `{}`); `mode` defaults to `requested` server-side.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CancelRequest {
    /// Cancellation mode (default `requested`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<CancelMode>,
    /// Free text, at most 1024 characters (`TOO_LONG` on `/reason`), stored as
    /// `cancel_request.reason`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl CancelRequest {
    /// Build a cancel request with a mode.
    #[must_use]
    pub const fn new(mode: CancelMode) -> Self {
        Self {
            mode: Some(mode),
            reason: None,
        }
    }
}

/// Change a task's tags (p2). `set` adds or replaces by key, `remove` deletes
/// keys; keys not named are unchanged. A key in both, or twice in `set` →
/// `400`; a result over `max_tags_per_task` (16) → `400` `TOO_MANY_TAGS`.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TagsPatchRequest {
    /// Tags to add or replace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub set: Vec<Tag>,
    /// Keys to remove.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
}

impl TagsPatchRequest {
    /// Build a tag patch.
    #[must_use]
    pub const fn new(set: Vec<Tag>, remove: Vec<String>) -> Self {
        Self { set, remove }
    }
}
