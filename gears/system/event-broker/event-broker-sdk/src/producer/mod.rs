pub mod backend;
mod builder;
mod sync_producer;

#[cfg(feature = "outbox")]
mod async_producer;

pub use backend::{IngestOutcome, ProducerBackend, ProducerCursor};
pub use builder::{ProducerBuilder, ProducerMode, ValidationTiming};
pub use sync_producer::SyncProducer;

#[cfg(feature = "outbox")]
pub use async_producer::AsyncProducer;
