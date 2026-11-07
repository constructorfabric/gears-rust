mod direct;
mod event_factory;
#[cfg(feature = "db")]
mod migrations;
#[cfg(feature = "outbox")]
mod partitioning;
#[cfg(feature = "outbox")]
mod registration;
mod schema_cache;
mod types;

#[cfg(test)]
mod direct_tests;
#[cfg(all(test, feature = "outbox"))]
mod outbox_tests;
#[cfg(all(test, feature = "outbox"))]
mod partitioning_tests;

#[cfg(feature = "outbox")]
mod db;
#[cfg(feature = "outbox")]
mod outbox;
#[cfg(feature = "outbox")]
mod outbox_setup;

pub use crate::api::{IngestOutcome, ProducerCursors, ProducerMode, TopicCursors};
pub use direct::{DirectProducer, DirectProducerBuilder};

#[cfg(feature = "db")]
pub use migrations::producer_registration_migrations;
#[cfg(all(feature = "outbox", any(test, feature = "test-util")))]
pub use outbox::TEST_QUEUE;
#[cfg(feature = "outbox")]
pub use outbox_setup::{
    OutboxBinder, OutboxProducer, OutboxProducerHandle, OutboxSetup, OutboxSetupBuilder,
};
#[cfg(feature = "outbox")]
pub use types::{ManagedRegistration, OutboxRegistration};
