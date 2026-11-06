//! Errors of the [`TaskEngineApi`](crate::TaskEngineApi) contract.
//!
//! The Task Engine defines no error types of its own: every method fails with a
//! platform [`CanonicalError`](toolkit_canonical_errors::CanonicalError), whose
//! category selects the HTTP status (`invalid_argument` 400 — 413 for size, 428
//! for a missing idempotency key —, `unauthenticated` 401, `permission_denied`
//! 403, `not_found` 404, `already_exists`/`aborted` 409, `failed_precondition`
//! **400**, `resource_exhausted` 429, `service_unavailable` 503, `internal` 500).
//!
//! The resource-error types below build those errors with the right
//! `resource_type`; [`crate::reason`] lists the Task Engine reason codes carried in
//! `context.reason`, field violations and precondition violations.

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
