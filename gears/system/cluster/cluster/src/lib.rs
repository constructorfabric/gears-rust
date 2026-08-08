//! # Cluster gear
//!
//! `cluster` (`cf-gears-cluster`) is the cluster gear (DESIGN §3.4 / §3.7,
//! component `cpt-cf-clst-component-wiring`). It registers the per-profile,
//! per-primitive coordination backends produced by cluster plugins into the
//! `ClientHub` — under the stable `cluster:{profile}` scope the SDK resolvers look
//! them up in — and owns the cluster lifecycle.
//!
//! The crate plays two roles, in line with the platform's one-gear-per-domain
//! layout (`<gear>-sdk` + `<gear>` + plugins):
//!
//! 1. **The gear** — a `RunnableCapability` (`name = "cluster"`) whose `start`
//!    builds the wiring from operator config and whose `stop` tears it down. See
//!    the private `gear` module.
//! 2. **An embeddable library** — [`ClusterWiring::builder`]`(hub).…build_and_start()
//!    ->` [`ClusterHandle`] (and [`ClusterWiring::from_config`]) are `pub`, so a
//!    consumer gear may own the wiring directly instead of depending on the
//!    `cluster` gear. [`ClusterHandle::stop`] is the single shutdown entry point.
//!
//! DESIGN §3.7 originally specified the wiring as a non-gear library owned by a
//! separate host gear (the outbox analogy). That was collapsed into this single
//! gear crate — the builder/handle library still exists and is embeddable, but the
//! reusable surface is `cluster-sdk`, so a dedicated wiring crate added a third
//! core crate no other gear has. See DESIGN §3.7 (amended).

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod defaults;

mod config;
mod gear;
mod provider;
mod wiring;

pub use config::{BackendBinding, ClusterConfig, DEFAULT_PROVIDER, ProfileConfig, SecretRef};
pub use provider::ProviderRegistry;
pub use wiring::{ClusterHandle, ClusterWiring, ClusterWiringBuilder, ProfileBackends};

// Re-exported for convenience: plugins implement these from the SDK, but the
// config-driven wiring API surfaces them here too.
pub use cluster_sdk::{
    ClusterCacheProvider, ClusterLeaderElectionProvider, ClusterLockProvider,
    ClusterServiceDiscoveryProvider, StopHook,
};
