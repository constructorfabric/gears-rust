//! Orders Lifecycle shared identities and the first typed SDK entry point.
//!
//! The runtime scaffold returns unavailable. Additional business methods land with their
//! implementation packages; the complete planned surface is in [`catalog::OPERATIONS`].
#![forbid(unsafe_code)]
// D-184: the crate-local clippy.toml disallows `AccessScope::allow_all`.
#![deny(clippy::disallowed_methods)]

pub mod api;
pub mod authoring;
pub mod catalog;
pub mod commercial;
pub mod errors;
pub mod models;
pub mod reads;
pub use api::OrdersLifecycleV1;
pub use errors::OrdersError;
