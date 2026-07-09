//! `coord` — shared BSS coordination primitives.
//!
//! Hosts a DB-backed distributed **lease** (a single-active job/run guard),
//! ported from the account-management lease pattern and generalized for reuse
//! by any BSS gear.
//!
//! It works **entirely within the existing `SecureORM` API**: the `coord_leases`
//! row is unscoped process-coordination state (`#[secure(no_tenant, …)]`)
//! accessed via [`toolkit_security::AccessScope::allow_all`], exactly as AM's
//! `am_leases` does — so adopting it needs **no toolkit changes**.
//!
//! # Usage
//!
//! A gear adds [`migration::Migration`] to its `Migrator`, then builds a
//! [`LeaseManager`] over its `Db` and brackets a job:
//!
//! ```ignore
//! let mgr = coord::LeaseManager::new(db.clone());
//! match mgr.acquire("recognition-run:{tenant}:{period}", ttl).await {
//!     Ok(guard) => { /* run the job */ guard.release().await?; }
//!     Err(coord::CoordError::LeaseHeld) => { /* a peer is already running */ }
//!     Err(e) => return Err(e.into()),
//! }
//! ```
//!
//! `LeaseHeld` means a peer already holds the slot (single-active); the consumer
//! maps it onto its own "already running" outcome.

pub mod lease;
pub mod migration;

pub use lease::{AckError, CoordError, LeaseGuard, LeaseManager, RenewalHandle, RenewalState};
