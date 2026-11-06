//! REST projection of [`TaskEngineApi`] — the paths of
//! `gears/task-engine/docs/openapi.yaml` under `/task_engine/v1` (the gateway
//! adds `/api`).
//!
//! With `rest-client` the macro emits `TaskEngineApiRestClient` (and its
//! directory-resolving wrapper), which implements [`TaskEngineApi`] over HTTP.
//! With `rest-server` it emits route registration for every method not marked
//! `#[server_manual]`.
//!
//! **`#[server_manual]` methods.** A generated route always answers `200` with
//! a JSON body and cannot set response headers. The operations whose wire
//! contract needs more are registered by hand in the gear with
//! `OperationBuilder`; the generated client still covers them (it accepts any
//! `2xx`, and decodes an empty body as `None` / `()`):
//!
//! | method                   | why                                              |
//! |--------------------------|--------------------------------------------------|
//! | `create_task`            | `201` + `Location`; `Idempotency-Key` header     |
//! | `cancel_task`            | `200` or `202`                                   |
//! | `claim_tasks`            | `200` or `204` (nothing claimed)                 |
//! | `report_progress`        | `200` with an empty body                         |
//! | `create_entry`           | `201` + `Location`                               |
//! | `resolve_wait_condition` | `Idempotency-Replayed` response header           |
//! | `save_runtime_config`    | `202`                                            |
//!
//! **Known wire gap.** `Idempotency-Key` is a request header in the document,
//! but the macro cannot bind headers yet, so the generated client sends
//! [`NewTask::idempotency_key`](crate::models::NewTask::idempotency_key) in the
//! body. See [`NewTask`].

use gts::GtsInstanceId;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::TaskEngineApi;
use crate::models::{
    AcceptRequest, AcceptResponse, BulkCancelRequest, BulkMoveRequest, BulkReprioritizeRequest,
    BulkResult, BulkTaskIdsRequest, CancelRequest, CheckpointRequest, CheckpointResponse,
    ClaimRequest, ClaimResponse, CompleteRequest, CountTasksQuery, CreateEntryRequest, Entry,
    EntryPage, HeartbeatRequest, HeartbeatResponse, HistoryEventPage, HoldRequest,
    LeaseTokenRequest, NewTask, PageQuery, ParkRequest, ParkResponse, ProgressRequest, Queue,
    QueueList, ReassignRequest, ReleaseRequest, ResolveRequest, ResolveResponse,
    RuntimeConfigAccepted, RuntimeConfigRequest, RuntimeConfigStatus, SearchTasksQuery,
    SetWaitRequest, SetWaitResponse, TagsPatchRequest, Task, TaskCount, TaskSearchPage,
    WaitConditionPage,
};

/// HTTP projection of [`TaskEngineApi`].
#[toolkit::rest_contract(
    base_path = "/task_engine/v1",
    require_full_coverage,
    visibility = "exposed"
)]
pub trait TaskEngineApiRest: TaskEngineApi {
    // ------------------------------------------------------------- tasks

    #[post("/tasks")]
    #[server_manual]
    async fn create_task(
        &self,
        ctx: SecurityContext,
        task: NewTask,
    ) -> Result<Task, CanonicalError>;

    #[get("/tasks")]
    #[retryable]
    async fn search_tasks(
        &self,
        ctx: SecurityContext,
        query: SearchTasksQuery,
    ) -> Result<TaskSearchPage, CanonicalError>;

    #[get("/tasks/count")]
    #[retryable]
    async fn count_tasks(
        &self,
        ctx: SecurityContext,
        query: CountTasksQuery,
    ) -> Result<TaskCount, CanonicalError>;

    #[get("/tasks/{task_id}")]
    #[retryable]
    async fn get_task(&self, ctx: SecurityContext, task_id: Uuid) -> Result<Task, CanonicalError>;

    #[post("/tasks/{task_id}/cancel")]
    #[server_manual]
    async fn cancel_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: CancelRequest,
    ) -> Result<Task, CanonicalError>;

    #[patch("/tasks/{task_id}/tags")]
    #[retryable]
    async fn update_task_tags(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        patch: TagsPatchRequest,
    ) -> Result<Task, CanonicalError>;

    // ------------------------------------------------------------- worker

    #[post("/tasks/claim")]
    #[server_manual]
    async fn claim_tasks(
        &self,
        ctx: SecurityContext,
        req: ClaimRequest,
    ) -> Result<Option<ClaimResponse>, CanonicalError>;

    #[post("/tasks/heartbeat")]
    #[retryable]
    async fn heartbeat_tasks(
        &self,
        ctx: SecurityContext,
        req: HeartbeatRequest,
    ) -> Result<HeartbeatResponse, CanonicalError>;

    #[post("/tasks/{task_id}/start")]
    async fn start_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: LeaseTokenRequest,
    ) -> Result<Task, CanonicalError>;

    #[post("/tasks/{task_id}/progress")]
    #[retryable]
    #[server_manual]
    async fn report_progress(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: ProgressRequest,
    ) -> Result<(), CanonicalError>;

    #[post("/tasks/{task_id}/checkpoint")]
    #[retryable]
    async fn save_checkpoint(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: CheckpointRequest,
    ) -> Result<CheckpointResponse, CanonicalError>;

    #[post("/tasks/{task_id}/complete")]
    #[retryable]
    async fn complete_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: CompleteRequest,
    ) -> Result<Task, CanonicalError>;

    #[post("/tasks/{task_id}/release")]
    async fn release_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: ReleaseRequest,
    ) -> Result<Task, CanonicalError>;

    #[post("/tasks/{task_id}/accept")]
    async fn accept_reassigned_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: AcceptRequest,
    ) -> Result<AcceptResponse, CanonicalError>;

    // ------------------------------------------------------------- entries

    #[post("/tasks/{task_id}/entries")]
    #[server_manual]
    async fn create_entry(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: CreateEntryRequest,
    ) -> Result<Entry, CanonicalError>;

    #[get("/tasks/{task_id}/entries")]
    #[retryable]
    async fn list_entries(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        query: PageQuery,
    ) -> Result<EntryPage, CanonicalError>;

    #[get("/tasks/{task_id}/entries/{entry_id}")]
    #[retryable]
    async fn get_entry(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        entry_id: Uuid,
    ) -> Result<Entry, CanonicalError>;

    // ------------------------------------------------------------- waits

    #[post("/tasks/{task_id}/wait-conditions")]
    async fn set_wait_conditions(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: SetWaitRequest,
    ) -> Result<SetWaitResponse, CanonicalError>;

    #[get("/tasks/{task_id}/wait-conditions")]
    #[retryable]
    async fn list_wait_conditions(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        query: PageQuery,
    ) -> Result<WaitConditionPage, CanonicalError>;

    #[post("/tasks/{task_id}/wait-conditions/park")]
    async fn park_wait_group(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: ParkRequest,
    ) -> Result<ParkResponse, CanonicalError>;

    #[post("/wait-conditions/{condition_id}/resolve")]
    #[retryable]
    #[server_manual]
    async fn resolve_wait_condition(
        &self,
        ctx: SecurityContext,
        condition_id: Uuid,
        req: ResolveRequest,
    ) -> Result<ResolveResponse, CanonicalError>;

    // ------------------------------------------------------------- history

    #[get("/tasks/{task_id}/events")]
    #[retryable]
    async fn list_task_events(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        query: PageQuery,
    ) -> Result<HistoryEventPage, CanonicalError>;

    // ------------------------------------------------------------- queues

    #[get("/queues")]
    #[retryable]
    async fn list_queues(&self, ctx: SecurityContext) -> Result<QueueList, CanonicalError>;

    #[get("/queues/{queue_id}")]
    #[retryable]
    async fn get_queue(
        &self,
        ctx: SecurityContext,
        queue_id: GtsInstanceId,
    ) -> Result<Queue, CanonicalError>;

    // ------------------------------------------------------------- bulk

    #[post("/tasks/bulk/cancel")]
    async fn bulk_cancel(
        &self,
        ctx: SecurityContext,
        req: BulkCancelRequest,
    ) -> Result<BulkResult, CanonicalError>;

    #[post("/tasks/bulk/reprioritize")]
    #[retryable]
    async fn bulk_reprioritize(
        &self,
        ctx: SecurityContext,
        req: BulkReprioritizeRequest,
    ) -> Result<BulkResult, CanonicalError>;

    #[post("/tasks/bulk/retry")]
    async fn bulk_retry(
        &self,
        ctx: SecurityContext,
        req: BulkTaskIdsRequest,
    ) -> Result<BulkResult, CanonicalError>;

    #[post("/tasks/bulk/move")]
    #[retryable]
    async fn bulk_move(
        &self,
        ctx: SecurityContext,
        req: BulkMoveRequest,
    ) -> Result<BulkResult, CanonicalError>;

    // ------------------------------------------------------------- admin

    #[post("/tasks/{task_id}/reassign")]
    async fn reassign_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: ReassignRequest,
    ) -> Result<Task, CanonicalError>;

    #[post("/admin/tasks/{task_id}/reenqueue")]
    async fn reenqueue_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
    ) -> Result<Task, CanonicalError>;

    #[post("/admin/config")]
    #[server_manual]
    async fn save_runtime_config(
        &self,
        ctx: SecurityContext,
        req: RuntimeConfigRequest,
    ) -> Result<RuntimeConfigAccepted, CanonicalError>;

    #[get("/admin/config/{version}")]
    #[retryable]
    async fn get_runtime_config_status(
        &self,
        ctx: SecurityContext,
        version: u64,
    ) -> Result<RuntimeConfigStatus, CanonicalError>;

    #[post("/admin/queues/{queue_id}/hold")]
    #[retryable]
    async fn hold_queue(
        &self,
        ctx: SecurityContext,
        queue_id: GtsInstanceId,
        req: HoldRequest,
    ) -> Result<Queue, CanonicalError>;

    #[post("/admin/queues/{queue_id}/release")]
    #[retryable]
    async fn release_queue(
        &self,
        ctx: SecurityContext,
        queue_id: GtsInstanceId,
    ) -> Result<Queue, CanonicalError>;
}
