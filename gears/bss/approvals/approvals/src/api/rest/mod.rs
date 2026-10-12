//! REST doors under `/bss-approvals/v1`.

mod dto;
mod handlers;
mod routes;

#[cfg(test)]
#[path = "doors_tests.rs"]
mod doors_tests;

/// The doors over `state`, for the gear and for a test that installs its own names.
pub use routes::router;
