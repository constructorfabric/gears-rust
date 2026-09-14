//! BSS Plan & Price Modeling gear.
//!
//! The authoring surface and System of Record for `Plan`, `Price`, bundle /
//! add-on composition and billing descriptors. It never computes a charge,
//! evaluates an overlay, or performs FX: it defines what the pricing primitives
//! MUST contain so that Tariffs can evaluate and Rating can charge
//! deterministically from a frozen snapshot.
//!
//! The crate is laid out as the design set is: a shared **Catalog Foundation**
//! (the publish engine — scope key, draft→publish state machine, fail-closed
//! validation pipeline, append-only history, read-model projection +
//! `pricingSnapshotRef`, event outbox) that capability slices author through.
//! Foundation owns publish; slices own capability policy.
//!
//! No crate-level lint allowances: the workspace bar (`pedantic` + the
//! restriction set, all `deny`) is met as written rather than paid down later.

#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod authz;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod gts;
#[doc(hidden)]
pub mod infra;
#[doc(hidden)]
pub mod module;

/// The shared reading of "is this token code or prose", for the two censuses
/// that walk this crate's own sources. Test-only; see the module's doc for why
/// it is one module and why its file carries the `_tests.rs` suffix.
#[cfg(test)]
#[path = "source_scan_tests.rs"]
mod source_scan;
