//! `Storage` facade (`ConsumerGroupRepo`/`CursorRepo`/`RoutingMarkers`/
//! `ActiveStreamMarker`).
//!
//! The event log itself is not here. It belongs to whichever storage backend a
//! topic's settings name - each one a plugin crate implementing
//! `event_broker_sdk::backend::Backend` - and backend RESOLUTION lives in
//! `domain::backend`
//! (`BackendResolver`/`SingleBackendResolver`).

pub mod entity;
pub mod error;
pub mod facade;
pub mod migrations;
mod producer_chain;

pub use facade::Storage;
