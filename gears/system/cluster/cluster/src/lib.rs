// Created: 2026-06-11 by Constructor Tech
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
//!
//! ## Per-primitive routing and the omit-default shorthand
//!
//! Each profile binds a cache backend (required) and, optionally, leader-election,
//! lock, and service-discovery backends — possibly served by different plugins
//! (`cpt-cf-clst-fr-routing-per-primitive`). Any primitive left unbound is
//! auto-filled with the SDK default backend over that profile's cache — the
//! "bind a cache, get all four" shorthand (`cpt-cf-clst-fr-routing-omit-default`).
//!
//! ```no_run
//! # async fn doc(
//! #     hub: std::sync::Arc<toolkit::client_hub::ClientHub>,
//! #     cache: std::sync::Arc<dyn cluster_sdk::ClusterCacheBackend>,
//! # ) -> Result<(), cluster_sdk::ClusterError> {
//! use cluster::{ClusterWiring, ProfileBackends};
//! use cluster_sdk::ClusterProfile;
//!
//! #[derive(Clone, Copy)]
//! struct EventBroker;
//! impl ClusterProfile for EventBroker {
//!     const NAME: &'static str = "event-broker";
//! }
//!
//! let handle = ClusterWiring::builder(hub)
//!     // Bind only the cache; leader/lock/discovery come from the SDK defaults.
//!     .profile(EventBroker, ProfileBackends::new(cache))
//!     .build_and_start()?;
//! // … consumers now resolve the four primitives for `EventBroker` …
//! handle.stop().await;
//! # Ok(())
//! # }
//! ```
//!
//! ## Config-driven wiring
//!
//! [`ClusterWiring::from_config`] parses operator [`ClusterConfig`] (DESIGN
//! §3.11) and instantiates each profile's cache backend through a
//! [`ClusterCacheProvider`] resolved from a [`ProviderRegistry`], letting the
//! omit-default auto-wrap supply the other three primitives. The programmatic
//! [`ProfileBackends`] builder remains the lower-level API the config path builds
//! on.
//!
//! ## Status
//!
//! Only the cache anchor is provider-instantiated; explicit non-cache bindings
//! are rejected pending native leader-election / lock / service-discovery
//! providers (a follow-up). Credential resolution for a backend's
//! [`secret_ref`](BackendBinding) is the deferred OOP open question — the field
//! is a placeholder.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

mod config;
mod gear;
mod provider;
mod wiring;

pub use config::{BackendBinding, ClusterConfig, ProfileConfig, SecretRef};
pub use provider::ProviderRegistry;
pub use wiring::{ClusterHandle, ClusterWiring, ClusterWiringBuilder, ProfileBackends};

// Re-exported for convenience: plugins implement these from the SDK, but the
// config-driven wiring API surfaces them here too.
pub use cluster_sdk::{ClusterCacheProvider, StopHook};
