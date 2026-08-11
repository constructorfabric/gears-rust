//! `OperationBuilder` route registration (`DESIGN.md:586`). Real
//! registration (DTOs, `OpenAPI` schemas, `OperationBuilder` chains) lands
//! with #4346 - these shells exist so `module.rs`'s per-mode gating has a
//! named registration point per route group to call into once bodies
//! exist. None are called from `module.rs` yet.

use axum::Router;

pub fn register_ingest_routes(router: Router) -> Router {
    router
}

pub fn register_delivery_routes(router: Router) -> Router {
    router
}

pub fn register_dispatcher_routes(router: Router) -> Router {
    router
}
