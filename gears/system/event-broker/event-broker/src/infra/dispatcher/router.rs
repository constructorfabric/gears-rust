//! Delivery-routing handler. Cache-based lookup, new-group CAS placement, and
//! SD-gated failover cache invalidation (`DESIGN.md` "Dispatcher Routing") are
//! out of scope - see `eb-dispatcher-routing` design.md D1; #4438 owns any real
//! per-instance routing algorithm. `handle` resolves a delivery instance via
//! `DirectoryService` and forwards to it.

use std::sync::Arc;

use axum::Extension;
use axum::extract::Request;
use axum::response::Response;
use toolkit::api::canonical_prelude::CanonicalError;

use super::forward::{DispatcherState, Role, forward};

/// Correct only when exactly one delivery instance is registered
/// (design.md D11): round-robin selection (`forward()`'s pick, D8)
/// combined with no group-ownership stickiness (deferred to #4438) means a
/// consumer's JOIN and its subsequent stream could land on different
/// delivery instances if more than one is registered, breaking the
/// session. Not runtime-enforced - this is a documentation-only boundary
/// until #4438 lands real ownership-aware routing.
#[derive(Debug, Default)]
pub struct DeliveryRouter;

impl DeliveryRouter {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Axum handler for every Delivery-classified route (`DESIGN.md` "Dispatcher
/// Routing Table"), registered by `register_dispatcher_routes`. `state` is
/// attached once via `.layer(Extension(...))` in `module.rs`.
///
/// # Errors
/// Returns `CanonicalError::ServiceUnavailable`, with the generic body, when no
/// delivery instance is registered or the resolved instance is unreachable;
/// which one is in the `warn` line `forward` logs.
pub async fn handle(
    Extension(state): Extension<Arc<DispatcherState>>,
    req: Request,
) -> Result<Response, CanonicalError> {
    forward(Role::Delivery, &state, req).await
}
