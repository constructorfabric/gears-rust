//! Ingest-routing proxy handler. Topic-pattern matching, hetero/sharded
//! modes, and specificity tie-break (`DESIGN.md` "Dispatcher Routing") are out
//! of scope (see `eb-dispatcher-routing` design.md D1; topic-affinity routing
//! is tracked in #5701). `handle` resolves an ingest instance via
//! `DirectoryService` and forwards to it.

use std::sync::Arc;

use axum::Extension;
use axum::extract::Request;
use axum::response::Response;
use toolkit::api::canonical_prelude::CanonicalError;

use super::forward::{DispatcherState, Role, forward};

#[derive(Debug, Default)]
pub struct IngestProxy;

impl IngestProxy {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Axum handler for every Ingest- and Shared-classified route
/// (`DESIGN.md` "Dispatcher Routing Table"), registered by
/// `register_dispatcher_routes`.
/// `state` is attached once via `.layer(Extension(...))` in `module.rs`.
///
/// # Errors
/// Returns `CanonicalError::ServiceUnavailable`, with the generic body, when no
/// ingest instance is registered or the resolved instance is unreachable;
/// which one is in the `warn` line `forward` logs.
pub async fn handle(
    Extension(state): Extension<Arc<DispatcherState>>,
    req: Request,
) -> Result<Response, CanonicalError> {
    forward(Role::Ingest, &state, req).await
}
