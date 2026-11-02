//! Orders shared identities and the typed SDK entry point ([`OrdersV1`]).
//!
//! The gear's local client serves the delivered operations (draft authoring and the draft reads)
//! and answers unavailable for the others. The complete registered surface is
//! [`catalog::OPERATIONS`]; further business methods land with their implementation packages.
//!
//! Subscription-side cardinality is advisory at order time. Neither a gate nor an activation
//! re-check pass guarantees admission. Subscriptions must enforce cardinality atomically at
//! activation; the submit/activation path is not production-ready until that upstream contract
//! is agreed and delivered (D-180). Workflow must handle later overlap collisions through its
//! failure acknowledgement and compensation path.
#![forbid(unsafe_code)]
// D-184: the crate-local clippy.toml disallows `AccessScope::allow_all`.
#![deny(clippy::disallowed_methods)]

pub mod api;
pub mod authoring;
pub mod catalog;
pub mod commercial;
pub mod errors;
pub mod gate_report;
pub mod models;
pub mod preview;
pub mod reads;
pub use api::OrdersV1;
pub use errors::OrdersError;
