//! Task Engine error codes, by the slot of the canonical error that carries
//! them (`openapi.yaml`, `ProblemContext`, `FieldViolation`, `Violation`).
//!
//! Only `permission_denied`, `aborted` and `unauthenticated` carry
//! `context.reason`. The other categories carry their code in the slot the
//! error library builds, never in `context.reason`:
//!
//! | category              | HTTP | where the code is                                   | constants          |
//! |-----------------------|------|-----------------------------------------------------|--------------------|
//! | `permission_denied`   | 403  | `context.reason`                                    | [`permission`]     |
//! | `aborted`             | 409  | `context.reason`                                    | [`aborted`]        |
//! | `failed_precondition` | 400  | `violations[{type: "STATE", subject: <code>}]`      | [`precondition`]   |
//! | `resource_exhausted`  | 429  | `violations[].subject` (the full quota)             | [`quota`]          |
//! | `invalid_argument`    | 400  | `field_violations[].reason` (every code names a field) | [`field`]       |
//!
//! A `failed_precondition` names the task or queue in `resource_name`; a plain
//! state conflict has `subject: "STATE"`. A `resource_exhausted` never carries
//! usage figures; `retry_after_seconds` is per violation. A `404` carries only
//! `resource_type` and `resource_name`; a `403` only its reason (a check or
//! condition index is only in `detail`).
//!
//! The `permission_denied`, `aborted` and `failed_precondition` lists are
//! closed in the document; the field-violation list is "the documented set".
//! Clients still treat every list as open. Plain constants, like the
//! single-value reasons of other SDKs.

/// `permission_denied` (403) reasons, in `context.reason`. A closed list.
pub mod permission {
    /// The PDP denies the action (`create`, `read`, `cancel`, `update`,
    /// `claim`, `resolve`, `admin`, `configure`) on the target.
    pub const ACTION_DENIED: &str = "ACTION_DENIED";
    /// `urgency: critical` (create, or raising it in bulk) without `expedite`.
    pub const EXPEDITE_REQUIRED: &str = "EXPEDITE_REQUIRED";
    /// A claimed queue is provably outside the caller's policy (`detail`
    /// names it); no session opened.
    pub const QUEUE_NOT_PERMITTED: &str = "QUEUE_NOT_PERMITTED";
    /// A creator-authority check denied the create.
    pub const CREATOR_AUTHORITY: &str = "CREATOR_AUTHORITY";
    /// A creator-authority check could not be decided.
    pub const CREATOR_AUTHORITY_UNVERIFIABLE: &str = "CREATOR_AUTHORITY_UNVERIFIABLE";
    /// The caller's `consume` check on an event condition was denied; lease kept.
    pub const EVENT_CONSUME_DENIED: &str = "EVENT_CONSUME_DENIED";
    /// The caller's `consume` check on an event condition is not enforceable;
    /// lease kept.
    pub const EVENT_CONSUME_UNVERIFIABLE: &str = "EVENT_CONSUME_UNVERIFIABLE";
    /// A dedup hit on a task the caller cannot read (no task ID disclosed).
    pub const DUPLICATE_NOT_READABLE: &str = "DUPLICATE_NOT_READABLE";
    /// Supersession without `cancel` on the replaced task (p2).
    pub const SUPERSEDE_CANCEL_DENIED: &str = "SUPERSEDE_CANCEL_DENIED";
    /// Lease-fenced worker call: the caller holds the lease, but its scope no
    /// longer covers the task. The task was released (no budget, attempt or
    /// blacklist change); drop it.
    pub const CLAIM_REVOKED: &str = "CLAIM_REVOKED";
}

/// `aborted` (409) reasons, in `context.reason`. A closed list.
pub mod aborted {
    /// The caller is not the current holder with that token (stale or unknown
    /// token, unknown task, a task of another tenant). Final; worker calls
    /// answer this instead of `404`.
    pub const LEASE_LOST: &str = "LEASE_LOST";
    /// Lost the optimistic `revision` guard on `context`; retry.
    pub const CONCURRENT_MODIFICATION: &str = "CONCURRENT_MODIFICATION";
    /// A non-worker write whose state guard lost; read the target and retry.
    pub const STATE_CHANGED: &str = "STATE_CHANGED";
}

/// `failed_precondition` (400, not 409) codes, in
/// `violations[{type: "STATE", subject: <code>}]`. A closed list.
pub mod precondition {
    /// The violation `type` of every `failed_precondition`, and the subject of
    /// a plain state conflict (the task or condition is in the wrong state).
    pub const STATE: &str = "STATE";
    /// Supersession or re-enqueue against a newer order (p2).
    pub const STALE_ORDER: &str = "STALE_ORDER";
    /// Successful completion of a `wait_for_children` parent with unfinished
    /// children (p2).
    pub const CHILDREN_PENDING: &str = "CHILDREN_PENDING";
    /// Cancel of a task that ended other than in the cancelled family.
    pub const TASK_FINISHED: &str = "TASK_FINISHED";
    /// `requested`/`graceful` cancel of a `cancellable: false` task.
    pub const NOT_CANCELLABLE: &str = "NOT_CANCELLABLE";
    /// Park with no armed wait group; lease kept.
    pub const NO_ARMED_WAIT: &str = "NO_ARMED_WAIT";
    /// Input condition already resolved with another value or by another
    /// subject.
    pub const ALREADY_RESOLVED: &str = "ALREADY_RESOLVED";
    /// Resolve of a timer, event or children condition.
    pub const NOT_RESOLVABLE: &str = "NOT_RESOLVABLE";
    /// Create with a dependency that cannot be satisfied (p2).
    pub const DEPENDENCY_UNSATISFIABLE: &str = "DEPENDENCY_UNSATISFIABLE";
    /// Re-enqueue of a child whose parent has ended.
    pub const PARENT_FINISHED: &str = "PARENT_FINISHED";
    /// Reassignment to a worker blacklisted on the task's queue.
    pub const TARGET_BLACKLISTED: &str = "TARGET_BLACKLISTED";
    /// Hold of a dead-letter queue. The same string is also a field-violation
    /// reason ([`field::DEAD_LETTER_QUEUE`](super::field::DEAD_LETTER_QUEUE)).
    pub const DEAD_LETTER_QUEUE: &str = "DEAD_LETTER_QUEUE";
    /// The task already has `max_entries_per_task` (1000) entries.
    pub const TOO_MANY_ENTRIES: &str = "TOO_MANY_ENTRIES";
}

/// `resource_exhausted` (429) quotas, in `violations[].subject`. Never in
/// `context.reason`; never with usage figures. Every `429` carries
/// `Retry-After`.
pub mod quota {
    /// Queue depth of the queue across all tenants.
    pub const QUEUE_DEPTH_QUEUE: &str = "queue_depth.queue";
    /// Queue depth of the tenant in the queue (sampled).
    pub const QUEUE_DEPTH_TENANT: &str = "queue_depth.tenant";
    /// Queue depth across the task domain.
    pub const QUEUE_DEPTH_DOMAIN_GLOBAL: &str = "queue_depth.domain_global";
    /// Deferred (`scheduled` + `blocked` + `waiting`) tasks of the tenant: a
    /// create into a deferred state, a delayed release, or a park.
    pub const QUEUE_DEPTH_DEFERRED: &str = "queue_depth.deferred";
    /// A claim assigned nothing because a concurrency dimension
    /// (`queue_global`, `queue_tenant`, `type_global`, `type_tenant`) was full.
    pub const CONCURRENCY: &str = "concurrency";
    /// Event waits of the tenant.
    pub const EVENT_WAITS_TENANT: &str = "event_waits.tenant";
    /// Event waits on the event type.
    pub const EVENT_WAITS_EVENT_TYPE: &str = "event_waits.event_type";
    /// Unkeyed event waits (no `subject_id`).
    pub const EVENT_WAITS_UNKEYED: &str = "event_waits.unkeyed";
}

/// `invalid_argument` (400) codes, in `field_violations[].reason`. The field is
/// an RFC 6901 JSON Pointer from the request body root (`/queues/1`); a header
/// or query parameter is named as sent (`Idempotency-Key`, `cursor`).
pub mod field {
    // --- generic ---------------------------------------------------------
    /// A required value is missing or empty.
    pub const REQUIRED: &str = "REQUIRED";
    /// A value is malformed (`worker_id`, `Idempotency-Key`, …).
    pub const INVALID_FORMAT: &str = "INVALID_FORMAT";
    /// A number or time outside its range.
    pub const OUT_OF_RANGE: &str = "OUT_OF_RANGE";
    /// A string over its length limit.
    pub const TOO_LONG: &str = "TOO_LONG";
    /// A list over its size limit.
    pub const TOO_MANY: &str = "TOO_MANY";
    /// The same item twice (tag key, predecessor).
    pub const DUPLICATE: &str = "DUPLICATE";
    /// A payload over its size limit.
    pub const PAYLOAD_TOO_LARGE: &str = "PAYLOAD_TOO_LARGE";
    /// A value outside a closed request enum.
    pub const INVALID_ENUM_VALUE: &str = "INVALID_ENUM_VALUE";
    /// A value or combination the operation does not allow.
    pub const NOT_ALLOWED: &str = "NOT_ALLOWED";
    /// A value fails its JSON Schema (`description` names the failed keyword).
    pub const SCHEMA_VIOLATION: &str = "SCHEMA_VIOLATION";
    /// A supplied schema is too large or too deep.
    pub const SCHEMA_TOO_LARGE: &str = "SCHEMA_TOO_LARGE";
    /// A referenced item does not exist.
    pub const NOT_FOUND: &str = "NOT_FOUND";
    /// `$count`, `$skip`, `$expand` or `$search`.
    pub const UNSUPPORTED_QUERY_PARAM: &str = "UNSUPPORTED_QUERY_PARAM";

    // --- Task Engine -----------------------------------------------------
    /// The task type is not registered (or not visible).
    pub const UNKNOWN_TASK_TYPE: &str = "UNKNOWN_TASK_TYPE";
    /// Unknown, hidden or non-entry entry type.
    pub const UNKNOWN_ENTRY_TYPE: &str = "UNKNOWN_ENTRY_TYPE";
    /// Unknown or hidden event type on an event condition.
    pub const UNKNOWN_EVENT_TYPE: &str = "UNKNOWN_EVENT_TYPE";
    /// Queue, type or target queue of another task domain.
    pub const TASK_DOMAIN_MISMATCH: &str = "TASK_DOMAIN_MISMATCH";
    /// More predecessors than `max_predecessors_per_task` (64).
    pub const DEPENDENCY_GRAPH_TOO_LARGE: &str = "DEPENDENCY_GRAPH_TOO_LARGE";
    /// The event type's topic is not waitable.
    pub const EVENT_TOPIC_NOT_WAITABLE: &str = "EVENT_TOPIC_NOT_WAITABLE";
    /// The event type allows several subject types and none was named.
    pub const EVENT_SUBJECT_TYPE_REQUIRED: &str = "EVENT_SUBJECT_TYPE_REQUIRED";
    /// The named subject type is not allowed for the event type.
    pub const EVENT_SUBJECT_TYPE_NOT_ALLOWED: &str = "EVENT_SUBJECT_TYPE_NOT_ALLOWED";
    /// A `match_expression` that does not parse or breaches a CEL limit.
    pub const INVALID_MATCH_EXPRESSION: &str = "INVALID_MATCH_EXPRESSION";
    /// More task IDs than `bulk_max_ids` (1000).
    pub const BATCH_TOO_LARGE: &str = "BATCH_TOO_LARGE";
    /// More than `max_tags_per_task` (16) tags.
    pub const TOO_MANY_TAGS: &str = "TOO_MANY_TAGS";
    /// More than `claim_max_queues` (16) distinct queues in a claim.
    pub const TOO_MANY_QUEUES: &str = "TOO_MANY_QUEUES";
    /// A hierarchy deeper than `max_hierarchy_depth` (10).
    pub const TOO_DEEP: &str = "TOO_DEEP";
    /// A dead-letter queue where a dispatch queue is required. The same
    /// string is also a `failed_precondition` subject
    /// ([`precondition::DEAD_LETTER_QUEUE`](super::precondition::DEAD_LETTER_QUEUE)).
    pub const DEAD_LETTER_QUEUE: &str = "DEAD_LETTER_QUEUE";
    /// Claimed queues of two domains, or a `domain` a named queue is not in.
    pub const CROSS_DOMAIN: &str = "CROSS_DOMAIN";
    /// A request value outside the type's `trust_limits`.
    pub const TRUST_LIMIT: &str = "TRUST_LIMIT";
    /// A queue of the wrong kind.
    pub const QUEUE_KIND: &str = "QUEUE_KIND";
    /// The same `Idempotency-Key` with a different request.
    pub const KEY_REUSED: &str = "KEY_REUSED";
    /// The task type's policy is invalid.
    pub const INVALID_TYPE_POLICY: &str = "INVALID_TYPE_POLICY";
    /// A p2/p3 feature requested in an earlier phase.
    pub const FEATURE_NOT_AVAILABLE: &str = "FEATURE_NOT_AVAILABLE";

    // --- cursor and paging -----------------------------------------------
    /// Malformed or foreign cursor.
    pub const INVALID_CURSOR: &str = "INVALID_CURSOR";
    /// A cursor sent with a different `$filter`.
    pub const FILTER_MISMATCH: &str = "FILTER_MISMATCH";
    /// `$orderby` sent with a cursor.
    pub const ORDER_WITH_CURSOR: &str = "ORDER_WITH_CURSOR";
    /// A cursor sent with a different `$select` or `view`.
    pub const FIELD_SELECTION_MISMATCH: &str = "FIELD_SELECTION_MISMATCH";
    /// `limit=0`.
    pub const INVALID_LIMIT: &str = "INVALID_LIMIT";
}
