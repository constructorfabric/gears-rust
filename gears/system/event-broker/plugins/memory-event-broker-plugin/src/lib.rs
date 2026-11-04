//! In-process storage backend for the event-broker gear.
//!
//! Meets the backend contract (`event_broker_sdk::backend`) with everything
//! held in the process, so it serves `standalone` mode and tests. Built for a
//! cluster role, the provider answers `InvalidConfig` unless the entry sets the
//! test-only `single_process: true`, for a test process that runs every role
//! in-process: otherwise a delivery process would never see an ingest
//! process's events. Backends built in one process that name the same store
//! share its events, fence state and heads, so a rebuilt instance keeps them.
//!
//! `provision` answers `Ready`, never `Pending`, and applies
//! [`LogClaim::check`] before it touches the store: a `Held` claim over a
//! topic the store holds nothing for answers `NotProvisioned` and creates no
//! log, and a claim over a log held under an id it neither names nor replaces
//! answers `LogConflict`.
//!
//! [`LogClaim::check`]: event_broker_sdk::backend::LogClaim::check

mod backend;
mod provider;
mod settings;
mod store;

#[cfg(test)]
mod backend_tests;
#[cfg(test)]
mod provider_tests;
#[cfg(test)]
mod settings_tests;

pub use backend::MemoryBackend;
pub use provider::{BACKEND_TYPE, MemoryBackendProvider};
