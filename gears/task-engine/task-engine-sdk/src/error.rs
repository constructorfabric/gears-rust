//! Errors of the [`TaskEngineApi`](crate::TaskEngineApi) contract.
//!
//! The Task Engine defines no error types of its own: every method fails with a
//! platform [`CanonicalError`](toolkit_canonical_errors::CanonicalError), whose
//! category selects the HTTP status (`invalid_argument` 400 — 413 for size, 428
//! for a missing idempotency key —, `unauthenticated` 401, `permission_denied`
//! 403, `not_found` 404, `already_exists`/`aborted` 409, `failed_precondition`
//! **400**, `resource_exhausted` 429, `service_unavailable` 503, `internal` 500).
//!
//! What the categories mean here:
//!
//! - `not_found`: missing, or not readable by the caller (no existence oracle).
//!   Lease-fenced worker calls never answer it: a caller that is not the
//!   current holder with that token gets `aborted` `LEASE_LOST`.
//! - `permission_denied` `CLAIM_REVOKED` on a worker call: the caller's scope
//!   no longer covers the task, and the task was released.
//! - `already_exists`: `resource_type` is the task base type and
//!   `resource_name` the existing task ID.
//! - `resource_exhausted`: a full quota (queue depth, concurrency, event
//!   waits). Node overload is `service_unavailable`, never
//!   `resource_exhausted`.
//! - `service_unavailable`: PDP unreachable (fail closed: nothing written,
//!   leases kept), GTS registry unreachable for an uncached type, or node
//!   overload. Retrying is safe.
//! - Every `429` and `503` carries `Retry-After` (at least 1 s).
//!
//! The resource-error types below build those errors with the right
//! `resource_type`; [`crate::reason`] lists the Task Engine codes and the slot
//! each travels in (`context.reason` only for `permission_denied`, `aborted`
//! and `unauthenticated`; field violations, precondition violations and quota
//! subjects otherwise).

use toolkit_canonical_errors_macro::resource_error;

/// Errors about a task (`gts.cf.core.tasks.task.v1~`).
#[resource_error(gts_id!("cf.core.tasks.task.v1~"))]
pub struct TaskResourceError;

/// Errors about a queue (`gts.cf.core.tasks.queue.v1~`).
#[resource_error(gts_id!("cf.core.tasks.queue.v1~"))]
pub struct QueueResourceError;

/// Errors about a task entry (`gts.cf.core.tasks.entry.v1~`).
#[resource_error(gts_id!("cf.core.tasks.entry.v1~"))]
pub struct EntryResourceError;
