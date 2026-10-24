//! BSS Rating gear.
//!
//! The public contract lives in `bss-rating-sdk` and is re-exported here. The foundation
//! (feature 01) provides the shared runtime every pipeline feature builds on: the `bss_rating`
//! namespace and its migrations, the outbox work queues, the tenancy and operation matrix with
//! audited operator actions, the inbox for every non-usage upstream fact, the exception register,
//! the operation idempotency store, advisory leadership over the Cluster SDK, gated event intake
//! and the provider test doubles.
#![forbid(unsafe_code)]

pub use bss_rating_sdk as sdk;

pub mod gear;
pub use gear::BssRatingGear;

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
#[cfg(feature = "test-providers")]
pub mod test_providers;
#[cfg(test)]
mod test_support;
