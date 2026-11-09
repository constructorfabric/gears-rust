//! Task Engine SDK.
//!
//! The public API of the `task-engine` gear, as described by
//! `gears/task-engine/docs/openapi.yaml`:
//!
//! - [`api`] — [`TaskEngineApi`], the transport-agnostic contract (`#[toolkit::contract]`),
//!   one method per REST operation. Resolved from `ClientHub` as
//!   `Arc<dyn TaskEngineApi>`, whether the engine runs in-process or remote.
//! - [`TaskEngineApiRest`] — the REST projection (`#[toolkit::rest_contract]`)
//!   under `/task_engine/v1`. With the `rest-client` feature it generates
//!   `TaskEngineApiRestClient` / `TaskEngineApiRestResolvingClient`; with
//!   `rest-server` it generates route registration for the gear.
//! - [`models`] — request, response and query DTOs.
//! - [`gts`] — GTS identifiers the Task Engine owns (base types, entry types,
//!   queues, lifecycle event types, permissions).
//! - [`test_support`] — [`UnimplementedTaskEngine`], a placeholder impl whose
//!   every method fails `unimplemented`.
//! - [`error`] — resource-error constructors; [`reason`] — the Task Engine
//!   error codes, by the slot that carries them.
//!   Every method fails with a platform [`CanonicalError`]; the Task Engine
//!   defines no error types of its own.
//!
//! ```ignore
//! let engine = hub.get::<dyn TaskEngineApi>()?;
//! let task = engine.get_task(ctx, task_id).await?;
//! ```
//!
//! [`CanonicalError`]: toolkit_canonical_errors::CanonicalError

#![forbid(unsafe_code)]

pub mod api;
pub mod error;
pub mod gts;
pub mod models;
pub mod reason;
pub mod rest;
pub mod test_support;

pub use api::{TaskEngineApi, task_engine_api_ir};
pub use error::{EntryResourceError, QueueResourceError, TaskResourceError};
pub use rest::{TaskEngineApiRest, task_engine_api_rest_http_binding};
pub use test_support::UnimplementedTaskEngine;

#[cfg(feature = "rest-client")]
pub use rest::{TaskEngineApiRestClient, TaskEngineApiRestResolvingClient};
