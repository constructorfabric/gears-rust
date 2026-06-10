// Created: 2026-06-03 by Constructor Tech
//! Leader-election primitive — named single-leader election with automatic
//! renewal, configurable failover timing, dual observability (event-driven and
//! gate-driven), graceful step-down, and explicitly advisory semantics.
//!
//! Provides the [`LeaderElectionBackend`] plugin trait, the [`LeaderElectionV1`]
//! facade, the [`LeaderWatch`] handle with its watch-union event shape, the
//! validated [`ElectionConfig`], and the fluent [`LeaderElectionResolverBuilder`]
//! with startup capability validation.
//!
//! Election is **advisory** coordination (DESIGN §3.3): the leadership signal is
//! a cached snapshot and correctness-critical exclusion must layer a distributed
//! lock or cache compare-and-swap on top — see [`LeaderElectionV1`].
//!
//! `scoped()` (DECOMPOSITION §2.7) and `LeaderWatch::auto_restart`
//! (DECOMPOSITION §2.8) are intentionally out of scope for this primitive and
//! delivered by their dedicated features.

pub mod backend;
pub mod facade;
pub mod resolver;
mod scoped;
pub mod types;
pub mod watch;

pub(crate) use scoped::ScopedLeaderElectionBackend;

pub use backend::LeaderElectionBackend;
pub use facade::LeaderElectionV1;
pub use resolver::{LeaderElectionResolverBuilder, validate_leader_election_capabilities};
pub use types::{ElectionConfig, LeaderElectionCapability, LeaderElectionFeatures, LeaderStatus};
pub use watch::{
    LeaderWatch, LeaderWatchEvent, LeaderWatchSender, ResignReceiver, ResignResponder,
};
