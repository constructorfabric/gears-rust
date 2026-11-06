//! `TaskEngineApi` contract and Contract IR.
//!
//! The Rust trait is the source of truth for the operation set of
//! `gears/task-engine/docs/openapi.yaml`: one method per `operationId`, in the
//! document's order. `#[toolkit::contract]` derives the Contract IR, the
//! static descriptor and the `Contract` impl. HTTP verbs, paths and status
//! codes live only in the REST projection ([`crate::rest`]).
//!
//! Authorization (the PDP action named on each method), phases (p1/p2/p3) and
//! the error categories are the document's; every method fails with a
//! [`CanonicalError`]. Requests about a task the caller may not `read` fail
//! exactly like a missing task (`not_found`).

use gts::GtsInstanceId;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::models::{
    AcceptRequest, AcceptResponse, BulkCancelRequest, BulkMoveRequest, BulkReprioritizeRequest,
    BulkResult, BulkTaskIdsRequest, CancelRequest, CheckpointRequest, CheckpointResponse,
    ClaimRequest, ClaimResponse, CompleteRequest, CountTasksQuery, CreateEntryRequest,
    CreateTaskRequest, Entry, EntryPage, HeartbeatRequest, HeartbeatResponse, HistoryEventPage,
    HoldRequest, LeaseTokenRequest, PageQuery, ParkRequest, ParkResponse, ProgressRequest, Queue,
    QueueList, ReassignRequest, ReleaseRequest, ResolveRequest, ResolveResponse,
    RuntimeConfigAccepted, RuntimeConfigRequest, RuntimeConfigStatus, SearchTasksQuery,
    SetWaitRequest, SetWaitResponse, TagsPatchRequest, Task, TaskCount, TaskSearchPage,
    WaitConditionPage,
};

/// Task Engine API — the same trait for in-process and remote consumption.
///
/// Registered in `ClientHub` as `Arc<dyn TaskEngineApi>`. All parameters are
/// owned and `'static`-compatible.
#[toolkit::contract(gear = "task-engine", version = "v1")]
pub trait TaskEngineApi: Send + Sync {
    // ------------------------------------------------------------- tasks

    /// Create a task (`createTask`). Action `create`; `critical` also needs
    /// `expedite`.
    ///
    /// Initial state: `blocked` with unmet predecessors, else `waiting` with an
    /// initial wait group, else `scheduled` with a future `run_after`, else
    /// `queued`. A replay of an equal request under the same idempotency key
    /// fails `already_exists` naming the existing task, so this is not retried
    /// automatically.
    ///
    /// # Errors
    /// `invalid_argument` (schema, unknown type/queue, trust limits,
    /// `FEATURE_NOT_AVAILABLE`, …; missing idempotency key), `permission_denied`,
    /// `not_found` (parent/predecessor), `already_exists`, `resource_exhausted`
    /// (`QUEUE_DEPTH_LIMIT`), `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn create_task(
        &self,
        ctx: SecurityContext,
        task: CreateTaskRequest,
    ) -> Result<Task, CanonicalError>;

    /// Keyset-paged search over the caller's visible tasks (`searchTasks`).
    /// Action `read`.
    ///
    /// # Errors
    /// `invalid_argument` (query options, window, cursor binding),
    /// `permission_denied` (no `read` grant at all), `service_unavailable`.
    #[idempotency(SafeRead)]
    async fn search_tasks(
        &self,
        ctx: SecurityContext,
        query: SearchTasksQuery,
    ) -> Result<TaskSearchPage, CanonicalError>;

    /// Capped count of the tasks a search with the same filter would return
    /// (`countTasks`). Action `read`.
    ///
    /// # Errors
    /// `invalid_argument`, `permission_denied`, `service_unavailable`.
    #[idempotency(SafeRead)]
    async fn count_tasks(
        &self,
        ctx: SecurityContext,
        query: CountTasksQuery,
    ) -> Result<TaskCount, CanonicalError>;

    /// Get a task by ID (`getTask`). Action `read`.
    ///
    /// # Errors
    /// `not_found`, `service_unavailable`.
    #[idempotency(SafeRead)]
    async fn get_task(&self, ctx: SecurityContext, task_id: Uuid) -> Result<Task, CanonicalError>;

    /// Cancel a task (`cancelTask`). Action `cancel`.
    ///
    /// An unleased task, or `forced`, ends at once (`cancelled`); `requested` /
    /// `graceful` on a leased task only sets `cancel_requested` (HTTP `202`).
    /// Either way the returned task shows which happened.
    ///
    /// # Errors
    /// `invalid_argument`, `failed_precondition` (`STATE`), `permission_denied`,
    /// `not_found`, `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn cancel_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: CancelRequest,
    ) -> Result<Task, CanonicalError>;

    /// Add, replace or remove tags (`updateTaskTags`). Action `update`. p2.
    ///
    /// # Errors
    /// `invalid_argument`, `permission_denied`, `not_found`, `service_unavailable`.
    #[idempotency(IdempotentWrite)]
    async fn update_task_tags(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        patch: TagsPatchRequest,
    ) -> Result<Task, CanonicalError>;

    // ------------------------------------------------------------- worker

    /// Claim up to `max_tasks` tasks, long-polling up to `timeout_secs`
    /// (`claimTasks`). Action `claim`.
    ///
    /// `Ok(None)` when nothing became eligible before the timeout (HTTP `204`).
    ///
    /// # Errors
    /// `invalid_argument`, `failed_precondition` (blacklisted, p3), `not_found`
    /// (unknown queue), `permission_denied`, `resource_exhausted`
    /// (`CONCURRENCY_LIMIT`), `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn claim_tasks(
        &self,
        ctx: SecurityContext,
        req: ClaimRequest,
    ) -> Result<Option<ClaimResponse>, CanonicalError>;

    /// Renew the worker session and check every listed lease (`heartbeatTasks`).
    /// No PDP action; an unknown session answers every lease `lease_lost`.
    ///
    /// # Errors
    /// `invalid_argument` (too many leases), `service_unavailable` (shed).
    #[idempotency(IdempotentWrite)]
    async fn heartbeat_tasks(
        &self,
        ctx: SecurityContext,
        req: HeartbeatRequest,
    ) -> Result<HeartbeatResponse, CanonicalError>;

    /// `claimed` → `running` (`startTask`). Action `claim`, lease-fenced.
    ///
    /// # Errors
    /// `failed_precondition` (`STATE`), `permission_denied` (revoked; task
    /// released), `not_found`, `aborted` (`LEASE_LOST`), `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn start_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: LeaseTokenRequest,
    ) -> Result<Task, CanonicalError>;

    /// Buffer a progress update, saved at the next flush (`reportProgress`).
    /// Action `claim`, lease-fenced.
    ///
    /// # Errors
    /// `invalid_argument`, `permission_denied`, `not_found`, `aborted`
    /// (`LEASE_LOST`), `service_unavailable`.
    #[idempotency(IdempotentWrite)]
    async fn report_progress(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: ProgressRequest,
    ) -> Result<(), CanonicalError>;

    /// Merge a checkpoint into the task `context` (`saveCheckpoint`). Action
    /// `claim`, lease-fenced. p3.
    ///
    /// # Errors
    /// `invalid_argument` (incl. size), `permission_denied`, `not_found`,
    /// `aborted` (`LEASE_LOST`, `CONCURRENT_MODIFICATION`), `service_unavailable`.
    #[idempotency(IdempotentWrite)]
    async fn save_checkpoint(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: CheckpointRequest,
    ) -> Result<CheckpointResponse, CanonicalError>;

    /// Report the result (`completeTask`). Action `claim`, lease-fenced.
    /// Repeating the same completion with the same token returns the stored
    /// outcome.
    ///
    /// # Errors
    /// `invalid_argument`, `failed_precondition` (`STATE`, `CHILDREN_PENDING`),
    /// `permission_denied`, `not_found`, `aborted` (`LEASE_LOST`),
    /// `service_unavailable`.
    #[idempotency(IdempotentWrite)]
    async fn complete_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: CompleteRequest,
    ) -> Result<Task, CanonicalError>;

    /// Give the task back without failing it (`releaseTask`). Action `claim`,
    /// lease-fenced.
    ///
    /// # Errors
    /// `invalid_argument`, `permission_denied`, `not_found`, `aborted`
    /// (`LEASE_LOST`), `resource_exhausted` (deferred limit),
    /// `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn release_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: ReleaseRequest,
    ) -> Result<Task, CanonicalError>;

    /// Take over a task reassigned to the caller (`acceptReassignedTask`).
    /// Action `claim`. p3.
    ///
    /// # Errors
    /// `invalid_argument`, `not_found` (nothing to accept), `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn accept_reassigned_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: AcceptRequest,
    ) -> Result<AcceptResponse, CanonicalError>;

    // ------------------------------------------------------------- entries

    /// Add an immutable entry (`createEntry`). Action `claim` with a lease
    /// token, else `create`.
    ///
    /// # Errors
    /// `invalid_argument` (incl. size), `permission_denied`, `not_found`,
    /// `aborted` (`LEASE_LOST`, worker path), `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn create_entry(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: CreateEntryRequest,
    ) -> Result<Entry, CanonicalError>;

    /// List a task's entries, oldest first (`listEntries`). Action `read`.
    /// Filter on `type`, `creator`, `created_at`.
    ///
    /// # Errors
    /// `invalid_argument`, `not_found`, `service_unavailable`.
    #[idempotency(SafeRead)]
    async fn list_entries(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        query: PageQuery,
    ) -> Result<EntryPage, CanonicalError>;

    /// Get one entry (`getEntry`). Action `read` on the task.
    ///
    /// # Errors
    /// `not_found`, `service_unavailable`.
    #[idempotency(SafeRead)]
    async fn get_entry(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        entry_id: Uuid,
    ) -> Result<Entry, CanonicalError>;

    // ------------------------------------------------------------- waits

    /// Set (arm or park) the task's wait group (`setWaitConditions`). Action
    /// `claim`, lease-fenced. Parking releases the lease.
    ///
    /// # Errors
    /// `invalid_argument`, `failed_precondition` (`STATE`), `permission_denied`
    /// (event `consume` checks, revoked `claim`), `not_found`, `aborted`,
    /// `resource_exhausted`, `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn set_wait_conditions(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: SetWaitRequest,
    ) -> Result<SetWaitResponse, CanonicalError>;

    /// List every wait condition of the task, oldest first
    /// (`listWaitConditions`). Action `read`. Filter on `wait_group_id`,
    /// `state`, `kind`.
    ///
    /// # Errors
    /// `not_found`, `service_unavailable`.
    #[idempotency(SafeRead)]
    async fn list_wait_conditions(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        query: PageQuery,
    ) -> Result<WaitConditionPage, CanonicalError>;

    /// Park the armed wait group (`parkWaitGroup`). Action `claim`,
    /// lease-fenced.
    ///
    /// # Errors
    /// `failed_precondition` (no armed group), `permission_denied`, `not_found`,
    /// `aborted`, `resource_exhausted`, `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn park_wait_group(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: ParkRequest,
    ) -> Result<ParkResponse, CanonicalError>;

    /// Provide the value of a pending input condition (`resolveWaitCondition`).
    /// Action `resolve`. p2. Repeating the same value as the same subject is a
    /// no-op that returns the same body.
    ///
    /// # Errors
    /// `invalid_argument`, `failed_precondition`, `permission_denied`,
    /// `not_found`, `aborted` (`ALREADY_RESOLVED`), `service_unavailable`.
    #[idempotency(IdempotentWrite)]
    async fn resolve_wait_condition(
        &self,
        ctx: SecurityContext,
        condition_id: Uuid,
        req: ResolveRequest,
    ) -> Result<ResolveResponse, CanonicalError>;

    // ------------------------------------------------------------- history

    /// The task's append-only history, oldest first (`listTaskEvents`). Action
    /// `read`. Filter on `event_type`, `timestamp`.
    ///
    /// # Errors
    /// `invalid_argument`, `not_found`, `service_unavailable`.
    #[idempotency(SafeRead)]
    async fn list_task_events(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        query: PageQuery,
    ) -> Result<HistoryEventPage, CanonicalError>;

    // ------------------------------------------------------------- queues

    /// Every registered queue with limits, counts, domains and hold state
    /// (`listQueues`). Action `queue_read`.
    ///
    /// # Errors
    /// `permission_denied`, `service_unavailable`.
    #[idempotency(SafeRead)]
    async fn list_queues(&self, ctx: SecurityContext) -> Result<QueueList, CanonicalError>;

    /// Get one queue (`getQueue`). Action `queue_read`.
    ///
    /// # Errors
    /// `permission_denied`, `not_found`, `service_unavailable`.
    #[idempotency(SafeRead)]
    async fn get_queue(
        &self,
        ctx: SecurityContext,
        queue_id: GtsInstanceId,
    ) -> Result<Queue, CanonicalError>;

    // ------------------------------------------------------------- bulk (p2)

    /// Cancel many tasks (`bulkCancel`). Action `cancel`, per task.
    ///
    /// # Errors
    /// `invalid_argument` (`BATCH_TOO_LARGE`), `service_unavailable`. Per-task
    /// failures are outcomes in the result.
    #[idempotency(NonIdempotentWrite)]
    async fn bulk_cancel(
        &self,
        ctx: SecurityContext,
        req: BulkCancelRequest,
    ) -> Result<BulkResult, CanonicalError>;

    /// Change priority and/or urgency of many tasks (`bulkReprioritize`).
    /// Action `admin`, per task.
    ///
    /// # Errors
    /// `invalid_argument`, `service_unavailable`.
    #[idempotency(IdempotentWrite)]
    async fn bulk_reprioritize(
        &self,
        ctx: SecurityContext,
        req: BulkReprioritizeRequest,
    ) -> Result<BulkResult, CanonicalError>;

    /// Re-enqueue many dead-lettered or failed tasks (`bulkRetry`). Action
    /// `admin`, per task.
    ///
    /// # Errors
    /// `invalid_argument`, `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn bulk_retry(
        &self,
        ctx: SecurityContext,
        req: BulkTaskIdsRequest,
    ) -> Result<BulkResult, CanonicalError>;

    /// Move many tasks to another queue (`bulkMove`). Action `admin`, per task.
    ///
    /// # Errors
    /// `invalid_argument`, `service_unavailable`.
    #[idempotency(IdempotentWrite)]
    async fn bulk_move(
        &self,
        ctx: SecurityContext,
        req: BulkMoveRequest,
    ) -> Result<BulkResult, CanonicalError>;

    // ------------------------------------------------------------- admin

    /// Hand a claimed or running task to another identity (`reassignTask`).
    /// Action `admin`. p3.
    ///
    /// # Errors
    /// `failed_precondition` (`STATE`), `permission_denied`, `not_found`,
    /// `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn reassign_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: ReassignRequest,
    ) -> Result<Task, CanonicalError>;

    /// `dead_lettered` or `failed` → `queued` (`reenqueueTask`). Action `admin`.
    /// p2.
    ///
    /// # Errors
    /// `failed_precondition`, `invalid_argument`, `permission_denied`,
    /// `not_found`, `already_exists` (dedup identity taken),
    /// `resource_exhausted`, `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn reenqueue_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
    ) -> Result<Task, CanonicalError>;

    /// Save a new runtime configuration version, applied asynchronously per
    /// domain (`saveRuntimeConfig`). Action `queue_configure`. p3.
    ///
    /// # Errors
    /// `invalid_argument`, `permission_denied`, `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn save_runtime_config(
        &self,
        ctx: SecurityContext,
        req: RuntimeConfigRequest,
    ) -> Result<RuntimeConfigAccepted, CanonicalError>;

    /// Which task domains have applied a configuration version
    /// (`getRuntimeConfigStatus`). Action `queue_configure`. p3.
    ///
    /// # Errors
    /// `permission_denied`, `not_found` (unknown version), `service_unavailable`.
    #[idempotency(SafeRead)]
    async fn get_runtime_config_status(
        &self,
        ctx: SecurityContext,
        version: u64,
    ) -> Result<RuntimeConfigStatus, CanonicalError>;

    /// Hold a queue: claims skip it, creates continue (`holdQueue`). Action
    /// `queue_configure`.
    ///
    /// # Errors
    /// `invalid_argument`, `permission_denied`, `not_found`, `service_unavailable`.
    #[idempotency(IdempotentWrite)]
    async fn hold_queue(
        &self,
        ctx: SecurityContext,
        queue_id: GtsInstanceId,
        req: HoldRequest,
    ) -> Result<Queue, CanonicalError>;

    /// End a queue hold (`releaseQueue`). Action `queue_configure`.
    ///
    /// # Errors
    /// `permission_denied`, `not_found`, `service_unavailable`.
    #[idempotency(IdempotentWrite)]
    async fn release_queue(
        &self,
        ctx: SecurityContext,
        queue_id: GtsInstanceId,
    ) -> Result<Queue, CanonicalError>;
}
