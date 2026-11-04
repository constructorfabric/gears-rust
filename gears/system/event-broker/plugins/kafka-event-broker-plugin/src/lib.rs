//! # Apache Kafka event-broker backend
//!
//! A storage backend for the event-broker gear over an Apache Kafka cluster.
//! It implements `event_broker_sdk::backend::Backend` and ships a
//! `event_broker_sdk::backend::Provider` for the backend type
//! `cf.core.events.backend.v1~cf.core.backend.kafka.v1~`.
//!
//! Each event-broker topic is one Kafka topic and each partition one Kafka
//! partition; an event's sequence is its Kafka offset plus one, so sequences
//! are strictly increasing but may have gaps (transaction markers occupy
//! offsets). An append and its fence record commit in one Kafka transaction
//! of a producer owned by the fence key (`Dedup::Fenced`); heads are polled
//! (`Watch::Poll`); retention is the brokers'. See `docs/DESIGN.md`.

#![forbid(unsafe_code)]

mod append;
mod backend;
mod body;
mod clients;
mod error;
mod fence;
mod names;
mod provider;
mod provision;
mod read;
mod records;
mod settings;
mod shared;
mod tail;

#[cfg(test)]
mod body_tests;
#[cfg(test)]
mod error_tests;
#[cfg(test)]
mod fence_tests;
#[cfg(test)]
mod names_tests;
#[cfg(test)]
mod provision_tests;
#[cfg(test)]
mod records_tests;
#[cfg(test)]
mod settings_tests;

pub use backend::KafkaBackend;
pub use provider::{BACKEND_TYPE, KafkaBackendProvider};
