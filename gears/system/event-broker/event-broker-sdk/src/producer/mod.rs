mod direct;
mod event_factory;
#[cfg(feature = "db")]
mod migrations;
mod partitioning;
#[cfg(feature = "db")]
mod registration;
mod schema_cache;
mod types;

#[cfg(all(test, feature = "test-util"))]
mod direct_tests;
#[cfg(test)]
mod partitioning_tests;

#[cfg(feature = "db")]
mod db;
#[cfg(feature = "outbox")]
mod outbox;

pub use crate::api::{IngestOutcome, ProducerCursor, ProducerMode};
pub use direct::{Producer, ProducerBuilder};
pub use types::{DirectDeduplication, ProducerIdentity, ValidationTiming};

#[cfg(feature = "db")]
pub use db::{DbProducer, DbProducerBuilder, UnknownProducerAction};
#[cfg(feature = "db")]
pub use migrations::producer_registration_migrations;
#[cfg(feature = "outbox")]
pub use outbox::{
    ProducerOutbox, ProducerOutboxEnvelope, ProducerOutboxHandle, ProducerOutboxQueue,
};
#[cfg(feature = "db")]
pub use types::{
    DbDeduplication, ManagedDeduplication, MissingProducerRegistration, UnknownProducerRegistration,
};
