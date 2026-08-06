//! `Storage` facade (`ConsumerGroupRepo`/`CursorRepo`/`SubscriptionRepo`/
//! `ActiveStreamMarker`/`DeliveryNotifier`) and the real, durable SQLite
//! `EventBrokerBackend` (`builtin::sqlite::SqliteEventBackend`,
//! eb-single-process-implementation D3). Backend RESOLUTION - which
//! backend serves a given topic - lives in `domain::backend`
//! (`BackendResolver`/`SingleBackendResolver`), not here; the planned
//! `StorageBackendRegistry` shell (`DESIGN.md` §"Storage Backend Plugin
//! System") this module used to hold was deleted as dead code once that
//! became clear (never constructed anywhere in the crate).

pub mod builtin;
pub mod entity;
pub mod error;
pub mod migrations;
pub mod storage;

pub use storage::Storage;
