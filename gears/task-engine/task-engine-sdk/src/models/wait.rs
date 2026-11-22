//! Durable waits: condition specs, stored conditions, wait groups and results.

use chrono::{DateTime, Utc};
use gts::{GtsInstanceId, GtsTypeId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::{
    OnTimeout, PageInfo, TaskState, WaitConditionKind, WaitConditionState, WaitMode,
};

// ---------------------------------------------------------------------------
// Condition specs (requests)
// ---------------------------------------------------------------------------

/// One condition of a set-wait request or an initial wait group, by `kind`.
///
/// Phases: `timer` p1; `input`, `children` and `event` p2, refused in p1 with
/// `FEATURE_NOT_AVAILABLE` on `/conditions/i/kind`.
///
/// Timeouts: the input, children and event kinds may carry `timeout_secs` and
/// `on_timeout`; the timeout starts at park time (at create for an initial
/// wait group). `timeout_secs` without `on_timeout` means `fail`. For input and
/// children conditions `on_timeout` without `timeout_secs` → `400`
/// `NOT_ALLOWED`; an event condition without `timeout_secs` gets
/// `event_waits.max_wait_secs`, so `on_timeout` alone is valid there. A timer
/// condition takes no timeout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum WaitConditionSpec {
    /// Satisfied once `resume_at` passes. Takes no timeout (`timeout_secs` or
    /// `on_timeout` → `400` `NOT_ALLOWED`). p1.
    Timer {
        /// Satisfied when reached. A time already past is satisfied at once (a
        /// set answers `parked: false` with the result).
        resume_at: DateTime<Utc>,
    },
    /// Satisfied by a resolve call with a value valid against `input_schema` (p2).
    Input {
        /// JSON Schema or GTS type reference the value must match. Untrusted:
        /// refused if too large or deep, with a remote `$ref`, or a non-linear
        /// regex.
        input_schema: serde_json::Value,
        /// Prompt shown to the resolver.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        /// Timeout in seconds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
        /// What the timeout does.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_timeout: Option<OnTimeout>,
    },
    /// Satisfied when the task has no unfinished children (p2).
    Children {
        /// Timeout in seconds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
        /// What the timeout does.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_timeout: Option<OnTimeout>,
    },
    /// Satisfied by a matching broker event (p2). Needs the caller's own Event
    /// Broker `consume` permission; timeout at most `event_waits.max_wait_secs`
    /// (7 days), which also applies when none is given.
    Event {
        /// GTS event type ID on a waitable topic, resolved with the caller's GTS
        /// visibility (unregistered or hidden → `UNKNOWN_EVENT_TYPE`).
        #[schema(value_type = String)]
        event_type: GtsTypeId,
        /// Required unless the event type's `allowed_subject_types` is a single
        /// entry that is not a wildcard pattern (`EVENT_SUBJECT_TYPE_REQUIRED`);
        /// a type outside the allowed ones → `EVENT_SUBJECT_TYPE_NOT_ALLOWED`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(value_type = Option<String>)]
        subject_type: Option<GtsTypeId>,
        /// Subject to match.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subject_id: Option<String>,
        /// CEL expression checked after the field match (p3); a parse error or a
        /// breach of a CEL limit → `INVALID_MATCH_EXPRESSION`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        match_expression: Option<String>,
        /// Timeout in seconds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
        /// What the timeout does.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_timeout: Option<OnTimeout>,
    },
}

impl WaitConditionSpec {
    /// A timer condition.
    #[must_use]
    pub const fn timer(resume_at: DateTime<Utc>) -> Self {
        Self::Timer { resume_at }
    }
}

/// Initial wait group on create: the task starts `waiting`. Same shape as a
/// worker's set-wait without `lease_token`, `park` and `checkpoint`.
///
/// Not allowed with `predecessors` or a future `run_after` (`NOT_ALLOWED` on
/// `/wait`). Never a `children` condition (`NOT_ALLOWED`, every phase); p1
/// accepts only `timer`. Condition timeouts and `resume_at` count from the
/// create. A deferred admission (`429` `queue_depth.deferred` when full).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct InitialWaitGroup {
    /// Group mode.
    pub mode: WaitMode,
    /// 1 to `wait.max_conditions_per_group` (16) conditions.
    pub conditions: Vec<WaitConditionSpec>,
}

impl InitialWaitGroup {
    /// Build an initial wait group.
    #[must_use]
    pub const fn new(mode: WaitMode, conditions: Vec<WaitConditionSpec>) -> Self {
        Self { mode, conditions }
    }
}

/// Set (arm or park) the task's wait group. Worker only, lease-fenced, from
/// `claimed` or `running`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SetWaitRequest {
    /// The caller's lease token (missing → `400` `REQUIRED` before any lookup).
    pub lease_token: Uuid,
    /// Group mode.
    pub mode: WaitMode,
    /// `false` arms the group (it matches while the task keeps running);
    /// default `true` parks the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub park: Option<bool>,
    /// Merged into `context` (RFC 7386 JSON Merge Patch) in the same guarded
    /// write; records `taskCheckpointed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub checkpoint: Option<serde_json::Map<String, serde_json::Value>>,
    /// 1 to `wait.max_conditions_per_group` (16) conditions: empty →
    /// `REQUIRED`, more → `TOO_MANY`, both on `/conditions`.
    pub conditions: Vec<WaitConditionSpec>,
}

impl SetWaitRequest {
    /// Build a set-wait request that parks the task.
    #[must_use]
    pub const fn new(
        lease_token: Uuid,
        mode: WaitMode,
        conditions: Vec<WaitConditionSpec>,
    ) -> Self {
        Self {
            lease_token,
            mode,
            park: None,
            checkpoint: None,
            conditions,
        }
    }
}

/// Park the armed wait group. Worker only, lease-fenced. With nothing armed →
/// `400` `failed_precondition` `NO_ARMED_WAIT`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ParkRequest {
    /// The caller's lease token.
    pub lease_token: Uuid,
    /// Merged into `context` (RFC 7386 JSON Merge Patch) in the same write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub checkpoint: Option<serde_json::Map<String, serde_json::Value>>,
}

impl ParkRequest {
    /// Build a park request.
    #[must_use]
    pub const fn new(lease_token: Uuid) -> Self {
        Self {
            lease_token,
            checkpoint: None,
        }
    }
}

/// Provide the value of a pending input condition (p2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ResolveRequest {
    /// Must be valid against the condition's `input_schema`. Measured alone
    /// against the 64 KiB payload limit (`413`); stored on the condition, not
    /// in the task's payload.
    pub value: serde_json::Value,
}

impl ResolveRequest {
    /// Build a resolve request.
    #[must_use]
    pub const fn new(value: serde_json::Value) -> Self {
        Self { value }
    }
}

// ---------------------------------------------------------------------------
// Stored conditions
// ---------------------------------------------------------------------------

/// Per-kind fields of a stored wait condition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum WaitConditionDetails {
    /// Timer condition.
    Timer {
        /// When it is satisfied.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume_at: Option<DateTime<Utc>>,
    },
    /// Input condition (p2).
    Input {
        /// The schema the value must match.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_schema: Option<serde_json::Value>,
        /// Prompt shown to the resolver.
        #[serde(default)]
        prompt: Option<String>,
    },
    /// Children condition (p2).
    Children {},
    /// Event condition (p2).
    Event {
        /// GTS event type ID.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(value_type = Option<String>)]
        event_type: Option<GtsTypeId>,
        /// Effective subject type (explicit or the event type's single allowed one).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(value_type = Option<String>)]
        subject_type: Option<GtsTypeId>,
        /// Subject to match.
        #[serde(default)]
        subject_id: Option<String>,
        /// CEL expression (p3).
        #[serde(default)]
        match_expression: Option<String>,
    },
    /// A kind this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

impl WaitConditionDetails {
    /// The condition kind.
    #[must_use]
    pub const fn kind(&self) -> WaitConditionKind {
        match self {
            Self::Timer { .. } => WaitConditionKind::Timer,
            Self::Input { .. } => WaitConditionKind::Input,
            Self::Children {} => WaitConditionKind::Children,
            Self::Event { .. } => WaitConditionKind::Event,
            Self::Unknown => WaitConditionKind::Unknown,
        }
    }
}

/// A stored wait condition: the fields every condition has, plus the per-kind
/// fields in [`details`](Self::details) (which carries `kind` on the wire).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct WaitCondition {
    /// Condition ID.
    pub id: Uuid,
    /// The wait group the condition belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_group_id: Option<Uuid>,
    /// Condition state.
    pub state: WaitConditionState,
    /// Timeout in seconds.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// What the timeout does.
    #[serde(default)]
    pub on_timeout: Option<OnTimeout>,
    /// When the timeout fires.
    #[serde(default)]
    pub timeout_at: Option<DateTime<Utc>>,
    /// When the group was parked; `None` while armed.
    #[serde(default)]
    pub parked_at: Option<DateTime<Utc>>,
    /// Creation time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    /// When the condition was resolved.
    #[serde(default)]
    pub resolved_at: Option<DateTime<Utc>>,
    /// Subject that resolved an input condition.
    #[serde(default)]
    pub resolved_by: Option<String>,
    /// Stored outcome; `None` while pending or when cancelled.
    #[serde(default)]
    pub result: Option<ConditionResult>,
    /// Per-kind fields, including `kind`.
    #[serde(flatten)]
    pub details: WaitConditionDetails,
}

impl WaitCondition {
    /// Build a stored condition.
    #[must_use]
    pub const fn new(id: Uuid, state: WaitConditionState, details: WaitConditionDetails) -> Self {
        Self {
            id,
            wait_group_id: None,
            state,
            timeout_secs: None,
            on_timeout: None,
            timeout_at: None,
            parked_at: None,
            created_at: None,
            resolved_at: None,
            resolved_by: None,
            result: None,
            details,
        }
    }

    /// The condition kind.
    #[must_use]
    pub const fn kind(&self) -> WaitConditionKind {
        self.details.kind()
    }
}

/// A task's wait group as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct WaitGroup {
    /// Group ID.
    pub id: Uuid,
    /// Group mode.
    pub mode: WaitMode,
    /// Whether the task is parked on it (`false` while armed).
    pub parked: bool,
    /// The group's conditions.
    pub conditions: Vec<WaitCondition>,
}

impl WaitGroup {
    /// Build a wait group.
    #[must_use]
    pub const fn new(
        id: Uuid,
        mode: WaitMode,
        parked: bool,
        conditions: Vec<WaitCondition>,
    ) -> Self {
        Self {
            id,
            mode,
            parked,
            conditions,
        }
    }
}

/// Response of set-wait when the group was stored: the group, and the task
/// state. See [`SetWaitOutcome`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct SetWaitResponse {
    /// The stored group.
    pub wait_group: WaitGroup,
    /// Task state after the call (`waiting` when parked, else `running`).
    pub task_state: TaskState,
}

impl SetWaitResponse {
    /// Build a set-wait response.
    #[must_use]
    pub const fn new(wait_group: WaitGroup, task_state: TaskState) -> Self {
        Self {
            wait_group,
            task_state,
        }
    }
}

/// The `200` body of set-wait: `oneOf` [`SetWaitResponse`] /
/// [`ParkResponse`].
///
/// A group that is satisfied at once (a timer whose `resume_at` already
/// passed) is not parked: the call answers `parked: false` with the `result`,
/// the lease is kept and the task stays as it was. The group is consumed: its
/// conditions stay listed as `satisfied`, but it is no longer the task's
/// current group and no claim returns it.
///
/// Untagged on the wire: a body with `wait_group` decodes as
/// [`Stored`](Self::Stored), anything else as [`Satisfied`](Self::Satisfied).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(untagged)]
#[non_exhaustive]
pub enum SetWaitOutcome {
    /// The group was stored: parked (`task_state: waiting`) or armed.
    Stored(SetWaitResponse),
    /// The group was satisfied at once (`parked: false` with `result`).
    Satisfied(ParkResponse),
}

impl SetWaitOutcome {
    /// The stored group, unless it was satisfied at once.
    #[must_use]
    pub const fn stored(&self) -> Option<&SetWaitResponse> {
        match self {
            Self::Stored(stored) => Some(stored),
            Self::Satisfied(_) => None,
        }
    }

    /// The wait result, when the group was satisfied at once.
    #[must_use]
    pub const fn satisfied_result(&self) -> Option<&WaitResult> {
        match self {
            Self::Stored(_) => None,
            Self::Satisfied(park) => park.result.as_ref(),
        }
    }
}

impl From<SetWaitResponse> for SetWaitOutcome {
    fn from(stored: SetWaitResponse) -> Self {
        Self::Stored(stored)
    }
}

impl From<ParkResponse> for SetWaitOutcome {
    fn from(park: ParkResponse) -> Self {
        Self::Satisfied(park)
    }
}

/// One page of wait conditions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct WaitConditionPage {
    /// The conditions.
    pub items: Vec<WaitCondition>,
    /// Paging cursors.
    pub page_info: PageInfo,
}

impl WaitConditionPage {
    /// Build a page.
    #[must_use]
    pub const fn new(items: Vec<WaitCondition>, page_info: PageInfo) -> Self {
        Self { items, page_info }
    }
}

/// Response of resolve: the condition, `satisfied`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct ResolveResponse {
    /// The resolved condition.
    pub condition: WaitCondition,
}

impl ResolveResponse {
    /// Build a resolve response.
    #[must_use]
    pub const fn new(condition: WaitCondition) -> Self {
        Self { condition }
    }
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

/// Outcome of a wait group.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WaitOutcome {
    /// The group was satisfied.
    Satisfied,
    /// A condition timed out with `on_timeout: resume`.
    TimedOut,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// Why a condition timed out.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TimedOutReason {
    /// Its timeout elapsed.
    Timeout,
    /// The match-expression evaluation budget ran out.
    EvalBudgetExceeded,
    /// A value this SDK version does not know (the set is open).
    #[serde(other)]
    Unknown,
}

/// Input result: the provided value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct InputWaitResult {
    /// The provided input value.
    pub value: serde_json::Value,
}

/// Header of the matched broker event — never its data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct WaitEventHeader {
    /// Event ID.
    pub id: String,
    /// Event type ID.
    #[serde(rename = "type")]
    #[schema(value_type = String)]
    pub event_type: GtsTypeId,
    /// Event subject.
    #[serde(default)]
    pub subject: Option<String>,
    /// Subject type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub subject_type: Option<GtsTypeId>,
    /// Event time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<DateTime<Utc>>,
    /// Topic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub topic: Option<GtsInstanceId>,
    /// Partition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<u32>,
    /// Offset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
}

/// Event result: the matched event's header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct EventWaitResult {
    /// The matched event.
    pub event: WaitEventHeader,
}

/// Number of children per terminal state.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[non_exhaustive]
pub struct ChildrenCounts {
    /// Children `completed`.
    #[serde(default)]
    pub completed: u64,
    /// Children `failed`.
    #[serde(default)]
    pub failed: u64,
    /// Children `cancelled`.
    #[serde(default)]
    pub cancelled: u64,
    /// Children `dead_lettered`.
    #[serde(default)]
    pub dead_lettered: u64,
}

/// Children result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct ChildrenWaitResult {
    /// Children per terminal state.
    pub counts: ChildrenCounts,
}

/// Timer result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct TimerWaitResult {
    /// When the timer fired.
    pub resume_at: DateTime<Utc>,
}

/// Timed-out result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct TimedOutWaitResult {
    /// Always `true`.
    pub timed_out: bool,
    /// Why it timed out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<TimedOutReason>,
}

/// The stored outcome of one condition, by the shape of its fields.
///
/// Variants are tried in order; each has a distinct required key
/// (`timed_out`, `value`, `event`, `counts`, `resume_at`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(untagged)]
#[non_exhaustive]
pub enum ConditionResult {
    /// The condition timed out.
    TimedOut(TimedOutWaitResult),
    /// Input value.
    Input(InputWaitResult),
    /// Matched event header.
    Event(EventWaitResult),
    /// Children counts.
    Children(ChildrenWaitResult),
    /// Timer fire time.
    Timer(TimerWaitResult),
}

/// One condition inside a [`WaitResult`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct WaitResultCondition {
    /// Condition ID.
    pub id: Uuid,
    /// Condition kind.
    pub kind: WaitConditionKind,
    /// Condition state.
    pub state: WaitConditionState,
    /// Per-kind outcome; `None` when cancelled.
    #[serde(default)]
    pub result: Option<ConditionResult>,
}

impl WaitResultCondition {
    /// Build a result condition.
    #[must_use]
    pub const fn new(
        id: Uuid,
        kind: WaitConditionKind,
        state: WaitConditionState,
        result: Option<ConditionResult>,
    ) -> Self {
        Self {
            id,
            kind,
            state,
            result,
        }
    }
}

/// The outcome of a wait group, given to the resumed worker (claim `resumed`)
/// or by a park that found the group already satisfied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct WaitResult {
    /// The group.
    pub wait_group_id: Uuid,
    /// Group outcome.
    pub outcome: WaitOutcome,
    /// Per-condition outcomes.
    pub conditions: Vec<WaitResultCondition>,
}

impl WaitResult {
    /// Build a wait result.
    #[must_use]
    pub const fn new(
        wait_group_id: Uuid,
        outcome: WaitOutcome,
        conditions: Vec<WaitResultCondition>,
    ) -> Self {
        Self {
            wait_group_id,
            outcome,
            conditions,
        }
    }
}

/// Response of park: `parked: false` with `result` when already satisfied,
/// else `parked: true` with `task_state: waiting`. Also the shape of a set-wait
/// whose group was satisfied at once ([`SetWaitOutcome::Satisfied`]).
///
/// Already satisfied means an armed timer whose `resume_at` has passed, an
/// armed input already resolved, or an armed children condition with no
/// unfinished children; the group is then consumed (its conditions stay listed
/// as `satisfied`, but it is no longer current and no claim returns it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct ParkResponse {
    /// Whether the task was parked.
    pub parked: bool,
    /// `waiting` when parked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_state: Option<TaskState>,
    /// Present when `parked` is `false` (already satisfied).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<WaitResult>,
}

impl ParkResponse {
    /// The task was parked.
    #[must_use]
    pub const fn parked() -> Self {
        Self {
            parked: true,
            task_state: Some(TaskState::Waiting),
            result: None,
        }
    }

    /// The group was already satisfied; the task keeps running.
    #[must_use]
    pub const fn satisfied(result: WaitResult) -> Self {
        Self {
            parked: false,
            task_state: Some(TaskState::Running),
            result: Some(result),
        }
    }
}
