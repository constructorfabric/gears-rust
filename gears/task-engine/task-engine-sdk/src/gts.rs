//! GTS identifiers the Task Engine owns or registers.
//!
//! Mirrors `gears/task-engine/docs/schemas/` (`types/` and `instances/`): base
//! types, built-in entry types and queues, the lifecycle event types published
//! to the Event Broker, and the authorization permissions. Use these instead of
//! spelling the IDs out, for example as a `resource_type` or when matching a
//! history event's broker type.

use toolkit_gts::gts_id;

// ---------------------------------------------------------------- base types
/// Base type of every task type; also the task resource type for authorization and errors.
pub const TASK_TYPE: &str = gts_id!("cf.core.tasks.task.v1~");
/// Base type of every queue instance; also the queue resource type.
pub const QUEUE_TYPE: &str = gts_id!("cf.core.tasks.queue.v1~");
/// Base type of every task entry type.
pub const ENTRY_TYPE: &str = gts_id!("cf.core.tasks.entry.v1~");

// ---------------------------------------------------------------- built-in entry types
/// Built-in entry type `entry_issue`.
pub const ENTRY_ISSUE_TYPE: &str = gts_id!("cf.core.tasks.entry.v1~cf.core.tasks.entry_issue.v1~");
/// Built-in entry type `entry_measurement`.
pub const ENTRY_MEASUREMENT_TYPE: &str =
    gts_id!("cf.core.tasks.entry.v1~cf.core.tasks.entry_measurement.v1~");
/// Built-in entry type `entry_note`.
pub const ENTRY_NOTE_TYPE: &str = gts_id!("cf.core.tasks.entry.v1~cf.core.tasks.entry_note.v1~");
/// Built-in entry type `entry_observation`.
pub const ENTRY_OBSERVATION_TYPE: &str =
    gts_id!("cf.core.tasks.entry.v1~cf.core.tasks.entry_observation.v1~");
/// Built-in entry type `entry_work`.
pub const ENTRY_WORK_TYPE: &str = gts_id!("cf.core.tasks.entry.v1~cf.core.tasks.entry_work.v1~");

// ---------------------------------------------------------------- built-in queues
/// Built-in `dead_letter` queue.
pub const DEAD_LETTER_QUEUE: &str = gts_id!("cf.core.tasks.queue.v1~cf.core.tasks.dead_letter.v1");
/// Built-in `default` queue.
pub const DEFAULT_QUEUE: &str = gts_id!("cf.core.tasks.queue.v1~cf.core.tasks.default.v1");
/// Built-in `system` queue.
pub const SYSTEM_QUEUE: &str = gts_id!("cf.core.tasks.queue.v1~cf.core.tasks.system.v1");

// ---------------------------------------------------------------- event broker
/// Event Broker topic the lifecycle events are published to.
pub const TASK_EVENTS_TOPIC: &str = gts_id!("cf.core.events.topic.v1~cf.core.tasks.task_events.v1");
/// The engine's own Event Broker consumer group (event waits).
pub const TASK_ENGINE_CONSUMER_GROUP: &str =
    gts_id!("cf.core.events.consumer_group.v1~cf.core.tasks.task_engine.v1");

/// Lifecycle event types (`gts.cf.core.events.event.v1~cf.core.tasks.<name>.v1~`).
///
/// A history event's `camelCase` `event_type` (`taskCreated`) is the `snake_case`
/// `<name>` here (`task_created`).
pub mod event {
    use toolkit_gts::gts_id;

    /// `config_updated`.
    pub const CONFIG_UPDATED: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.config_updated.v1~");
    /// `sla_breached`.
    pub const SLA_BREACHED: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.sla_breached.v1~");
    /// `task_cancel_requested`.
    pub const TASK_CANCEL_REQUESTED: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.task_cancel_requested.v1~");
    /// `task_cancelled`.
    pub const TASK_CANCELLED: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.task_cancelled.v1~");
    /// `task_checkpointed`.
    pub const TASK_CHECKPOINTED: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.task_checkpointed.v1~");
    /// `task_claimed`.
    pub const TASK_CLAIMED: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.task_claimed.v1~");
    /// `task_completed`.
    pub const TASK_COMPLETED: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.task_completed.v1~");
    /// `task_created`.
    pub const TASK_CREATED: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.task_created.v1~");
    /// `task_dead_lettered`.
    pub const TASK_DEAD_LETTERED: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.task_dead_lettered.v1~");
    /// `task_failed`.
    pub const TASK_FAILED: &str = gts_id!("cf.core.events.event.v1~cf.core.tasks.task_failed.v1~");
    /// `task_progressed`.
    pub const TASK_PROGRESSED: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.task_progressed.v1~");
    /// `task_queued`.
    pub const TASK_QUEUED: &str = gts_id!("cf.core.events.event.v1~cf.core.tasks.task_queued.v1~");
    /// `task_reassigned`.
    pub const TASK_REASSIGNED: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.task_reassigned.v1~");
    /// `task_resumed`.
    pub const TASK_RESUMED: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.task_resumed.v1~");
    /// `task_retrying`.
    pub const TASK_RETRYING: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.task_retrying.v1~");
    /// `task_started`.
    pub const TASK_STARTED: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.task_started.v1~");
    /// `task_timed_out`.
    pub const TASK_TIMED_OUT: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.task_timed_out.v1~");
    /// `task_waiting`.
    pub const TASK_WAITING: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.task_waiting.v1~");
    /// `wait_condition_satisfied`.
    pub const WAIT_CONDITION_SATISFIED: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.wait_condition_satisfied.v1~");
    /// `wait_condition_set`.
    pub const WAIT_CONDITION_SET: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.wait_condition_set.v1~");
    /// `wait_timed_out`.
    pub const WAIT_TIMED_OUT: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.wait_timed_out.v1~");
    /// `worker_blacklisted`.
    pub const WORKER_BLACKLISTED: &str =
        gts_id!("cf.core.events.event.v1~cf.core.tasks.worker_blacklisted.v1~");
}

/// Authorization permission instances (`gts.cf.toolkit.authz.permission.v1~cf.task_engine._.<name>.v1`).
pub mod permission {
    use toolkit_gts::gts_id;

    /// Permission `queue_configure`.
    pub const QUEUE_CONFIGURE: &str =
        gts_id!("cf.toolkit.authz.permission.v1~cf.task_engine._.queue_configure.v1");
    /// Permission `queue_read`.
    pub const QUEUE_READ: &str =
        gts_id!("cf.toolkit.authz.permission.v1~cf.task_engine._.queue_read.v1");
    /// Permission `task_cancel`.
    pub const TASK_CANCEL: &str =
        gts_id!("cf.toolkit.authz.permission.v1~cf.task_engine._.task_cancel.v1");
    /// Permission `task_claim`.
    pub const TASK_CLAIM: &str =
        gts_id!("cf.toolkit.authz.permission.v1~cf.task_engine._.task_claim.v1");
    /// Permission `task_create`.
    pub const TASK_CREATE: &str =
        gts_id!("cf.toolkit.authz.permission.v1~cf.task_engine._.task_create.v1");
    /// Permission `task_delete`.
    pub const TASK_DELETE: &str =
        gts_id!("cf.toolkit.authz.permission.v1~cf.task_engine._.task_delete.v1");
    /// Permission `task_expedite`.
    pub const TASK_EXPEDITE: &str =
        gts_id!("cf.toolkit.authz.permission.v1~cf.task_engine._.task_expedite.v1");
    /// Permission `task_read`.
    pub const TASK_READ: &str =
        gts_id!("cf.toolkit.authz.permission.v1~cf.task_engine._.task_read.v1");
    /// Permission `task_resolve`.
    pub const TASK_RESOLVE: &str =
        gts_id!("cf.toolkit.authz.permission.v1~cf.task_engine._.task_resolve.v1");
    /// Permission `task_update`.
    pub const TASK_UPDATE: &str =
        gts_id!("cf.toolkit.authz.permission.v1~cf.task_engine._.task_update.v1");
    /// Permission `tasks_admin`.
    pub const TASKS_ADMIN: &str =
        gts_id!("cf.toolkit.authz.permission.v1~cf.task_engine._.tasks_admin.v1");
}
