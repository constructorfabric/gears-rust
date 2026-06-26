// Created: 2026-06-04 by Constructor Tech
//! Distributed-lock domain types: the capability requirement and the
//! native-features descriptor a backend declares.

/// A capability a consumer can require of a distributed-lock backend at
/// resolution time. Each variant maps to a concrete backend characteristic
/// check (DESIGN §3.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LockCapability {
    /// Require the backend to declare linearizable lock semantics
    /// ([`LockFeatures::linearizable`] is `true`) — the condition under which
    /// the lock provides correctness-grade mutual exclusion rather than merely
    /// advisory coordination.
    Linearizable,
}

/// Native capability flags a distributed-lock backend declares via
/// [`DistributedLockBackend::features`](crate::lock::DistributedLockBackend::features).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LockFeatures {
    /// Whether the backend provides linearizable mutual exclusion. Required for
    /// correctness-critical exclusion; eventually-consistent backends may
    /// transiently grant the same lock to two holders under partition.
    pub linearizable: bool,
}

impl LockFeatures {
    /// Creates a features descriptor.
    #[must_use]
    pub fn new(linearizable: bool) -> Self {
        Self { linearizable }
    }
}
