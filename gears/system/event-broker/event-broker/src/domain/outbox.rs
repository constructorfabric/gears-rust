//! Ingest-side outbox constants (design.md D5). The payload itself needs no
//! dedicated envelope type: `IngestServiceImpl::publish_event` enqueues the
//! `domain::model::Event` it already has (partition already stamped) as
//! JSON directly - `infra::workers::ingest_outbox::IngestOutboxHandler`
//! decodes the same type back and reuses `domain::backend::to_sdk_event`
//! unchanged, so nothing here duplicates that conversion.

/// `toolkit_db::outbox` queue name for the ingest publish path.
pub const INGEST_QUEUE_NAME: &str = "evbk-ingest";

/// `payload_type` every ingest outbox row is stamped with.
pub const INGEST_PAYLOAD_TYPE: &str = "application/vnd.event-broker.ingest-event+json;version=1";

/// Partition count `Outbox::builder(..).queue(INGEST_QUEUE_NAME, ..)` is
/// registered with - `toolkit_db::outbox::Partitions::of` requires a power
/// of 2 in `1..=64`; `4` is a reasonable default for a single-process
/// deployment (eb-single-process-implementation D7), not yet
/// operator-configurable.
pub const INGEST_OUTBOX_PARTITIONS: u16 = 4;
