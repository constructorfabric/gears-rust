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

/// Re-enqueue many tasks. At most the configured bulk size (default 1,000);
/// more → `400` `BATCH_TOO_LARGE`.
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

/// Cancel many tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct BulkCancelRequest {
    /// Unique task IDs.
    pub task_ids: Vec<Uuid>,
    /// Cancellation mode.
    pub mode: CancelMode,
    /// Free-text reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl BulkCancelRequest {
    /// Build the request.
    #[must_use]
    pub const fn new(task_ids: Vec<Uuid>, mode: CancelMode) -> Self {
        Self {
            task_ids,
            mode,
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

/// Move many tasks to another queue of the same task domain.
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

/// Per-task outcome of a bulk operation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BulkOutcome {
    /// Applied.
    Succeeded,
    /// The caller may read the task but not perform the action.
    Unauthorized,
    /// Missing, or not readable by the caller.
    NotFound,
    /// The task's state does not allow the action.
    InvalidState,
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

/// Hand a claimed or running task to another identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ReassignRequest {
    /// New assignee subject.
    pub subject_id: String,
    /// New assignee worker.
    pub worker_id: String,
}

impl ReassignRequest {
    /// Build the request.
    #[must_use]
    pub fn new(subject_id: impl Into<String>, worker_id: impl Into<String>) -> Self {
        Self {
            subject_id: subject_id.into(),
            worker_id: worker_id.into(),
        }
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
/// GTS patterns (`gts.cf.core.tasks.queue.v1~acme.reports.*`); the most
/// specific pattern wins.
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
}

impl RuntimeConfigRequest {
    /// Build a configuration version.
    #[must_use]
    pub const fn new(
        queues: BTreeMap<String, QueueSettingsOverride>,
        types: BTreeMap<String, TypeSettingsOverride>,
    ) -> Self {
        Self { queues, types }
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
