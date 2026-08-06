//! Background workers: `reaper` (expired subscriptions + idempotency-key
//! cleanup - `DESIGN.md` §3.7 Key Invariants states the storage backend
//! owns all event deletion, so no cleaner/retention worker exists here,
//! `docs/ADR/0007-service-decomposition.md` D5) and `ingest_outbox` (the
//! `toolkit-db` outbox's `LeasedMessageHandler` draining the ingest publish
//! path, design.md D5).

pub mod ingest_outbox;
pub mod reaper;

pub use ingest_outbox::IngestOutboxHandler;
pub use reaper::Reaper;
