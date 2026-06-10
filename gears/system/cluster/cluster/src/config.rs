// Created: 2026-06-15 by Constructor Tech
//! Operator YAML schema for the cluster gear (DESIGN §3.4 / §3.11).
//!
//! [`ClusterConfig`] is the operator-facing contract: a map of named profiles,
//! each binding the four coordination primitives to a backend `provider`. The
//! `cache` binding is the required anchor; the other three may be omitted to ride
//! the SDK default backends over that profile's cache
//! (`cpt-cf-clst-fr-routing-omit-default`), or bound to their own provider for
//! per-primitive routing (`cpt-cf-clst-fr-routing-per-primitive`).
//!
//! These types are serde-deserializable (typically via `ctx.config()` in a host
//! gear, fed by `serde-saphyr`). They live in the wiring crate, not the SDK — the
//! SDK coordination contract stays serde-free per `cpt-cf-clst-constraint-no-serde`.
//!
//! Per-provider options are **flattened** into the backend binding and parsed by
//! the provider itself (see [`crate::provider::ClusterCacheProvider`]), so adding
//! a backend is a new crate plus config, not a schema change here.

use serde::Deserialize;

/// The whole cluster section of operator YAML: a set of named profiles.
///
/// ```yaml
/// cluster:
///   profiles:
///     default:
///       cache: { provider: standalone }
///       # leader_election / lock / service_discovery omitted → SDK defaults
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterConfig {
    /// Profile name → per-primitive backend bindings. Profile names must conform
    /// to the cluster name rule (`[a-zA-Z0-9_-]+`); the wiring validates this at
    /// registration time.
    #[serde(default)]
    pub profiles: std::collections::BTreeMap<String, ProfileConfig>,
}

/// The per-primitive backend bindings for one profile.
///
/// `cache` is required (it is the omit-default anchor). Each of the other three
/// primitives may be bound to its own provider or omitted; an omitted primitive
/// is auto-filled with the SDK default backend over this profile's cache.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    /// The cache backend — required. Serves as the anchor the SDK default
    /// leader-election, lock, and service-discovery backends wrap when those
    /// primitives are omitted.
    pub cache: BackendBinding,
    /// An explicit leader-election backend. Omit to use the SDK default over the
    /// cache.
    #[serde(default)]
    pub leader_election: Option<BackendBinding>,
    /// An explicit distributed-lock backend. Omit to use the SDK default over the
    /// cache.
    #[serde(default)]
    pub lock: Option<BackendBinding>,
    /// An explicit service-discovery backend. Omit to use the SDK default over the
    /// cache.
    #[serde(default)]
    pub service_discovery: Option<BackendBinding>,
}

/// One primitive's binding to a backend `provider`, plus that provider's own
/// options (flattened) and an optional credential reference.
///
/// The known keys are `provider` and `secret_ref`; every other key is captured
/// into [`options`](Self::options) verbatim for the provider to parse. This keeps
/// the schema open: a new backend defines its own option keys without changing
/// this struct.
#[derive(Debug, Clone, Deserialize)]
pub struct BackendBinding {
    /// The backend provider name, e.g. `standalone`, `postgres`, `redis`,
    /// `k8s-lease`. Matched against the registered providers at wiring time; an
    /// unknown provider fails startup with `ClusterError::InvalidConfig`.
    pub provider: String,
    /// A provisional, OPEN reference to the credential the backend uses to reach
    /// its infrastructure (DESIGN §3 open question — credential wiring is deferred
    /// to the OOP deployment design). Placeholder shape only; not a committed
    /// contract. Ignored by the in-process standalone provider.
    #[serde(default)]
    pub secret_ref: Option<SecretRef>,
    /// Provider-specific options captured verbatim (Design A: flattened options).
    /// The provider deserializes the keys it understands from this map.
    #[serde(flatten)]
    pub options: serde_json::Map<String, serde_json::Value>,
}

/// Provisional placeholder for a backend credential reference (DESIGN §3 open
/// question, deferred to the OOP deployment design). The concrete resolution —
/// credstore lookup, K8s service-account fallback, rotation — is intentionally
/// unspecified here.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretRef {
    /// An opaque name the future credential layer will resolve. Treated as an
    /// opaque string for now.
    pub name: String,
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod config_tests;
