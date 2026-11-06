//! Queue introspection and hold.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use gts::{GtsInstanceId, GtsTypeId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{
    OperatedBy, ProgressEvents, QueueKind, RetryPolicy, TaskState, TenantFairness, Timeouts,
};

/// Effective limits of a dispatch queue. `0` means unlimited.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[non_exhaustive]
pub struct QueueLimits {
    /// Concurrency across all tenants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_concurrency_limit: Option<u64>,
    /// Concurrency per tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_tenant_concurrency_limit: Option<u64>,
    /// Queued tasks across all tenants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_max_queue_depth: Option<u64>,
    /// Queued tasks per tenant (checked against a sampled count).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_max_queue_depth: Option<u64>,
    /// `scheduled` + `blocked` + `waiting` tasks per tenant (sampled).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_max_deferred_tasks: Option<u64>,
}

/// Effective non-limit settings of a dispatch queue after config overrides.
#[derive(
    Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[non_exhaustive]
pub struct QueueEffectiveSettings {
    /// Default task priority (0–1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_priority: Option<u16>,
    /// Default timeouts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeouts: Option<Timeouts>,
    /// Default retry policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_policy: Option<RetryPolicy>,
    /// Tenant ordering within one urgency class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_fairness: Option<TenantFairness>,
    /// Dead-letter queue.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub dead_letter_queue: Option<GtsInstanceId>,
    /// Who runs the queue's workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operated_by: Option<OperatedBy>,
    /// Task types the queue accepts; `None` = any.
    #[serde(default)]
    #[schema(value_type = Option<Vec<String>>)]
    pub accepted_types: Option<Vec<GtsTypeId>>,
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

/// Hold state of a queue: since when, by whom, why and until when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct QueueHold {
    /// Hold start.
    pub since: DateTime<Utc>,
    /// Subject that held the queue.
    pub by: String,
    /// Reason.
    #[serde(default)]
    pub reason: Option<String>,
    /// Automatic release time.
    #[serde(default)]
    pub until: Option<DateTime<Utc>>,
}

impl QueueHold {
    /// Build a hold record.
    #[must_use]
    pub fn new(since: DateTime<Utc>, by: impl Into<String>) -> Self {
        Self {
            since,
            by: by.into(),
            reason: None,
            until: None,
        }
    }
}

/// Queue resource.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct Queue {
    /// GTS queue ID.
    #[schema(value_type = String)]
    pub id: GtsInstanceId,
    /// Queue kind.
    pub kind: QueueKind,
    /// Effective limits; `None` for a `dead_letter` queue.
    #[serde(default)]
    pub limits: Option<QueueLimits>,
    /// Domain set: primary first, plus the source domain while a move lasts.
    pub domains: Vec<String>,
    /// Tasks per state: the live states for a dispatch queue, `dead_lettered`
    /// for a dead-letter queue.
    pub counts: BTreeMap<TaskState, u64>,
    /// When `counts` were taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampled_at: Option<DateTime<Utc>>,
    /// Hold state; `None` when not held.
    pub hold: Option<QueueHold>,
    /// Effective settings; `None` for a `dead_letter` queue.
    #[serde(default)]
    pub settings: Option<QueueEffectiveSettings>,
    /// Runtime configuration version applied in the queue's domain.
    #[serde(default)]
    pub config_version: Option<u64>,
}

impl Queue {
    /// Build a queue resource.
    #[must_use]
    pub fn new(
        id: GtsInstanceId,
        kind: QueueKind,
        domains: Vec<String>,
        counts: BTreeMap<TaskState, u64>,
    ) -> Self {
        Self {
            id,
            kind,
            limits: None,
            domains,
            counts,
            sampled_at: None,
            hold: None,
            settings: None,
            config_version: None,
        }
    }
}

/// All queues, unpaged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct QueueList {
    /// The queues.
    pub items: Vec<Queue>,
}

impl QueueList {
    /// Build a queue list.
    #[must_use]
    pub const fn new(items: Vec<Queue>) -> Self {
        Self { items }
    }
}

/// Hold a queue. Both fields are optional.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct HoldRequest {
    /// Reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Automatic release time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
}

impl HoldRequest {
    /// Build a hold request.
    #[must_use]
    pub const fn new(reason: Option<String>, until: Option<DateTime<Utc>>) -> Self {
        Self { reason, until }
    }
}
