//! Transactional outbox pattern for reliable event publishing.
//!
//! This module provides a general-purpose transactional outbox mechanism.
//! Producers enqueue events inside their domain transaction using [`enqueue`],
//! and a background dispatcher claims and delivers them via [`OutboxStore`].
//!
//! # Architecture
//!
//! - **`enqueue`**: Inserts a row into `modkit_outbox_events` inside the caller's
//!   existing transaction. Supports idempotent enqueue via `dedupe_key`.
//! - **`OutboxStore`**: Dispatcher-side API for claiming, acknowledging, and
//!   nacking outbox rows with lease-based concurrency and exponential backoff.
//! - **`OutboxDispatcher`**: Generic poll→claim→publish→ack/nack loop that
//!   modules wire into their lifecycle entry point with a publish callback.
//! - **`setup_outbox_table`** / **`outbox_migrations`**: Migration helpers to
//!   create the shared `modkit_outbox_events` infrastructure table.
//!
//! # Example
//!
//! ```ignore
//! use modkit_db::outbox::{enqueue, OutboxMessage, OutboxStore, RetryCfg, ClaimCfg};
//! use modkit_db::outbox::setup_outbox_table;
//!
//! // 1. Run migration once at startup.
//! setup_outbox_table(&db).await?;
//!
//! // 2. Enqueue inside a domain transaction.
//! db.transaction_ref(|tx| Box::pin(async move {
//!     // ... domain side effects ...
//!     enqueue(tx, OutboxMessage {
//!         namespace: "my-module",
//!         topic: "events",
//!         tenant_id: Some(tenant_id),
//!         dedupe_key: Some(format!("{tenant_id}/{event_id}")),
//!         payload: serde_json::json!({"event": "created"}),
//!     }).await?;
//!     Ok(())
//! })).await?;
//!
//! // 3. Dispatcher claims and publishes.
//! let store = OutboxStore::new(db_provider, worker_id, "my-module".into(), retry_cfg);
//! let batch = store.claim_batch(claim_cfg).await?;
//! for msg in batch {
//!     match publish(&msg).await {
//!         Ok(()) => store.ack(msg.id).await?,
//!         Err(e) => store.nack(msg.id, &e.to_string()).await?,
//!     }
//! }
//! ```

mod dispatcher;
mod enqueue;
mod helpers;
mod migration;
mod store;
mod types;

pub use dispatcher::OutboxDispatcher;
pub use enqueue::enqueue;
pub use migration::{outbox_migrations, setup_outbox_table};
pub use store::OutboxStore;
pub use types::{ClaimCfg, ClaimedMessage, OutboxMessage, OutboxStatus, RetryCfg};
