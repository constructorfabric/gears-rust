// Created: 2026-06-16 by Constructor Tech
//! The plugin-facing backend-provider trait (DESIGN §3.4 / §3.7).
//!
//! A cluster backend plugin implements [`ClusterCacheProvider`] to turn a set of
//! operator-supplied options into a constructed cache backend plus a shutdown
//! hook. The wiring crate (`cf-gears-cluster`) parses operator YAML, collects the
//! providers into a registry, and calls [`build_cache`](ClusterCacheProvider::build_cache)
//! per profile — letting the omit-default auto-wrap supply the other three
//! primitives over the returned cache.
//!
//! This trait lives in the SDK, alongside the backend traits and the
//! [`ClusterPluginSpecV1`](crate::ClusterPluginSpecV1) discovery spec, so a plugin
//! implements it while depending on the SDK only — never on the wiring crate. The
//! provider receives its options as a raw serde map; the typed operator YAML
//! schema (`BackendBinding`) is owned by the wiring crate, keeping the SDK free of
//! the config schema.
//!
//! Only the cache anchor is provider-instantiated today. Native leader-election /
//! lock / service-discovery providers are a follow-up that adds sibling `build_*`
//! methods.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::cache::ClusterCacheBackend;
use crate::error::ClusterError;

/// A boxed, owned shutdown action for a started backend. The cluster handle owns
/// it and awaits it once during shutdown — typically a plugin handle's `stop()`.
pub type StopHook = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

/// Builds the cache backend for one provider, owning the lifecycle of whatever
/// background work that backend needs (e.g. a TTL sweeper or renewal loop).
///
/// Implementors live in the backend plugin crates and depend on the SDK only. The
/// wiring crate registers them into a provider registry and dispatches on the
/// operator config's `provider` string.
pub trait ClusterCacheProvider: Send + Sync {
    /// The provider name this builds for, matched against the operator config's
    /// `provider` field. Must be stable and unique within a registry.
    fn provider(&self) -> &'static str;

    /// Builds the cache backend from `options` (the provider-specific keys from
    /// the operator config) and returns it alongside a hook that stops the
    /// backend's background work.
    ///
    /// # Options contract
    /// `options` is the flattened, provider-specific subset of one operator
    /// backend binding (the wiring strips the framing keys like `provider` before
    /// the call). It is intentionally an untyped `serde_json::Map` so the SDK
    /// stays free of any provider's config schema: each provider owns its own key
    /// set and deserializes `options` into its typed config (rejecting unknown
    /// keys). To preserve the stable plugin contract, that key set must evolve
    /// additively — new keys are optional with backward-compatible defaults, so a
    /// binding written for an older provider build still deserializes. An empty
    /// map means "all defaults".
    ///
    /// # Errors
    /// Returns [`ClusterError::InvalidConfig`] if `options` are invalid for this
    /// provider, or propagates any startup error from the backend.
    fn build_cache(
        &self,
        options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(Arc<dyn ClusterCacheBackend>, StopHook), ClusterError>;
}
