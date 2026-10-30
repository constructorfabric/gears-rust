//! Background workers.
//!
//! - `ingest_outbox`: the `toolkit-db` outbox's `LeasedMessageHandler`
//!   draining the ingest publish path.
//! - `reaper`: expired subscriptions and idempotency-key cleanup
//!   (`DESIGN.md` §3.7 Key Invariants).

pub mod ingest_outbox;
pub mod reaper;
pub mod specification_refresh;

pub use ingest_outbox::IngestOutboxHandler;
pub use reaper::Reaper;
pub use specification_refresh::SpecificationRefreshWorker;
