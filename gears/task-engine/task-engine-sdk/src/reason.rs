//! Task Engine reason codes: the wire `reason` vocabulary carried in
//! `context.reason`, repeated in `field_violations[].reason`,
//! `violations[].type` or (for a full dimension) `violations[].subject`.
//!
//! The documented set (`openapi.yaml`, `ProblemContext`); clients treat it as
//! open. Plain constants, like the single-value reasons of other SDKs.

// --- aborted (409) -------------------------------------------------------
/// Stale or unknown lease token, or a call not from the lease holder. Final.
pub const LEASE_LOST: &str = "LEASE_LOST";
/// Lost the optimistic `revision` guard on `context`; retry.
pub const CONCURRENT_MODIFICATION: &str = "CONCURRENT_MODIFICATION";
/// Input condition already resolved with another value or by another subject.
pub const ALREADY_RESOLVED: &str = "ALREADY_RESOLVED";

// --- failed_precondition (400) -------------------------------------------
/// The task is in the wrong state for the call.
pub const STATE: &str = "STATE";
/// Supersession or re-enqueue against a newer order (p2).
pub const STALE_ORDER: &str = "STALE_ORDER";
/// Successful completion of a `wait_for_children` parent with unfinished children (p2).
pub const CHILDREN_PENDING: &str = "CHILDREN_PENDING";

// --- resource_exhausted (429) --------------------------------------------
/// A queue-depth dimension (`queue`, `tenant`, `domain_global`, `deferred`) is full.
pub const QUEUE_DEPTH_LIMIT: &str = "QUEUE_DEPTH_LIMIT";
/// A concurrency dimension (`queue_global`, `queue_tenant`, `type_global`, `type_tenant`) is full.
pub const CONCURRENCY_LIMIT: &str = "CONCURRENCY_LIMIT";
/// An event-wait dimension (`tenant`, `event_type`, `unkeyed`) is full.
pub const EVENT_WAIT_LIMIT: &str = "EVENT_WAIT_LIMIT";

// --- permission_denied (403) ---------------------------------------------
/// A creator-authority check denied the create.
pub const CREATOR_AUTHORITY: &str = "CREATOR_AUTHORITY";
/// A creator-authority check could not be decided.
pub const CREATOR_AUTHORITY_UNVERIFIABLE: &str = "CREATOR_AUTHORITY_UNVERIFIABLE";
/// The caller's `consume` check on an event condition was denied.
pub const EVENT_CONSUME_DENIED: &str = "EVENT_CONSUME_DENIED";
/// The caller's `consume` check on an event condition is not enforceable.
pub const EVENT_CONSUME_UNVERIFIABLE: &str = "EVENT_CONSUME_UNVERIFIABLE";

// --- invalid_argument (400) ----------------------------------------------
/// A p2/p3 feature requested in an earlier phase.
pub const FEATURE_NOT_AVAILABLE: &str = "FEATURE_NOT_AVAILABLE";
/// The task type is not registered.
pub const UNKNOWN_TASK_TYPE: &str = "UNKNOWN_TASK_TYPE";
/// Queue, type, predecessor or parent of another task domain.
pub const TASK_DOMAIN_MISMATCH: &str = "TASK_DOMAIN_MISMATCH";
/// More predecessors than `max_predecessors_per_task`.
pub const DEPENDENCY_GRAPH_TOO_LARGE: &str = "DEPENDENCY_GRAPH_TOO_LARGE";
/// More task IDs than the bulk size limit.
pub const BATCH_TOO_LARGE: &str = "BATCH_TOO_LARGE";
/// Unknown event type on an event condition.
pub const UNKNOWN_EVENT_TYPE: &str = "UNKNOWN_EVENT_TYPE";
/// The event type's topic is not waitable.
pub const EVENT_TOPIC_NOT_WAITABLE: &str = "EVENT_TOPIC_NOT_WAITABLE";
/// The event type allows several subject types and none was named.
pub const EVENT_SUBJECT_TYPE_REQUIRED: &str = "EVENT_SUBJECT_TYPE_REQUIRED";
/// The named subject type is not allowed for the event type.
pub const EVENT_SUBJECT_TYPE_NOT_ALLOWED: &str = "EVENT_SUBJECT_TYPE_NOT_ALLOWED";

// --- paging (invalid_argument 400) ---------------------------------------
/// `limit=0`.
pub const INVALID_LIMIT: &str = "INVALID_LIMIT";
/// Malformed or foreign cursor.
pub const INVALID_CURSOR: &str = "INVALID_CURSOR";
/// A cursor sent with a different `$filter`.
pub const FILTER_MISMATCH: &str = "FILTER_MISMATCH";
/// `$orderby` sent with a cursor.
pub const ORDER_WITH_CURSOR: &str = "ORDER_WITH_CURSOR";
/// A cursor sent with a different `$select` or `view`.
pub const FIELD_SELECTION_MISMATCH: &str = "FIELD_SELECTION_MISMATCH";
/// `$count`, `$skip`, `$expand` or `$search`.
pub const UNSUPPORTED_QUERY_PARAM: &str = "UNSUPPORTED_QUERY_PARAM";
