//! Background workers. Only `reaper` (expired subscriptions +
//! idempotency-key cleanup) is broker-owned - `DESIGN.md` §3.7 Key
//! Invariants states the storage backend owns all event deletion, so no
//! cleaner/retention worker exists here
//! (`docs/ADR/0007-service-decomposition.md` D5).

pub mod reaper;

pub use reaper::Reaper;
