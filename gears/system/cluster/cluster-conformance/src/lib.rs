// Created: 2026-06-24 by Constructor Tech
//! # Cluster backend conformance suites
//!
//! `cluster_conformance` is the **keystone** of the cluster testing strategy
//! (see `docs/TESTING-STRATEGY.md` §4): a set of parametrized suites, each
//! generic over a backend *factory*, that assert the cluster contract
//! independent of *which* backend implements it.
//!
//! This is the only mechanism that operationalizes
//! `cpt-cf-clst-nfr-cross-backend-stability` — "a consumer gear's behavior MUST
//! NOT change when an operator switches the backend" is a claim you can only
//! test by running **one shared test body against every backend**. A plugin's
//! integration test then reduces to ~10 lines: build the real backend in a
//! container, hand it to the matching `run_*_conformance` entry point.
//!
//! ## Capability-gated assertions
//!
//! Each suite reads the backend's `features()` / `consistency()` and asserts
//! strict guarantees *only* where the backend claims them (e.g.
//! single-leader-under-contention only when
//! [`LeaderElectionFeatures::linearizable`](cluster_sdk::LeaderElectionFeatures)
//! is `true`). A backend that lies about a capability fails the suite — this is
//! how the honest-declaration requirement
//! (`cpt-cf-clst-fr-validation-honest-declaration`) is tested.
//!
//! ## Entry-point shape
//!
//! Each scenario is its own `pub async fn scenario_<id>(backend)` so a plugin
//! can run the whole suite or cherry-pick one case. The `run_*_conformance`
//! runners build a **fresh backend per scenario** via the supplied `make`
//! closure, so state never leaks between scenarios.
//!
//! The suites are plain `async fn`s the caller drives (rather than
//! macro-generated `#[tokio::test]` cases) so they compose with the simulated
//! runtimes used by L4 DST (`turmoil`/`madsim`). This resolves the §11 open
//! question for the first scaffold; revisit if a backend needs per-scenario
//! `#[tokio::test]` isolation.
//!
//! ## Scenario coverage
//!
//! Every scenario maps to an `SC-*` row in the
//! [scenario catalog](../docs/scenarios/README.md). Rows still marked ☐/◑ in the
//! catalog appear here as scenario functions with a `// TODO(SC-*)` marker until
//! the behavior (and, for L4 rows, the fault-injection harness) lands.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![allow(
    clippy::expect_used,
    reason = "the conformance suite's job is to assert: an `expect`/`panic` IS the test failure, with a message naming the violated SC-* scenario"
)]
#![allow(
    clippy::missing_panics_doc,
    reason = "every scenario panics by design when the contract is violated; documenting `# Panics` on each would restate the assertion"
)]

pub mod cache;
pub mod discovery;
pub mod fixture;
pub mod leader;
pub mod lock;
pub mod model;
pub mod restart;
pub mod watch_lifecycle;

pub use cache::run_cache_conformance;
pub use discovery::run_discovery_conformance;
pub use fixture::MemCache;
pub use leader::run_leader_conformance;
pub use lock::run_lock_conformance;
pub use model::{CacheModel, CacheOp, VersionTarget, replay_against_model};
pub use restart::run_restart_conformance;
pub use watch_lifecycle::run_watch_lifecycle_conformance;
