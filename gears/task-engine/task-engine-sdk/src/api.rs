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
//!
//! **Worker calls** (the lease-fenced `start`, `progress`, `checkpoint`,
//! `complete`, `release`, set-wait, park, and the worker path of
//! `create_entry`) never fail `not_found`: a caller that is not the current
//! holder with that token — stale or unknown token, unknown task, a task of
//! another tenant — gets `aborted` `LEASE_LOST`. `permission_denied`
//! `CLAIM_REVOKED` means the caller's scope no longer covers the task and the
//! task was released; drop it. `progress`, `complete`, `release` and set-wait
//! are allowed from `claimed` as well as `running` (`start` is optional).
//!
//! **Retry hints.** Every `resource_exhausted` (429) and `service_unavailable`
//! (503) carries `Retry-After`. `service_unavailable` covers an unreachable
//! PDP (fail closed: nothing written, leases kept), an unreachable GTS
//! registry for an uncached type, and node overload — overload is never
//! `resource_exhausted`. See [`crate::reason`] for where each code travels.

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
    QueueHistoryEventPage, QueueList, ReassignRequest, ReenqueueRequest, ReleaseRequest,
    ResolveRequest, ResolveResponse, RuntimeConfigAccepted, RuntimeConfigRequest,
    RuntimeConfigStatus, SearchTasksQuery, SetWaitOutcome, SetWaitRequest, TagsPatchRequest, Task,
    TaskCount, TaskSearchPage, WaitConditionPage,
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
    /// `queued` (also for a past `run_after`). `owner_tenant_id` defaults to
    /// the caller's tenant. A replay of an equal request under the same
    /// idempotency key fails `already_exists` naming the existing task (never
    /// refused as full), so this is not retried automatically.
    ///
    /// # Errors
    /// `invalid_argument` (field violations: `SCHEMA_VIOLATION`,
    /// `UNKNOWN_TASK_TYPE`, `DEAD_LETTER_QUEUE`, `TASK_DOMAIN_MISMATCH`,
    /// `TRUST_LIMIT`, `KEY_REUSED`, `FEATURE_NOT_AVAILABLE`, …; missing
    /// idempotency key is `428`), `failed_precondition` (`STALE_ORDER`,
    /// `DEPENDENCY_UNSATISFIABLE`, p2), `permission_denied` (`ACTION_DENIED`,
    /// `EXPEDITE_REQUIRED`, `CREATOR_AUTHORITY`,
    /// `CREATOR_AUTHORITY_UNVERIFIABLE`, `DUPLICATE_NOT_READABLE`,
    /// `SUPERSEDE_CANCEL_DENIED`), `not_found` (queue, parent, predecessor),
    /// `already_exists`, `aborted` (`STATE_CHANGED`, p2), `resource_exhausted`
    /// (`queue_depth.*`), `service_unavailable`.
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
    /// `permission_denied` (`ACTION_DENIED`: the PDP denies `read` outright; a
    /// scope that matches nothing is an empty page), `service_unavailable`.
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
    /// The body is optional on the wire (`CancelRequest::default()`); `mode`
    /// defaults to `requested`. An unleased task, or `forced`, ends at once
    /// (`cancelled`); `requested` / `graceful` on a leased task only sets
    /// `cancel_requested` (HTTP `202`), and modes only move forward (a repeat
    /// is a no-op `202`). A task already ended in the cancelled family is
    /// returned unchanged. Either way the returned task shows which happened.
    ///
    /// # Errors
    /// `invalid_argument`, `failed_precondition` (`TASK_FINISHED`,
    /// `NOT_CANCELLABLE`), `permission_denied`, `not_found`, `aborted`
    /// (`STATE_CHANGED`), `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn cancel_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: CancelRequest,
    ) -> Result<Task, CanonicalError>;

    /// Add, replace or remove tags (`updateTaskTags`). Action `update`. p2.
    /// Records one `taskTagsUpdated` history event.
    ///
    /// # Errors
    /// `invalid_argument` (`DUPLICATE`, `TOO_LONG`, `REQUIRED`,
    /// `TOO_MANY_TAGS`), `permission_denied`, `not_found`, `service_unavailable`.
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
    /// `Ok(None)` when nothing became eligible before the timeout (HTTP `204`),
    /// also when every named queue is held or the worker is blacklisted on it;
    /// no session is opened, renewed or returned then.
    ///
    /// # Errors
    /// `invalid_argument` (`REQUIRED`, `INVALID_FORMAT`, `OUT_OF_RANGE`,
    /// `TOO_MANY_QUEUES`, `DEAD_LETTER_QUEUE`, `CROSS_DOMAIN`), `not_found`
    /// (unregistered queue), `permission_denied` (`ACTION_DENIED`,
    /// `QUEUE_NOT_PERMITTED`), `resource_exhausted` (`concurrency`),
    /// `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn claim_tasks(
        &self,
        ctx: SecurityContext,
        req: ClaimRequest,
    ) -> Result<Option<ClaimResponse>, CanonicalError>;

    /// Renew the worker session and check every listed lease (`heartbeatTasks`).
    /// No PDP action; an unknown, expired or foreign session answers every
    /// lease `lease_lost` and is not revived. A pending cancel is reported per
    /// lease (with `superseded_by` when a supersession caused it).
    ///
    /// # Errors
    /// `invalid_argument` (`TOO_MANY` on `/leases`; session not renewed),
    /// `service_unavailable` (node overloaded).
    #[idempotency(IdempotentWrite)]
    async fn heartbeat_tasks(
        &self,
        ctx: SecurityContext,
        req: HeartbeatRequest,
    ) -> Result<HeartbeatResponse, CanonicalError>;

    /// Optional `claimed` → `running` (`startTask`). Action `claim`,
    /// lease-fenced. A repeated start by the holder on a `running` task is a
    /// no-op; `start` never moves the execution deadline the claim set.
    ///
    /// # Errors
    /// `invalid_argument` (`REQUIRED`), `permission_denied` (`CLAIM_REVOKED`;
    /// task released), `aborted` (`LEASE_LOST`), `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn start_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: LeaseTokenRequest,
    ) -> Result<Task, CanonicalError>;

    /// Buffer a progress update from `claimed` or `running`, saved at the next
    /// flush (`reportProgress`). Action `claim`, lease-fenced.
    ///
    /// # Errors
    /// `invalid_argument` (`OUT_OF_RANGE`), `permission_denied`
    /// (`CLAIM_REVOKED`), `aborted` (`LEASE_LOST`), `service_unavailable`.
    #[idempotency(IdempotentWrite)]
    async fn report_progress(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: ProgressRequest,
    ) -> Result<(), CanonicalError>;

    /// Merge a checkpoint into the task `context` as an RFC 7386 JSON Merge
    /// Patch (`saveCheckpoint`). Action `claim`, lease-fenced. p3.
    ///
    /// # Errors
    /// `invalid_argument` (incl. size), `permission_denied` (`CLAIM_REVOKED`),
    /// `aborted` (`LEASE_LOST`, `CONCURRENT_MODIFICATION`), `service_unavailable`.
    #[idempotency(IdempotentWrite)]
    async fn save_checkpoint(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: CheckpointRequest,
    ) -> Result<CheckpointResponse, CanonicalError>;

    /// Report the result from `claimed` or `running` (`completeTask`). Action
    /// `claim`, lease-fenced. Repeating a terminal completion with the same
    /// token returns the stored outcome; a repeated retryable failure after a
    /// lost response gets `LEASE_LOST` (the first report was applied).
    ///
    /// # Errors
    /// `invalid_argument` (lease kept), `failed_precondition` (`STATE`,
    /// `CHILDREN_PENDING`), `permission_denied` (`CLAIM_REVOKED`), `aborted`
    /// (`LEASE_LOST`; checked before validation), `service_unavailable`.
    #[idempotency(IdempotentWrite)]
    async fn complete_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: CompleteRequest,
    ) -> Result<Task, CanonicalError>;

    /// Give the task back without failing it, from `claimed` or `running`
    /// (`releaseTask`). Action `claim`, lease-fenced. A repeated release after
    /// a lost response gets `LEASE_LOST` (the first one was applied).
    ///
    /// # Errors
    /// `invalid_argument` (`OUT_OF_RANGE` on `/delay_secs`), `permission_denied`
    /// (`CLAIM_REVOKED`), `aborted` (`LEASE_LOST`, `CONCURRENT_MODIFICATION`),
    /// `resource_exhausted` (`queue_depth.deferred`, delayed release only;
    /// lease kept), `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn release_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: ReleaseRequest,
    ) -> Result<Task, CanonicalError>;

    /// Take over a task reassigned to the caller (`acceptReassignedTask`).
    /// Action `claim`, checked with the accepting worker's own scope. p3.
    ///
    /// # Errors
    /// `invalid_argument`, `permission_denied` (`ACTION_DENIED`), `not_found`
    /// (nothing to accept), `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn accept_reassigned_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: AcceptRequest,
    ) -> Result<AcceptResponse, CanonicalError>;

    // ------------------------------------------------------------- entries

    /// Add an immutable entry (`createEntry`). Action `claim` with a lease
    /// token (lease-fenced), else `create` (any task state, terminal
    /// included). At most `max_entries_per_task` (1000) entries per task.
    ///
    /// # Errors
    /// `invalid_argument` (`UNKNOWN_ENTRY_TYPE`, `SCHEMA_VIOLATION`; incl.
    /// size), `failed_precondition` (`TOO_MANY_ENTRIES`), `permission_denied`
    /// (`ACTION_DENIED`; worker path `CLAIM_REVOKED`), `not_found` (API-consumer
    /// path only), `aborted` (`LEASE_LOST`, worker path), `service_unavailable`.
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

    /// Set (arm or park) the task's wait group from `claimed` or `running`
    /// (`setWaitConditions`). Action `claim`, lease-fenced. Parking releases
    /// the lease. A group satisfied at once is not parked: the result is
    /// [`SetWaitOutcome::Satisfied`] (`parked: false` with the `result`), the
    /// lease is kept and the task stays as it was.
    ///
    /// # Errors
    /// `invalid_argument`, `failed_precondition` (`STATE`: the task already has
    /// a group), `permission_denied` (`EVENT_CONSUME_DENIED`,
    /// `EVENT_CONSUME_UNVERIFIABLE`, lease kept; `CLAIM_REVOKED`), `aborted`
    /// (`LEASE_LOST`, `CONCURRENT_MODIFICATION`), `resource_exhausted`
    /// (`queue_depth.deferred`, `event_waits.*`; lease kept),
    /// `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn set_wait_conditions(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: SetWaitRequest,
    ) -> Result<SetWaitOutcome, CanonicalError>;

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
    /// lease-fenced. An already satisfied group is not parked; the task keeps
    /// running under the same lease.
    ///
    /// # Errors
    /// `invalid_argument` (`REQUIRED`), `failed_precondition`
    /// (`NO_ARMED_WAIT`, lease kept), `permission_denied` (`CLAIM_REVOKED`),
    /// `aborted` (`LEASE_LOST`, `CONCURRENT_MODIFICATION`), `resource_exhausted`
    /// (`queue_depth.deferred`), `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn park_wait_group(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: ParkRequest,
    ) -> Result<ParkResponse, CanonicalError>;

    /// Provide the value of a pending input condition, parked or armed
    /// (`resolveWaitCondition`). Action `resolve` only (no `read` needed). p2.
    /// Repeating the same value as the same subject is a no-op that returns
    /// the same body.
    ///
    /// # Errors
    /// `invalid_argument` (`SCHEMA_VIOLATION`), `failed_precondition`
    /// (`ALREADY_RESOLVED`: another value or subject; `NOT_RESOLVABLE`; `STATE`),
    /// `permission_denied` (`ACTION_DENIED`), `not_found`, `aborted`
    /// (`STATE_CHANGED`), `service_unavailable`.
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

    /// Every registered queue with limits, settings, domains and hold state
    /// (`listQueues`). Action `queue_read`. `counts` and `sampled_at` only for
    /// a caller that also holds `configure` on queues.
    ///
    /// # Errors
    /// `permission_denied`, `service_unavailable`.
    #[idempotency(SafeRead)]
    async fn list_queues(&self, ctx: SecurityContext) -> Result<QueueList, CanonicalError>;

    /// Get one queue (`getQueue`). Action `queue_read`. `counts` and
    /// `sampled_at` only for a caller that also holds `configure` on queues.
    ///
    /// # Errors
    /// `permission_denied`, `not_found`, `service_unavailable`.
    #[idempotency(SafeRead)]
    async fn get_queue(
        &self,
        ctx: SecurityContext,
        queue_id: GtsInstanceId,
    ) -> Result<Queue, CanonicalError>;

    /// The queue-scoped history of one queue (`queueHeld`, `queueReleased`,
    /// `configUpdated`, `workerBlacklisted`), oldest first, cursor-paged like
    /// the task history (`listQueueEvents`). Action `queue_read`. Filter on
    /// `event_type` (`eq`, `in`) and `timestamp`.
    ///
    /// # Errors
    /// `invalid_argument`, `permission_denied`, `not_found`,
    /// `service_unavailable`.
    #[idempotency(SafeRead)]
    async fn list_queue_events(
        &self,
        ctx: SecurityContext,
        queue_id: GtsInstanceId,
        query: PageQuery,
    ) -> Result<QueueHistoryEventPage, CanonicalError>;

    // ------------------------------------------------------------- bulk (p2)

    /// Cancel many tasks (`bulkCancel`). Action `cancel`, per task.
    ///
    /// `mode` defaults to `requested`; a leased task only flagged reports
    /// `cancel_requested`.
    ///
    /// # Errors
    /// `invalid_argument` (`BATCH_TOO_LARGE`), `permission_denied`
    /// (`ACTION_DENIED`: denied outright), `service_unavailable`. Per-task
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
    /// `invalid_argument` (`BATCH_TOO_LARGE`), `permission_denied`
    /// (`ACTION_DENIED`: denied outright), `service_unavailable`.
    #[idempotency(IdempotentWrite)]
    async fn bulk_reprioritize(
        &self,
        ctx: SecurityContext,
        req: BulkReprioritizeRequest,
    ) -> Result<BulkResult, CanonicalError>;

    /// Re-enqueue many dead-lettered or failed tasks into their source queues
    /// (`bulkRetry`). Action `admin`, per task. Depth refusals report
    /// `limit_exceeded`, precondition refusals `invalid_state`.
    ///
    /// # Errors
    /// `invalid_argument` (`BATCH_TOO_LARGE`), `permission_denied`
    /// (`ACTION_DENIED`: denied outright), `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn bulk_retry(
        &self,
        ctx: SecurityContext,
        req: BulkTaskIdsRequest,
    ) -> Result<BulkResult, CanonicalError>;

    /// Move many tasks to another queue (`bulkMove`). Action `admin`, per task.
    /// Per task: another domain → `domain_mismatch`, a depth refusal →
    /// `limit_exceeded`.
    ///
    /// # Errors
    /// `invalid_argument` (`BATCH_TOO_LARGE`, `DEAD_LETTER_QUEUE`),
    /// `permission_denied` (`ACTION_DENIED`: denied outright), `not_found`
    /// (unregistered destination), `service_unavailable`.
    #[idempotency(IdempotentWrite)]
    async fn bulk_move(
        &self,
        ctx: SecurityContext,
        req: BulkMoveRequest,
    ) -> Result<BulkResult, CanonicalError>;

    // ------------------------------------------------------------- admin

    /// Hand a claimed or running task to another identity (`reassignTask`).
    /// Action `admin`. p3. An optional `reason` is recorded in
    /// `taskReassigned`.
    ///
    /// # Errors
    /// `invalid_argument` (`TOO_LONG`), `failed_precondition` (`STATE`,
    /// `TARGET_BLACKLISTED`), `permission_denied`, `not_found`, `aborted`
    /// (`STATE_CHANGED`), `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn reassign_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: ReassignRequest,
    ) -> Result<Task, CanonicalError>;

    /// `dead_lettered` or `failed` → `queued` (`reenqueueTask`). Action `admin`.
    /// p2. The task returns to its source queue unless `req.queue` names
    /// another dispatch queue (`ReenqueueRequest::default()` sends `{}`).
    ///
    /// # Errors
    /// `failed_precondition` (`STATE`, `STALE_ORDER`, `PARENT_FINISHED`),
    /// `invalid_argument` (`TASK_DOMAIN_MISMATCH`, `DEAD_LETTER_QUEUE`,
    /// `NOT_ALLOWED` on `/queue`), `permission_denied`, `not_found` (task, or
    /// an unregistered queue), `already_exists` (dedup identity taken),
    /// `aborted` (`STATE_CHANGED`), `resource_exhausted` (`queue_depth.*`),
    /// `service_unavailable`.
    #[idempotency(NonIdempotentWrite)]
    async fn reenqueue_task(
        &self,
        ctx: SecurityContext,
        task_id: Uuid,
        req: ReenqueueRequest,
    ) -> Result<Task, CanonicalError>;

    /// Save a new runtime configuration version, applied asynchronously per
    /// domain (`saveRuntimeConfig`). Action `queue_configure` on the queue base
    /// type (one check; all-or-nothing). p3.
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
    /// `queue_configure`. Holding a held queue replaces `reason` and `until`.
    ///
    /// # Errors
    /// `invalid_argument` (`OUT_OF_RANGE` on `/until`, `TOO_LONG`),
    /// `failed_precondition` (`DEAD_LETTER_QUEUE`), `permission_denied`,
    /// `not_found`, `service_unavailable`.
    #[idempotency(IdempotentWrite)]
    async fn hold_queue(
        &self,
        ctx: SecurityContext,
        queue_id: GtsInstanceId,
        req: HoldRequest,
    ) -> Result<Queue, CanonicalError>;

    /// End a queue hold (`releaseQueue`). Action `queue_configure`. Releasing
    /// a queue that is not held returns it with `hold: None` and records
    /// nothing.
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
