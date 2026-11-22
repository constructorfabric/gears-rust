//! Search, count, entries and history: query parameters, pages and items.
//!
//! Every list that can grow is cursor-paged: `limit` + `cursor` in, `{ items,
//! page_info }` out, no total. The query structs spell the `OData` options the
//! way the wire does (`$filter`, `$orderby`, `$select`); the aliases `$top` /
//! `$skiptoken` the engine also accepts are not modelled.

use chrono::{DateTime, Utc};
use gts::{GtsInstanceId, GtsTypeId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::{PageInfo, Tag, TaskState, Urgency};
use super::entry::Entry;
use super::task::{Assignment, Predecessor, ProgressInfo, ResourceRef, TaskResult};
use super::wait::WaitCondition;

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

/// Detail level of a task search item (`view`). Default `short`; any other
/// value → `400` `NOT_ALLOWED` on `view`. A cursor is bound to the `view` it
/// was issued for.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskView {
    /// `id`, `state`, `type`.
    Tiny,
    /// + `urgency`, `priority`, `queue`, `assignment`, `dead_lettered_from`,
    ///   `created_at`, `updated_at`.
    Short,
    /// + `input`, `result`, `context`, `progress`, `tags`, `resource_refs`.
    Long,
    /// + `events` (newest 50), current wait conditions, `entries` (newest 20),
    ///   `dependencies`, and the two `*_truncated` flags.
    Full,
}

impl toolkit_contract::query::QueryScalar for TaskView {}

/// Sort order of a task search (`$orderby`). `id` is always the tiebreaker;
/// other keys → `400` `NOT_ALLOWED` on `$orderby`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
pub enum TaskOrderBy {
    /// Newest first (default).
    #[serde(rename = "created_at desc")]
    CreatedAtDesc,
    /// Oldest first.
    #[serde(rename = "created_at asc")]
    CreatedAtAsc,
    /// Latest terminal first; only when `$filter` limits `state` to terminal states.
    #[serde(rename = "terminal_at desc")]
    TerminalAtDesc,
    /// Earliest terminal first; only when `$filter` limits `state` to terminal states.
    #[serde(rename = "terminal_at asc")]
    TerminalAtAsc,
}

impl toolkit_contract::query::QueryScalar for TaskOrderBy {}

/// Query of `GET /tasks`: keyset-paged search over the caller's visible tasks.
///
/// A search that can return terminal tasks covers at most
/// `search_max_window_days` (31) of `created_at`; without a range the engine
/// adds `created_at ge now − search_default_window_days` (7), and a range over
/// the window → `400` `OUT_OF_RANGE` on `$filter`. A `state` filter that allows
/// only non-terminal states has no window, and neither has one restricted to
/// `dead_lettered` (`read` is enough to list those; a `queue` filter on a
/// dead-letter queue searches every task domain). A scope that matches nothing
/// gives an empty page.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
    utoipa::ToSchema,
    toolkit::QueryParams,
)]
#[non_exhaustive]
pub struct SearchTasksQuery {
    /// `OData` filter over `state`, `assignee`, `queue`, `type`, `urgency`,
    /// `priority`, `created_at`, `dependencies`, `parent_task_id`,
    /// `predecessor_id`, `dead_lettered_from`, and the filter functions
    /// `has_tag('<key>')`, `has_tag('<key>', '<value>')` and
    /// `has_resource_ref('<resource_type>', '<resource_id>')` (see
    /// [`has_tag`](Self::has_tag), [`has_resource_ref`](Self::has_resource_ref)).
    #[serde(rename = "$filter")]
    pub filter: Option<String>,
    /// Sort order.
    #[serde(rename = "$orderby")]
    pub order_by: Option<TaskOrderBy>,
    /// Comma-separated top-level fields, narrowing the item within `view`; a
    /// field outside the view → `400` `NOT_ALLOWED` on `$select`.
    #[serde(rename = "$select")]
    pub select: Option<String>,
    /// Page size (default 25; clamped to 100, or 20 at `view=full`).
    pub limit: Option<u32>,
    /// `page_info.next_cursor` (or `prev_cursor`) of the previous page.
    pub cursor: Option<String>,
    /// Detail level.
    pub view: Option<TaskView>,
}

impl SearchTasksQuery {
    /// Search with a filter and defaults for everything else.
    #[must_use]
    pub fn filter(filter: impl Into<String>) -> Self {
        Self {
            filter: Some(filter.into()),
            ..Self::default()
        }
    }

    /// `$filter` term `has_tag('<key>')`: the task has a tag with this key.
    /// Single quotes in `key` are doubled, as `OData` string literals require.
    #[must_use]
    pub fn has_tag(key: &str) -> String {
        format!("has_tag({})", odata_string(key))
    }

    /// `$filter` term `has_tag('<key>', '<value>')`: the task has this tag.
    #[must_use]
    pub fn has_tag_value(key: &str, value: &str) -> String {
        format!("has_tag({}, {})", odata_string(key), odata_string(value))
    }

    /// `$filter` term `has_resource_ref('<resource_type>', '<resource_id>')`:
    /// the task works on this resource.
    #[must_use]
    pub fn has_resource_ref(resource_type: &str, resource_id: &str) -> String {
        format!(
            "has_resource_ref({}, {})",
            odata_string(resource_type),
            odata_string(resource_id)
        )
    }

    /// The next page of a previous search: the cursor, plus the `view` and
    /// `$select` it is bound to.
    #[must_use]
    pub fn next_page(previous: &Self, cursor: impl Into<String>) -> Self {
        Self {
            filter: previous.filter.clone(),
            order_by: None,
            select: previous.select.clone(),
            limit: previous.limit,
            cursor: Some(cursor.into()),
            view: previous.view,
        }
    }
}

/// An `OData` string literal: `value` in single quotes, inner quotes doubled.
fn odata_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Query of `GET /tasks/count`: the filter only.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
    utoipa::ToSchema,
    toolkit::QueryParams,
)]
#[non_exhaustive]
pub struct CountTasksQuery {
    /// `OData` filter, as for search.
    #[serde(rename = "$filter")]
    pub filter: Option<String>,
}

impl CountTasksQuery {
    /// Count with a filter.
    #[must_use]
    pub fn new(filter: Option<String>) -> Self {
        Self { filter }
    }
}

/// Query of a per-task or per-queue list (entries, wait conditions, task and
/// queue history): filter and paging. Sort is fixed (oldest first, then `id`);
/// maximum page size 100.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
    utoipa::ToSchema,
    toolkit::QueryParams,
)]
#[non_exhaustive]
pub struct PageQuery {
    /// `OData` filter; the allowed fields are per operation.
    #[serde(rename = "$filter")]
    pub filter: Option<String>,
    /// Page size (default 25, maximum 100).
    pub limit: Option<u32>,
    /// `page_info.next_cursor` of the previous page.
    pub cursor: Option<String>,
}

impl PageQuery {
    /// Build a page query.
    #[must_use]
    pub const fn new(filter: Option<String>, limit: Option<u32>, cursor: Option<String>) -> Self {
        Self {
            filter,
            limit,
            cursor,
        }
    }
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

/// A task narrowed by `view` and `$select`. Each field says the first view that
/// returns it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct TaskSearchItem {
    /// Task ID.
    pub id: Uuid,
    /// State.
    pub state: TaskState,
    /// Task type.
    #[serde(rename = "type")]
    #[schema(value_type = String)]
    pub task_type: GtsTypeId,
    /// From `short`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgency: Option<Urgency>,
    /// From `short`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u16>,
    /// From `short`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub queue: Option<GtsInstanceId>,
    /// From `short`; `None` when not leased (or not selected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment: Option<Assignment>,
    /// From `short`: source queue of a dead-lettered task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub dead_lettered_from: Option<GtsInstanceId>,
    /// From `short`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    /// From `short`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    /// From `long`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    /// From `long`; `None` without a result (or not selected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskResult>,
    /// From `long`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    /// From `long`; `None` without progress (or not selected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<ProgressInfo>,
    /// From `long`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<Tag>>,
    /// From `long`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_refs: Option<Vec<ResourceRef>>,
    /// `full` only: the newest 50 events, newest first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<HistoryEvent>>,
    /// `full` only: the conditions of the current wait group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_conditions: Option<Vec<WaitCondition>>,
    /// `full` only: the newest 20 entries, newest first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<Entry>>,
    /// `full` only: all predecessors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<Predecessor>>,
    /// `full` only: `true` when the task has more than 50 events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events_truncated: Option<bool>,
    /// `full` only: `true` when the task has more than 20 entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries_truncated: Option<bool>,
}

impl TaskSearchItem {
    /// Build a `tiny` item.
    #[must_use]
    pub const fn new(id: Uuid, state: TaskState, task_type: GtsTypeId) -> Self {
        Self {
            id,
            state,
            task_type,
            urgency: None,
            priority: None,
            queue: None,
            assignment: None,
            dead_lettered_from: None,
            created_at: None,
            updated_at: None,
            input: None,
            result: None,
            context: None,
            progress: None,
            tags: None,
            resource_refs: None,
            events: None,
            wait_conditions: None,
            entries: None,
            dependencies: None,
            events_truncated: None,
            entries_truncated: None,
        }
    }
}

/// One page of tasks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct TaskSearchPage {
    /// The tasks.
    pub items: Vec<TaskSearchItem>,
    /// Paging cursors.
    pub page_info: PageInfo,
}

impl TaskSearchPage {
    /// Build a page.
    #[must_use]
    pub const fn new(items: Vec<TaskSearchItem>, page_info: PageInfo) -> Self {
        Self { items, page_info }
    }
}

/// Capped count of matching tasks.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[non_exhaustive]
pub struct TaskCount {
    /// Matching tasks, at most `search_max_count`.
    pub count: u64,
    /// `true` when more than `search_max_count` tasks match.
    pub capped: bool,
}

impl TaskCount {
    /// Build a count.
    #[must_use]
    pub const fn new(count: u64, capped: bool) -> Self {
        Self { count, capped }
    }
}

/// One entry of a task's history. Never carries a lease token, the input, the
/// context or the output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct HistoryEvent {
    /// Event ID.
    pub id: Uuid,
    /// camelCase lifecycle event name, an open set: `taskCreated`,
    /// `taskQueued`, `taskClaimed`, `taskStarted`, `taskProgressed`,
    /// `taskCheckpointed`, `taskWaiting`, `waitConditionSet`,
    /// `waitConditionSatisfied`, `waitTimedOut`, `taskResumed`,
    /// `taskRetrying` (with the full `error` of the failed attempt),
    /// `taskReassigned`, `taskCancelRequested`, `taskCancelled`,
    /// `taskCompleted`, `taskFailed`, `taskTimedOut`, `taskDeadLettered`,
    /// `slaBreached`, `taskTagsUpdated`, `taskUpdated`.
    pub event_type: String,
    /// Subject that caused the event; `None` for engine-caused events.
    #[serde(default)]
    pub actor: Option<String>,
    /// When it happened.
    pub timestamp: DateTime<Utc>,
    /// The `data` member of the matching broker event type
    /// (`gts.cf.core.events.event.v1~cf.core.tasks.<snake_name>.v1~`).
    pub payload: serde_json::Value,
}

impl HistoryEvent {
    /// Build a history event.
    #[must_use]
    pub fn new(
        id: Uuid,
        event_type: impl Into<String>,
        timestamp: DateTime<Utc>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            id,
            event_type: event_type.into(),
            actor: None,
            timestamp,
            payload,
        }
    }
}

/// One page of history events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct HistoryEventPage {
    /// The events, oldest first.
    pub items: Vec<HistoryEvent>,
    /// Paging cursors.
    pub page_info: PageInfo,
}

impl HistoryEventPage {
    /// Build a page.
    #[must_use]
    pub const fn new(items: Vec<HistoryEvent>, page_info: PageInfo) -> Self {
        Self { items, page_info }
    }
}

/// One queue-scoped event (`queueHeld`, `queueReleased`, `configUpdated`,
/// `workerBlacklisted`): same item shape as a task's [`HistoryEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct QueueHistoryEvent {
    /// Event ID.
    pub id: Uuid,
    /// camelCase event name, an open set: `queueHeld`, `queueReleased`,
    /// `configUpdated`, `workerBlacklisted`.
    pub event_type: String,
    /// Subject that caused the event; `None` for an automatic release.
    #[serde(default)]
    pub actor: Option<String>,
    /// When it happened.
    pub timestamp: DateTime<Utc>,
    /// The event data; its shape is chosen by `event_type`:
    /// [`QueueHeldData`], [`QueueReleasedData`], or the `data` member of the
    /// `config_updated` / `worker_blacklisted` broker event types. Decode it
    /// with `serde_json::from_value` after matching `event_type`.
    pub payload: serde_json::Value,
}

impl QueueHistoryEvent {
    /// Build a queue history event.
    #[must_use]
    pub fn new(
        id: Uuid,
        event_type: impl Into<String>,
        timestamp: DateTime<Utc>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            id,
            event_type: event_type.into(),
            actor: None,
            timestamp,
            payload,
        }
    }
}

/// Payload of a `queueHeld` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct QueueHeldData {
    /// The held queue.
    #[schema(value_type = String)]
    pub queue: GtsInstanceId,
    /// Subject that held it.
    pub actor: String,
    /// Reason.
    #[serde(default)]
    pub reason: Option<String>,
    /// Automatic release time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
}

impl QueueHeldData {
    /// Build a `queueHeld` payload.
    #[must_use]
    pub fn new(queue: GtsInstanceId, actor: impl Into<String>) -> Self {
        Self {
            queue,
            actor: actor.into(),
            reason: None,
            until: None,
        }
    }
}

/// Payload of a `queueReleased` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct QueueReleasedData {
    /// The released queue.
    #[schema(value_type = String)]
    pub queue: GtsInstanceId,
    /// Subject that released it; absent for an automatic release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// `true` when `until` ended the hold.
    pub automatic: bool,
}

impl QueueReleasedData {
    /// Build a `queueReleased` payload.
    #[must_use]
    pub const fn new(queue: GtsInstanceId, actor: Option<String>, automatic: bool) -> Self {
        Self {
            queue,
            actor,
            automatic,
        }
    }
}

/// One page of queue history events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct QueueHistoryEventPage {
    /// The events, oldest first.
    pub items: Vec<QueueHistoryEvent>,
    /// Paging cursors.
    pub page_info: PageInfo,
}

impl QueueHistoryEventPage {
    /// Build a page.
    #[must_use]
    pub const fn new(items: Vec<QueueHistoryEvent>, page_info: PageInfo) -> Self {
        Self { items, page_info }
    }
}
