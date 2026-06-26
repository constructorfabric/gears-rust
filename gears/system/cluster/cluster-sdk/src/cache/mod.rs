// Created: 2026-06-03 by Constructor Tech
//! Distributed cache primitive — the universal compare-and-swap building block
//! on which leader election, locks, and service discovery are layered.
//!
//! Provides the [`ClusterCacheBackend`] plugin trait, the [`ClusterCacheV1`]
//! facade, the versioned domain types, the watch-union event shape, and the
//! fluent [`CacheResolverBuilder`] with startup capability validation.

pub mod backend;
pub mod facade;
pub mod polyfill;
pub mod resolver;
mod scoped;
pub mod types;
pub mod watch;

pub use backend::ClusterCacheBackend;
pub use facade::ClusterCacheV1;
pub use polyfill::PollingPrefixWatch;
pub use resolver::{CacheResolverBuilder, validate_cache_capabilities};
pub(crate) use scoped::ScopedCacheBackend;
pub use types::{
    CacheCapability, CacheConsistency, CacheEntry, CacheEvent, CacheFeatures, PutRequest, Ttl,
};
pub use watch::{CacheWatch, CacheWatchEvent, CacheWatchSender, CacheWatchTrySendError};
