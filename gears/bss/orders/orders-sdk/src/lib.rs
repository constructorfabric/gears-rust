//! Orders shared identities and the typed SDK entry point ([`OrdersV1`]).
//!
//! The gear's local client serves the delivered operations (draft authoring and the draft reads)
//! and answers unavailable for the others. The complete registered surface is
//! [`catalog::OPERATIONS`]; further business methods land with their implementation packages.
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
pub use api::OrdersV1;
pub use errors::OrdersError;
