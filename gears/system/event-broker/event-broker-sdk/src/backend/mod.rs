//! The storage backend contract (feature 0007): the trait every storage
//! backend plugin implements, the values it speaks, and the helpers more than
//! one plugin shares ([`support`]).
//!
//! Backends are plugins linked into the gear, so nothing here carries a
//! `SecurityContext` or derives serde: a backend that reaches a remote store
//! does so with its own credentials, and none of these values crosses a wire.

mod api;
mod append;
mod build;
mod error;
mod ids;
mod provision;
mod read;
pub mod support;
mod watch;

#[cfg(test)]
mod provision_tests;

pub use api::{Backend, Provider};
pub use append::{AppendRequest, Appended, Dedup, Fence, Flush, Head, KeyedEvent};
pub use build::{BuildContext, Role};
pub use error::BackendError;
pub use ids::{FenceKey, FenceSeq, LogId, Partition, TopicPartition};
pub use provision::{LogClaim, Provisioning, Replacing, Retention, TopicSpec};
pub use read::Wait;
pub use watch::{HeadWatch, Watch};
