//! Transactional outbox for reliable asynchronous message production.
//!
//! # Architecture
//!
//! Four-stage pipeline: **incoming → sequencer → outgoing → processor**.
//!
//! 1. **Enqueue** — messages are written atomically within business transactions
//!    to the `incoming` table via [`Outbox::enqueue()`].
//! 2. **Sequencer** — a background task claims incoming rows, assigns
//!    per-partition sequence numbers, and writes to the `outgoing` table.
//! 3. **Processor** — one long-lived task per partition reads from `outgoing`,
//!    dispatches to the registered handler, and acks via cursor advance
//!    (append-only — no deletes on the hot path).
//! 4. **Vacuum** — a standalone background task (peer of the sequencer) that
//!    garbage-collects processed outgoing and body rows across dirty partitions.
//!
//! # Processing modes
//!
//! - **Transactional** — handler runs inside the DB transaction holding the
//!   partition lock. Provides exactly-once semantics within the database.
//! - **Decoupled** — handler runs outside any transaction, with lease-based
//!   locking. Provides at-least-once delivery; handlers must be idempotent.
//!
//! # Usage
//!
//! ```ignore
//! let handle = Outbox::builder(db)
//!     .profile(OutboxProfile::low_latency())
//!     .queue("orders", Partitions::of(4))
//!         .decoupled(my_handler)
//!     .start().await?;
//! // ... enqueue via handle.outbox() ...
//! handle.stop().await;
//! ```
//!
//! # Backend notes
//!
//! - **`PostgreSQL`** — Full support. Uses `FOR UPDATE SKIP LOCKED` for partition
//!   locking and `INSERT ... RETURNING` for body ID retrieval.
//! - **`MySQL` 8.0+** — Requires `MySQL` 8.0 or later for `FOR UPDATE SKIP LOCKED`
//!   support (added in 8.0.1). Earlier versions will fail at runtime when
//!   attempting to acquire partition locks. Uses `LAST_INSERT_ID()` for body IDs.
//! - **`SQLite`** — Single-process only. `SQLite` has no row-level locking; the
//!   outbox relies on `SQLite`'s single-writer model. Suitable for development,
//!   testing, and single-instance deployments. Not recommended for production
//!   multi-process scenarios.
//!
//! # Dead letters
//!
//! Messages that a handler permanently rejects ([`HandlerResult::Reject`]) are
//! moved to a dead-letter table with the original payload, partition, sequence,
//! and error reason preserved. The outbox does **not** auto-replay dead letters;
//! that policy is owned by the application.
//!
//! Dead letter operations are available as methods on [`Outbox`]:
//! [`dead_letter_list`](Outbox::dead_letter_list),
//! [`dead_letter_count`](Outbox::dead_letter_count),
//! [`dead_letter_replay`](Outbox::dead_letter_replay),
//! [`dead_letter_resolve`](Outbox::dead_letter_resolve),
//! [`dead_letter_reject`](Outbox::dead_letter_reject),
//! [`dead_letter_discard`](Outbox::dead_letter_discard), and
//! [`dead_letter_cleanup`](Outbox::dead_letter_cleanup).
//!
//! Dead letters have a status lifecycle: `pending → reprocessing → resolved`
//! (or `pending → discarded`). The [`DeadLetterStatus`] enum tracks this.
//!
//! ## Example: application-level consumption
//!
//! The library provides the building blocks; the application decides **when**
//! and **how** to use them. `dead_letter_replay` claims messages (sets them
//! to `reprocessing` with a deadline) and returns them — the application
//! then processes and calls `resolve` or `reject`.
//!
//! ```ignore
//! use std::time::Duration;
//!
//! let scope = DeadLetterScope::default().payload_type("order.created");
//! let msgs = outbox.dead_letter_replay(&conn, &scope, Duration::from_secs(60)).await?;
//! for msg in &msgs {
//!     match my_handler(&msg.payload).await {
//!         Ok(_)  => outbox.dead_letter_resolve(&conn, &[msg.id]).await?,
//!         Err(e) => outbox.dead_letter_reject(&conn, &[msg.id], &e.to_string()).await?,
//!     };
//! }
//! ```

mod builder;
mod core;
mod dead_letter;
mod dialect;
mod handler;
mod manager;
mod migrations;
pub(crate) mod prioritizer;
pub(crate) mod stats;
mod strategy;
#[doc(hidden)]
pub mod taskward;
mod types;
mod validation;
mod workers;

#[cfg(test)]
#[cfg(feature = "sqlite")]
#[cfg_attr(coverage_nightly, coverage(off))]
mod integration_tests;

pub use builder::QueueBuilder;
pub use core::Outbox;
pub use dead_letter::{DeadLetterFilter, DeadLetterMessage, DeadLetterScope, DeadLetterStatus};
pub use handler::{
    Handler, HandlerResult, MessageHandler, OutboxMessage, PerMessageAdapter, TransactionalHandler,
    TransactionalMessageHandler,
};
pub use manager::{OutboxBuilder, OutboxHandle};
pub use migrations::outbox_migrations;
pub use types::{
    EnqueueMessage, OutboxError, OutboxMessageId, OutboxProfile, Partitions, WorkerTuning,
};

// Internal re-exports for tests and internal modules
