//! Bulk operations (p2), reassignment and runtime configuration (p3).

use std::collections::BTreeMap;

use gts::GtsInstanceId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::{CancelMode, ProgressEvents, RetryPolicy, Timeouts, Urgency};

// ---------------------------------------------------------------------------
// Bulk
// ---------------------------------------------------------------------------

/// Re-enqueue many tasks. At most `bulk_max_ids` (default 1000); more → `400`
/// `BATCH_TOO_LARGE` on `/task_ids`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct BulkTaskIdsRequest {
    /// Unique task IDs.
    pub task_ids: Vec<Uuid>,
}

impl BulkTaskIdsRequest {
    /// Build the request.
    #[must_use]
    pub const fn new(task_ids: Vec<Uuid>) -> Self {
        Self { task_ids }
    }
}

/// Cancel many tasks. Each task follows the single-cancel rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct BulkCancelRequest {
    /// Unique task IDs, at most `bulk_max_ids` (default 1000).
    pub task_ids: Vec<Uuid>,
    /// Cancellation mode (default `requested`, server-side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<CancelMode>,
    /// Free text, at most 1024 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl BulkCancelRequest {
    /// Build the request with the default mode (`requested`).
    #[must_use]
    pub const fn new(task_ids: Vec<Uuid>) -> Self {
        Self {
            task_ids,
            mode: None,
            reason: None,
        }
    }

    /// Build the request with an explicit mode.
    #[must_use]
    pub const fn with_mode(task_ids: Vec<Uuid>, mode: CancelMode) -> Self {
        Self {
            task_ids,
            mode: Some(mode),
            reason: None,
        }
    }
}

/// Change priority and/or urgency of many tasks; at least one of the two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct BulkReprioritizeRequest {
    /// Unique task IDs.
    pub task_ids: Vec<Uuid>,
    /// New priority (0–1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u16>,
    /// New urgency; raising to `critical` needs `expedite` per task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgency: Option<Urgency>,
}

impl BulkReprioritizeRequest {
    /// Build the request.
    #[must_use]
    pub const fn new(task_ids: Vec<Uuid>, priority: Option<u16>, urgency: Option<Urgency>) -> Self {
        Self {
            task_ids,
            priority,
            urgency,
        }
    }
}

/// Move many tasks to another queue of the same task domain. One destination
/// serves every task: an unregistered destination refuses the whole request
/// (`404`), a dead-letter one too (`400` `DEAD_LETTER_QUEUE` on `/queue`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct BulkMoveRequest {
    /// Unique task IDs.
    pub task_ids: Vec<Uuid>,
    /// Destination queue.
    #[schema(value_type = String)]
    pub queue: GtsInstanceId,
}

impl BulkMoveRequest {
    /// Build the request.
    #[must_use]
    pub const fn new(task_ids: Vec<Uuid>, queue: GtsInstanceId) -> Self {
        Self { task_ids, queue }
    }
}

/// Per-task outcome of a bulk operation. Open set.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BulkOutcome {
    /// Applied; for cancel, the task ended (or had already ended in the
    /// cancelled family).
    Succeeded,
    /// The caller may read the task but not perform the action.
    Unauthorized,
    /// Missing, or not readable by the caller.
    NotFound,
    /// The task's state does not allow the action (for retry also a
    /// precondition refusal: `TASK_DOMAIN_MISMATCH`, `PARENT_FINISHED`,
    /// `STALE_ORDER`).
    InvalidState,
    /// Cancel: a leased task was only flagged (`requested`/`graceful`).
    CancelRequested,
    /// Retry, move: refused by a queue-depth limit (no usage figures).
    LimitExceeded,
    /// Move: the destination is in another task domain.
    DomainMismatch,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// Outcome for one requested task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct BulkTaskResult {
    /// The task.
    pub task_id: Uuid,
    /// Outcome.
    pub outcome: BulkOutcome,
}

impl BulkTaskResult {
    /// Build a per-task result.
    #[must_use]
    pub const fn new(task_id: Uuid, outcome: BulkOutcome) -> Self {
        Self { task_id, outcome }
    }
}

/// Multi-status in the body: one outcome per requested ID, in request order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct BulkResult {
    /// Per-task outcomes.
    pub results: Vec<BulkTaskResult>,
}

impl BulkResult {
    /// Build a bulk result.
    #[must_use]
    pub const fn new(results: Vec<BulkTaskResult>) -> Self {
        Self { results }
    }
}

// ---------------------------------------------------------------------------
// Reassignment (p3)
// ---------------------------------------------------------------------------

/// Hand a claimed or running task to another identity. The target's rights
/// are not checked here: its accept is authorized with its own scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ReassignRequest {
    /// New assignee subject.
    pub subject_id: String,
    /// New assignee worker.
    pub worker_id: String,
    /// Free text, at most 1024 characters (`TOO_LONG`), recorded in
    /// `taskReassigned`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ReassignRequest {
    /// Build the request.
    #[must_use]
    pub fn new(subject_id: impl Into<String>, worker_id: impl Into<String>) -> Self {
        Self {
            subject_id: subject_id.into(),
            worker_id: worker_id.into(),
            reason: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Re-enqueue (p2)
// ---------------------------------------------------------------------------

/// Optional re-enqueue body. Without `queue` the task returns to its source
/// queue (`dead_lettered_from`, or its own queue for a failed task);
/// `ReenqueueRequest::default()` sends `{}`.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ReenqueueRequest {
    /// Dispatch queue to re-enqueue into: routed to the task's current domain
    /// and accepting its type. A different queue re-resolves the queue-level
    /// settings, as a move does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub queue: Option<GtsInstanceId>,
}

impl ReenqueueRequest {
    /// Build the request.
    #[must_use]
    pub const fn new(queue: Option<GtsInstanceId>) -> Self {
        Self { queue }
    }
}

// ---------------------------------------------------------------------------
// Runtime configuration (p3)
// ---------------------------------------------------------------------------

/// Queue settings a runtime version may override.
#[derive(
    Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct QueueSettingsOverride {
    /// Concurrency across all tenants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_concurrency_limit: Option<u64>,
    /// Concurrency per tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_tenant_concurrency_limit: Option<u64>,
    /// Default priority (0–1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_priority: Option<u16>,
    /// Queued tasks across all tenants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_max_queue_depth: Option<u64>,
    /// Queued tasks per tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_max_queue_depth: Option<u64>,
    /// Timeouts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeouts: Option<Timeouts>,
    /// Retry policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_policy: Option<RetryPolicy>,
    /// Progress event mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_events: Option<ProgressEvents>,
    /// Progress flush interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_flush_interval_secs: Option<u64>,
    /// Consecutive worker failures that blacklist the worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blacklist_failure_threshold: Option<u32>,
    /// How long a blacklisted worker is excluded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blacklist_duration_minutes: Option<u32>,
}

/// Task-type settings a runtime version may override.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TypeSettingsOverride {
    /// Concurrency across all tenants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_concurrency_limit: Option<u64>,
    /// Concurrency per tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_tenant_concurrency_limit: Option<u64>,
    /// Progress event mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_events: Option<ProgressEvents>,
    /// Progress flush interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_flush_interval_secs: Option<u64>,
}

/// The complete override set of a new runtime configuration version: a key not
/// present reverts to the registration or engine default. Keys are exact IDs or
/// GTS patterns (`gts.cf.core.tasks.queue.v1~acme.reports.*`): an exact ID
/// beats any pattern, then the longest literal prefix before the first
/// wildcard; two patterns of equal specificity matching the same queue are
/// refused (`NOT_ALLOWED` on the later pattern).
#[derive(
    Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RuntimeConfigRequest {
    /// Overrides by queue ID or pattern.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub queues: BTreeMap<String, QueueSettingsOverride>,
    /// Overrides by task-type ID or pattern.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub types: BTreeMap<String, TypeSettingsOverride>,
    /// Retention overrides, such as a per-tenant retention or legal hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<RetentionOverride>,
}

impl RuntimeConfigRequest {
    /// Build a configuration version.
    #[must_use]
    pub const fn new(
        queues: BTreeMap<String, QueueSettingsOverride>,
        types: BTreeMap<String, TypeSettingsOverride>,
    ) -> Self {
        Self {
            queues,
            types,
            retention: None,
        }
    }

    /// Add retention overrides to this version.
    #[must_use]
    pub fn with_retention(mut self, retention: RetentionOverride) -> Self {
        self.retention = Some(retention);
        self
    }
}

/// Retention overrides of a runtime configuration version. A legal hold is a long
/// per-tenant retention.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetentionOverride {
    /// Retention by owner tenant ID. A policy applies to that tenant only, not to its
    /// subtree, and reaches every row not yet removed. A value above
    /// `trust_limits.global.max_retention_days` is refused (`400`, `OUT_OF_RANGE`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_tenant: BTreeMap<Uuid, TenantRetention>,
}

impl RetentionOverride {
    /// Build retention overrides from per-tenant policies.
    #[must_use]
    pub const fn new(per_tenant: BTreeMap<Uuid, TenantRetention>) -> Self {
        Self { per_tenant }
    }
}

/// One tenant's retention policy.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TenantRetention {
    /// Days a finished task of this tenant is kept, at least 1.
    pub retention_days: u32,
}

impl TenantRetention {
    /// Build a tenant retention policy.
    #[must_use]
    pub const fn new(retention_days: u32) -> Self {
        Self { retention_days }
    }
}

/// A saved runtime configuration version, applied asynchronously per domain.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[non_exhaustive]
pub struct RuntimeConfigAccepted {
    /// The new version.
    pub config_version: u64,
}

impl RuntimeConfigAccepted {
    /// Build the response.
    #[must_use]
    pub const fn new(config_version: u64) -> Self {
        Self { config_version }
    }
}

/// Apply status of one task domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct DomainApplyStatus {
    /// Task domain.
    pub domain: String,
    /// Whether the domain has applied the version (versions are cumulative).
    pub applied: bool,
}

impl DomainApplyStatus {
    /// Build a per-domain status.
    #[must_use]
    pub fn new(domain: impl Into<String>, applied: bool) -> Self {
        Self {
            domain: domain.into(),
            applied,
        }
    }
}

/// Which task domains have applied a configuration version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct RuntimeConfigStatus {
    /// The version.
    pub config_version: u64,
    /// Per-domain status.
    pub domains: Vec<DomainApplyStatus>,
}

impl RuntimeConfigStatus {
    /// Build the status.
    #[must_use]
    pub const fn new(config_version: u64, domains: Vec<DomainApplyStatus>) -> Self {
        Self {
            config_version,
            domains,
        }
    }
}
