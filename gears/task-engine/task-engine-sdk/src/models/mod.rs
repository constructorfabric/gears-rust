//! Models of the [`TaskEngineApi`](crate::TaskEngineApi) contract.
//!
//! One Rust type per schema of `gears/task-engine/docs/openapi.yaml`, named
//! after it. Field
//! names and JSON shapes follow the document; `type` fields are spelled
//! `task_type` / `entry_type` / `event_type` in Rust.
//!
//! Conventions:
//! - Every public struct and enum is `#[non_exhaustive]`; build values with the
//!   `new(...)` constructors (required fields) and set optional fields through
//!   the public fields.
//! - Optional fields are `Option`/empty `Vec` and are omitted from the JSON
//!   when empty; nullable fields the document always returns are sent as `null`.
//! - Enums the engine sends are open sets with an `Unknown` variant (see
//!   [`common`]).
//! - GTS identifiers are typed: type IDs (ending in `~`: task, entry, event and
//!   resource types) are [`gts::GtsTypeId`], instance IDs (queues, topics) are
//!   [`gts::GtsInstanceId`]; both validate on deserialization and appear as
//!   `string` in the `OpenAPI` schema. Runtime-config keys stay `String` because
//!   they may be GTS patterns.

pub mod admin;
pub mod common;
pub mod entry;
pub mod queue;
pub mod search;
pub mod task;
pub mod wait;
pub mod worker;

pub use admin::{
    BulkCancelRequest, BulkMoveRequest, BulkOutcome, BulkReprioritizeRequest, BulkResult,
    BulkTaskIdsRequest, BulkTaskResult, DomainApplyStatus, QueueSettingsOverride, ReassignRequest,
    RuntimeConfigAccepted, RuntimeConfigRequest, RuntimeConfigStatus, TypeSettingsOverride,
};
pub use common::{
    CancelMode, DependencyMode, ErrorFault, OnPredecessorFailure, OnTimeout, OperatedBy, PageInfo,
    ProgressEvents, QueueKind, ResultCode, RetryPolicy, RetryStrategy, Tag, TaskState,
    TenantFairness, Timeouts, Urgency, WaitConditionKind, WaitConditionState, WaitMode,
    WorkerResultCode,
};
pub use entry::{CreateEntryRequest, Entry, EntryPage, EntryPayload};
pub use queue::{HoldRequest, Queue, QueueEffectiveSettings, QueueHold, QueueLimits, QueueList};
pub use search::{
    CountTasksQuery, HistoryEvent, HistoryEventPage, PageQuery, SearchTasksQuery, TaskCount,
    TaskOrderBy, TaskSearchItem, TaskSearchPage, TaskView,
};
pub use task::{
    Assignment, CancelRequest, CancelRequestInfo, CreateTaskRequest, Predecessor, PredecessorInput,
    ProgressInfo, ResourceRef, ResourceRefInput, TagsPatchRequest, Task, TaskError, TaskResult,
};
pub use wait::{
    ChildrenCounts, ChildrenWaitResult, ConditionResult, EventWaitResult, InitialWaitGroup,
    InputWaitResult, ParkRequest, ParkResponse, ResolveRequest, ResolveResponse, SetWaitRequest,
    SetWaitResponse, TimedOutReason, TimedOutWaitResult, TimerWaitResult, WaitCondition,
    WaitConditionDetails, WaitConditionPage, WaitConditionSpec, WaitEventHeader, WaitGroup,
    WaitOutcome, WaitResult, WaitResultCondition,
};
pub use worker::{
    AcceptRequest, AcceptResponse, Authority, AuthorityCheck, AuthorityMode, AuthorityVerdict,
    Checkpoint, CheckpointRequest, CheckpointResponse, Claim, ClaimRequest, ClaimResponse,
    CompleteRequest, HeartbeatLease, HeartbeatOutcome, HeartbeatRequest, HeartbeatResponse,
    HeartbeatResult, Lease, LeaseTokenRequest, ProgressRequest, ReleaseRequest,
};

// Marker impls so these DTOs are accepted by `toolkit::OperationBuilder` (the
// generated `rest-server` routes and the gear's hand-written `#[server_manual]`
// routes). Tag traits only; the serde/`ToSchema` derives carry the behaviour.
mod api_dto_markers {
    use toolkit::api::api_dto::{RequestApiDto, ResponseApiDto};

    use super::{
        AcceptRequest, AcceptResponse, BulkCancelRequest, BulkMoveRequest, BulkReprioritizeRequest,
        BulkResult, BulkTaskIdsRequest, CancelRequest, CheckpointRequest, CheckpointResponse,
        ClaimRequest, ClaimResponse, CompleteRequest, CreateEntryRequest, CreateTaskRequest, Entry,
        EntryPage, HeartbeatRequest, HeartbeatResponse, HistoryEventPage, HoldRequest,
        LeaseTokenRequest, ParkRequest, ParkResponse, ProgressRequest, Queue, QueueList,
        ReassignRequest, ReleaseRequest, ResolveRequest, ResolveResponse, RuntimeConfigAccepted,
        RuntimeConfigRequest, RuntimeConfigStatus, SetWaitRequest, SetWaitResponse,
        TagsPatchRequest, Task, TaskCount, TaskSearchPage, WaitConditionPage,
    };

    macro_rules! markers {
        ($marker:ident: $($ty:ty),+ $(,)?) => { $( impl $marker for $ty {} )+ };
    }

    markers!(RequestApiDto:
        CreateTaskRequest, CancelRequest, TagsPatchRequest, ClaimRequest, HeartbeatRequest,
        LeaseTokenRequest, ProgressRequest, CheckpointRequest, CompleteRequest, ReleaseRequest,
        AcceptRequest, CreateEntryRequest, SetWaitRequest, ParkRequest, ResolveRequest,
        BulkCancelRequest, BulkReprioritizeRequest, BulkTaskIdsRequest, BulkMoveRequest,
        ReassignRequest, RuntimeConfigRequest, HoldRequest,
    );
    markers!(ResponseApiDto:
        Task, TaskSearchPage, TaskCount, ClaimResponse, HeartbeatResponse, CheckpointResponse,
        AcceptResponse, Entry, EntryPage, SetWaitResponse, WaitConditionPage, ParkResponse,
        ResolveResponse, HistoryEventPage, QueueList, Queue, BulkResult, RuntimeConfigAccepted,
        RuntimeConfigStatus,
    );
}
