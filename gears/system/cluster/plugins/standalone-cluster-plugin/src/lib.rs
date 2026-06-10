// Created: 2026-06-11 by Constructor Tech
//! # Standalone cluster plugin
//!
//! `standalone_cluster_plugin` is the in-process, zero-infrastructure backend for
//! the cluster gear — the default for developer laptops and unit/integration
//! tests (PRD §3.1, DESIGN §3.11). It implements the cache primitive natively
//! over an in-memory store and derives leader election, distributed lock, and
//! service discovery from the SDK's cache-based default backends, so a single
//! process gets all four cluster primitives with no external dependencies.
//!
//! ## Lifecycle (outbox-style builder/handle, DESIGN §3.7)
//!
//! This plugin is **not** registered as a `RunnableCapability`. It exposes a
//! builder/handle pair owned by a parent host gear (or, in follow-ups, by the
//! cluster wiring crate):
//!
//! ```no_run
//! # async fn doc() -> Result<(), cluster_sdk::ClusterError> {
//! use standalone_cluster_plugin::StandaloneClusterPlugin;
//!
//! let handle = StandaloneClusterPlugin::builder().build_and_start()?;
//! // Hand the backends to the wiring crate / register them in the ClientHub:
//! let _cache = handle.cache();
//! let _leader = handle.leader_election();
//! let _lock = handle.lock();
//! let _discovery = handle.service_discovery();
//! // On graceful shutdown:
//! handle.stop().await;
//! # Ok(())
//! # }
//! ```
//!
//! `build_and_start` starts the cache's background TTL sweeper; `handle.stop()`
//! cancels it. The plugin performs no remote I/O and never relies on `Drop` for
//! cleanup.
//!
//! ## Status
//!
//! The cache is native. Leader election, lock, and service discovery currently
//! ride the SDK default backends (`CasBased*` / `CacheBased*`) over the native
//! cache — the "implement cache only, get all four" guarantee (PRD
//! §5.5, DESIGN §3.11). Native implementations of those three (the DESIGN §3.11
//! end state) are a follow-up optimization.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

mod cache;
mod config;
mod plugin;
mod provider;

pub use cache::StandaloneCache;
pub use config::StandaloneClusterConfig;
pub use plugin::{StandaloneClusterBuilder, StandaloneClusterHandle, StandaloneClusterPlugin};
pub use provider::{PROVIDER_NAME, StandaloneCacheProvider};
