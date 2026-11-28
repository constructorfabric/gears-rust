//! Private Event Broker memory coordination.
//!
//! `cache.rs` adapts `standalone-cluster-plugin/src/cache.rs`.
//! The cache-backed primitives adapt `cluster/src/defaults/{mod,guard,identity,lease,leader,lock}.rs`.
//! `lock.rs` and `leader.rs` enforce the linearizable cache requirement and create fresh v4
//! owner IDs for guards and candidacies. `lease.rs` owns the separate write nonce.
//!
//! All three implement the cluster SDK backend contracts. `client.rs` binds private, immutable
//! profiles and owns taskward expiry workers and the coordination workers tracked by
//! `guard.rs` and `leader.rs`. Cache TTL checks use elapsed time, so a large finite TTL
//! cannot overflow an absolute deadline. Mutations and fanout share a lock to preserve per-key
//! watch order, and reserved terminal slots preserve shutdown delivery under backpressure.
//! Both lease primitives use whole-byte conditional writes. Election records retain fractional
//! expiry, and lock records retain the SDK's millisecond encoding.

mod cache;
mod guard;
mod lease;
mod lock;

#[cfg(test)]
pub use cache::MemoryCache;
#[cfg(test)]
pub use lock::MemoryLock;

mod client;
mod leader;

#[cfg(test)]
pub use client::PreparedMemory;
pub use client::{EbMemoryOptions, RunningMemory, prepare_memory};
#[cfg(test)]
pub use leader::{MemoryLeader, TerminalGate};
