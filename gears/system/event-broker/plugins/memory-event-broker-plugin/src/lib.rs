//! In-process storage backend for the event-broker gear.
//!
//! Meets the backend contract (`event_broker_sdk::backend`) with everything
//! held in the process: the gear's test backend, and the standalone binary's
//! backend when nothing durable is configured. Backends built in one process
//! that name the same store share its events, fence state and heads, so a
//! rebuilt instance keeps them.

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
