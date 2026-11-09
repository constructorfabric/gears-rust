//! Test support: a placeholder implementation of [`TaskEngineApi`].
//!
//! [`UnimplementedTaskEngine`] implements every method by failing with
//! `unimplemented` (HTTP `501`). It is a starting point for the gear's local
//! client and a stand-in for consumers and tests until the engine exists:
//! register it in `ClientHub`, then replace methods one by one.

use gts::GtsInstanceId;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::TaskEngineApi;
use crate::error::TaskResourceError;
use crate::models::{
    AcceptRequest, AcceptResponse, BulkCancelRequest, BulkMoveRequest, BulkReprioritizeRequest,
    BulkResult, BulkTaskIdsRequest, CancelRequest, CheckpointRequest, CheckpointResponse,
    ClaimRequest, ClaimResponse, CompleteRequest, CountTasksQuery, CreateEntryRequest,
    CreateTaskRequest, Entry, EntryPage, HeartbeatRequest, HeartbeatResponse, HistoryEventPage,
    HoldRequest, LeaseTokenRequest, PageQuery, ParkRequest, ParkResponse, ProgressRequest, Queue,
    QueueHistoryEventPage, QueueList, ReassignRequest, ReenqueueRequest, ReleaseRequest,
    ResolveRequest, ResolveResponse, RuntimeConfigAccepted, RuntimeConfigRequest,
    RuntimeConfigStatus, SearchTasksQuery, SetWaitOutcome, SetWaitRequest, TagsPatchRequest, Task,
    TaskCount, TaskSearchPage, WaitConditionPage,
};

/// A [`TaskEngineApi`] whose every method fails with `unimplemented`.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnimplementedTaskEngine;

/// The `unimplemented` error a placeholder method returns, naming the operation.
#[must_use]
pub fn not_implemented(operation: &str) -> CanonicalError {
    TaskResourceError::unimplemented(format!(
        "Task Engine operation `{operation}` is not implemented"
    ))
    .create()
}

#[async_trait::async_trait]
impl TaskEngineApi for UnimplementedTaskEngine {
    async fn create_task(
        &self,
        _ctx: SecurityContext,
        _task: CreateTaskRequest,
    ) -> Result<Task, CanonicalError> {
        Err(not_implemented("create_task"))
    }

    async fn search_tasks(
        &self,
        _ctx: SecurityContext,
        _query: SearchTasksQuery,
    ) -> Result<TaskSearchPage, CanonicalError> {
        Err(not_implemented("search_tasks"))
    }

    async fn count_tasks(
        &self,
        _ctx: SecurityContext,
        _query: CountTasksQuery,
    ) -> Result<TaskCount, CanonicalError> {
        Err(not_implemented("count_tasks"))
    }

    async fn get_task(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
    ) -> Result<Task, CanonicalError> {
        Err(not_implemented("get_task"))
    }

    async fn cancel_task(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
        _req: CancelRequest,
    ) -> Result<Task, CanonicalError> {
        Err(not_implemented("cancel_task"))
    }

    async fn update_task_tags(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
        _patch: TagsPatchRequest,
    ) -> Result<Task, CanonicalError> {
        Err(not_implemented("update_task_tags"))
    }

    async fn claim_tasks(
        &self,
        _ctx: SecurityContext,
        _req: ClaimRequest,
    ) -> Result<Option<ClaimResponse>, CanonicalError> {
        Err(not_implemented("claim_tasks"))
    }

    async fn heartbeat_tasks(
        &self,
        _ctx: SecurityContext,
        _req: HeartbeatRequest,
    ) -> Result<HeartbeatResponse, CanonicalError> {
        Err(not_implemented("heartbeat_tasks"))
    }

    async fn start_task(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
        _req: LeaseTokenRequest,
    ) -> Result<Task, CanonicalError> {
        Err(not_implemented("start_task"))
    }

    async fn report_progress(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
        _req: ProgressRequest,
    ) -> Result<(), CanonicalError> {
        Err(not_implemented("report_progress"))
    }

    async fn save_checkpoint(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
        _req: CheckpointRequest,
    ) -> Result<CheckpointResponse, CanonicalError> {
        Err(not_implemented("save_checkpoint"))
    }

    async fn complete_task(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
        _req: CompleteRequest,
    ) -> Result<Task, CanonicalError> {
        Err(not_implemented("complete_task"))
    }

    async fn release_task(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
        _req: ReleaseRequest,
    ) -> Result<Task, CanonicalError> {
        Err(not_implemented("release_task"))
    }

    async fn accept_reassigned_task(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
        _req: AcceptRequest,
    ) -> Result<AcceptResponse, CanonicalError> {
        Err(not_implemented("accept_reassigned_task"))
    }

    async fn create_entry(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
        _req: CreateEntryRequest,
    ) -> Result<Entry, CanonicalError> {
        Err(not_implemented("create_entry"))
    }

    async fn list_entries(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
        _query: PageQuery,
    ) -> Result<EntryPage, CanonicalError> {
        Err(not_implemented("list_entries"))
    }

    async fn get_entry(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
        _entry_id: Uuid,
    ) -> Result<Entry, CanonicalError> {
        Err(not_implemented("get_entry"))
    }

    async fn set_wait_conditions(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
        _req: SetWaitRequest,
    ) -> Result<SetWaitOutcome, CanonicalError> {
        Err(not_implemented("set_wait_conditions"))
    }

    async fn list_wait_conditions(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
        _query: PageQuery,
    ) -> Result<WaitConditionPage, CanonicalError> {
        Err(not_implemented("list_wait_conditions"))
    }

    async fn park_wait_group(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
        _req: ParkRequest,
    ) -> Result<ParkResponse, CanonicalError> {
        Err(not_implemented("park_wait_group"))
    }

    async fn resolve_wait_condition(
        &self,
        _ctx: SecurityContext,
        _condition_id: Uuid,
        _req: ResolveRequest,
    ) -> Result<ResolveResponse, CanonicalError> {
        Err(not_implemented("resolve_wait_condition"))
    }

    async fn list_task_events(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
        _query: PageQuery,
    ) -> Result<HistoryEventPage, CanonicalError> {
        Err(not_implemented("list_task_events"))
    }

    async fn list_queues(&self, _ctx: SecurityContext) -> Result<QueueList, CanonicalError> {
        Err(not_implemented("list_queues"))
    }

    async fn get_queue(
        &self,
        _ctx: SecurityContext,
        _queue_id: GtsInstanceId,
    ) -> Result<Queue, CanonicalError> {
        Err(not_implemented("get_queue"))
    }

    async fn list_queue_events(
        &self,
        _ctx: SecurityContext,
        _queue_id: GtsInstanceId,
        _query: PageQuery,
    ) -> Result<QueueHistoryEventPage, CanonicalError> {
        Err(not_implemented("list_queue_events"))
    }

    async fn bulk_cancel(
        &self,
        _ctx: SecurityContext,
        _req: BulkCancelRequest,
    ) -> Result<BulkResult, CanonicalError> {
        Err(not_implemented("bulk_cancel"))
    }

    async fn bulk_reprioritize(
        &self,
        _ctx: SecurityContext,
        _req: BulkReprioritizeRequest,
    ) -> Result<BulkResult, CanonicalError> {
        Err(not_implemented("bulk_reprioritize"))
    }

    async fn bulk_retry(
        &self,
        _ctx: SecurityContext,
        _req: BulkTaskIdsRequest,
    ) -> Result<BulkResult, CanonicalError> {
        Err(not_implemented("bulk_retry"))
    }

    async fn bulk_move(
        &self,
        _ctx: SecurityContext,
        _req: BulkMoveRequest,
    ) -> Result<BulkResult, CanonicalError> {
        Err(not_implemented("bulk_move"))
    }

    async fn reassign_task(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
        _req: ReassignRequest,
    ) -> Result<Task, CanonicalError> {
        Err(not_implemented("reassign_task"))
    }

    async fn reenqueue_task(
        &self,
        _ctx: SecurityContext,
        _task_id: Uuid,
        _req: ReenqueueRequest,
    ) -> Result<Task, CanonicalError> {
        Err(not_implemented("reenqueue_task"))
    }

    async fn save_runtime_config(
        &self,
        _ctx: SecurityContext,
        _req: RuntimeConfigRequest,
    ) -> Result<RuntimeConfigAccepted, CanonicalError> {
        Err(not_implemented("save_runtime_config"))
    }

    async fn get_runtime_config_status(
        &self,
        _ctx: SecurityContext,
        _version: u64,
    ) -> Result<RuntimeConfigStatus, CanonicalError> {
        Err(not_implemented("get_runtime_config_status"))
    }

    async fn hold_queue(
        &self,
        _ctx: SecurityContext,
        _queue_id: GtsInstanceId,
        _req: HoldRequest,
    ) -> Result<Queue, CanonicalError> {
        Err(not_implemented("hold_queue"))
    }

    async fn release_queue(
        &self,
        _ctx: SecurityContext,
        _queue_id: GtsInstanceId,
    ) -> Result<Queue, CanonicalError> {
        Err(not_implemented("release_queue"))
    }
}
