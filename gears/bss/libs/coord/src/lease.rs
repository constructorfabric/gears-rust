//! Distributed-lease primitive — a DB-backed single-active job/run guard.
//!
//! [`LeaseManager::acquire`] takes the gate (or returns [`CoordError::LeaseHeld`]
//! when a peer holds it); the returned [`LeaseGuard`] renews, releases, fences a
//! write in-tx, or drives a renewal heartbeat. The `coord_leases` row is
//! unscoped (`#[secure(no_tenant, …)]`), so every access scopes with
//! `AccessScope::allow_all()` — the primitive needs nothing beyond the existing
//! `SecureORM` API.

mod entity;
pub mod error;
pub mod guard;
pub mod manager;

#[cfg(test)]
mod sqlite_tests;

pub use error::{AckError, CoordError};
pub use guard::{LeaseGuard, RenewalHandle, RenewalState};
pub use manager::LeaseManager;
