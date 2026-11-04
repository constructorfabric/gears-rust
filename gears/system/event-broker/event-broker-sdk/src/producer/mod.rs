mod direct;
mod event_factory;
#[cfg(feature = "db")]
mod migrations;
#[cfg(feature = "outbox")]
mod partitioning;
#[cfg(feature = "db")]
mod registration;
mod schema_cache;
mod types;

#[cfg(test)]
mod direct_tests;
#[cfg(all(test, feature = "outbox"))]
mod outbox_tests;
#[cfg(all(test, feature = "outbox"))]
mod partitioning_tests;

#[cfg(feature = "db")]
mod db;
#[cfg(feature = "outbox")]
mod outbox;

pub use crate::api::{IngestOutcome, ProducerCursors, ProducerMode, TopicCursors};
pub use direct::{Producer, ProducerBuilder};
pub use types::{DirectDeduplication, ProducerIdentity, ValidationTiming};

#[cfg(feature = "db")]
pub use db::{DbProducer, DbProducerBuilder};
#[cfg(feature = "db")]
pub use migrations::producer_registration_migrations;
#[cfg(feature = "outbox")]
pub use outbox::{
    ProducerOutbox, ProducerOutboxEnvelope, ProducerOutboxHandle, ProducerOutboxQueue, TEST_QUEUE,
};
#[cfg(feature = "db")]
pub use types::{DbDeduplication, ManagedDeduplication};
