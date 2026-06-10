// Created: 2026-06-04 by Constructor Tech
//! Distributed lock primitive — TTL-bounded mutual exclusion with non-blocking
//! and blocking-with-timeout acquisition, explicit asynchronous release, and
//! TTL extension for long-running operations.
//!
//! Provides the [`DistributedLockBackend`] plugin trait, the [`DistributedLockV1`]
//! facade, the [`LockGuard`] handle with its typed command channel, the
//! capability/features descriptors, and the fluent [`LockResolverBuilder`] with
//! startup capability validation.
//!
//! Cleanup safety is provided by **TTL**, not by Rust `Drop`: the guard has a
//! no-op drop and exposes **no fencing tokens**. Code holding a [`LockGuard`]
//! MUST NOT make remote I/O calls inside the critical section (ADR-002,
//! DESIGN §2.2/§3.3); that rule — enforced by the separate lock-misuse lint
//! feature (DECOMPOSITION §2.10) — eliminates the stale-writer scenario fencing
//! tokens would otherwise guard against.
//!
//! `scoped()` (DECOMPOSITION §2.7) is intentionally out of scope for this
//! primitive and delivered by the dedicated scoping feature.

pub mod backend;
pub mod facade;
pub mod guard;
pub mod resolver;
mod scoped;
pub mod types;

pub(crate) use scoped::ScopedDistributedLockBackend;

pub use backend::DistributedLockBackend;
pub use facade::DistributedLockV1;
pub use guard::{LockCommandReceiver, LockGuard, LockRequest, LockResponder};
pub use resolver::{LockResolverBuilder, validate_lock_capabilities};
pub use types::{LockCapability, LockFeatures};
