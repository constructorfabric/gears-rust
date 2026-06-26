// Created: 2026-06-04 by Constructor Tech
//! Service-discovery domain types: the registration and discovered-instance
//! records, the serving-intent state, the per-key metadata predicate, the
//! serving-state filter, the extensible discovery filter, and the
//! capability/features descriptors.
//!
//! [`DiscoveryFilter::matches`] is the SDK-side realization of the
//! Apply-Discovery-Filter algorithm (`cpt-cf-clst-algo-service-discovery-filter`):
//! pure predicate logic with no timing dependency, so — unlike the
//! backend-owned heartbeat algorithm — it lives in the SDK and is unit-tested.
//! Consumers reuse it to filter the unfiltered topology watch client-side
//! (flow `inst-tw-filter`).

use std::collections::HashMap;
use std::time::SystemTime;

/// The serving intent a module declares for one of its instances.
///
/// This is the complete serving-intent state space, so the enum is deliberately
/// **closed** (not `#[non_exhaustive]`): consumers match it exhaustively.
///
/// Per [ADR-008](../ADR/008-service-discovery-state-is-intent-not-health.md),
/// this is *module-declared serving intent, not a health observation*. A stuck
/// instance cannot flip its own intent; it disappears from discovery only when
/// its TTL-bounded heartbeat stops (`cpt-cf-clst-algo-service-discovery-heartbeat`).
/// External liveness detection is out of scope for this primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceState {
    /// The module intends this instance to take new work — the initial state of
    /// every registration and the default the primary-routing filter selects.
    Enabled,
    /// The module has flipped this instance out of rotation (e.g. draining for a
    /// graceful shutdown). Still present in discovery while it heartbeats, but
    /// excluded by the default filter so routers stop sending it new work.
    Disabled,
}

/// A request to register a service instance (DESIGN §3.1 / §3.3).
///
/// `instance_id` is optional: when `None`, the backend assigns one
/// (`cpt-cf-clst-algo-service-discovery-heartbeat` precondition, flow
/// `inst-rg-assign`). `metadata` is an attribute namespace per instance and is
/// **never scoped** (§3.8) — coordination namespacing lives on `name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRegistration {
    /// The service name instances register under and consumers discover by.
    pub name: String,
    /// An optional caller-supplied instance id; the backend assigns one when
    /// this is `None`.
    pub instance_id: Option<String>,
    /// The network address other instances reach this one at.
    pub address: String,
    /// Free-form routing attributes (e.g. `region`, `topic-shard`, `version`).
    /// Keys and values pass through scoping unchanged (§3.8).
    pub metadata: HashMap<String, String>,
}

/// A discovered service instance returned by
/// [`ServiceDiscoveryV1::discover`](crate::discovery::ServiceDiscoveryV1::discover)
/// (DESIGN §3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInstance {
    /// The instance id — caller-supplied or backend-assigned at registration.
    pub instance_id: String,
    /// The network address to reach this instance at.
    pub address: String,
    /// The instance's routing attributes.
    pub metadata: HashMap<String, String>,
    /// The module-declared serving intent (NOT a health observation — ADR-008).
    pub state: InstanceState,
    /// The wall-clock time the backend recorded the registration.
    pub registered_at: SystemTime,
}

/// A per-key metadata predicate evaluated against an instance's metadata
/// (DESIGN §3.1).
///
/// `#[non_exhaustive]` so future predicate kinds (prefix, regex, …) are
/// additive.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MetaMatch {
    /// The metadata value for the key must equal this string exactly.
    Equals(String),
    /// The metadata value for the key must be one of these strings.
    OneOf(Vec<String>),
}

impl MetaMatch {
    /// Returns whether `value` — the instance's metadata value for the predicate
    /// key, or `None` when the key is absent — satisfies this predicate
    /// (`inst-fl-meta-check`). A predicate on an absent key never matches.
    #[must_use]
    pub fn matches(&self, value: Option<&str>) -> bool {
        // Matched exhaustively (no catch-all): although `MetaMatch` is
        // `#[non_exhaustive]`, within this crate every variant must be handled,
        // so a future predicate kind fails to compile here rather than being
        // silently treated as a match.
        match self {
            Self::Equals(expected) => value == Some(expected.as_str()),
            Self::OneOf(options) => value.is_some_and(|v| options.iter().any(|option| option == v)),
        }
    }
}

/// The serving-state dimension of a [`DiscoveryFilter`] (DESIGN §3.1).
///
/// Closed enum — this is the complete set of state selections. The default is
/// [`StateFilter::Enabled`] (primary routing): discovery returns only instances
/// the module intends to take work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StateFilter {
    /// Match only [`InstanceState::Enabled`] instances (the default).
    #[default]
    Enabled,
    /// Match only [`InstanceState::Disabled`] instances.
    Disabled,
    /// Match instances in any serving state.
    Any,
}

impl StateFilter {
    /// Returns whether `state` satisfies this filter (`inst-fl-state`).
    #[must_use]
    pub fn matches(self, state: InstanceState) -> bool {
        match self {
            Self::Enabled => state == InstanceState::Enabled,
            Self::Disabled => state == InstanceState::Disabled,
            Self::Any => true,
        }
    }
}

/// The extensible discovery filter (DESIGN §3.1 / §3.3).
///
/// The default — [`DiscoveryFilter::default`] — is enabled-only with no metadata
/// constraint (primary routing). [`DiscoveryFilter::any`] selects every serving
/// state. Metadata predicates are **AND-conjoined**: an instance matches only if
/// it satisfies the state filter *and* every metadata predicate.
///
/// `#[non_exhaustive]` so future filter dimensions are additive; build instances
/// through the constructors and chainable setters rather than a struct literal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct DiscoveryFilter {
    /// The serving-state dimension (default [`StateFilter::Enabled`]).
    pub state: StateFilter,
    /// AND-conjoined per-key metadata predicates (default: none).
    pub metadata: Vec<(String, MetaMatch)>,
}

impl DiscoveryFilter {
    /// A filter matching instances in **any** serving state with no metadata
    /// constraint — the explicit opt-in to all states (§3.3). Chain
    /// [`require_metadata`](Self::require_metadata) to add predicates.
    #[must_use]
    pub fn any() -> Self {
        Self {
            state: StateFilter::Any,
            metadata: Vec::new(),
        }
    }

    /// Sets the serving-state dimension, returning the updated filter.
    #[must_use]
    pub fn with_state(mut self, state: StateFilter) -> Self {
        self.state = state;
        self
    }

    /// Adds an AND-conjoined metadata predicate on `key`, returning the updated
    /// filter. Repeated keys add independent predicates that must all hold.
    #[must_use]
    pub fn require_metadata(mut self, key: impl Into<String>, predicate: MetaMatch) -> Self {
        self.metadata.push((key.into(), predicate));
        self
    }

    /// Returns whether `instance` satisfies this filter — the
    /// Apply-Discovery-Filter algorithm (`cpt-cf-clst-algo-service-discovery-filter`):
    /// the instance's serving state must satisfy [`state`](Self::state)
    /// (`inst-fl-state`) and every metadata predicate must hold (`inst-fl-meta`,
    /// AND-conjunction). An instance is included only when all checks pass
    /// (`inst-fl-return`).
    #[must_use]
    pub fn matches(&self, instance: &ServiceInstance) -> bool {
        if !self.state.matches(instance.state) {
            return false;
        }
        self.metadata.iter().all(|(key, predicate)| {
            predicate.matches(instance.metadata.get(key).map(String::as_str))
        })
    }
}

/// A capability a consumer can require of a service-discovery backend at
/// resolution time. Each variant maps to a concrete backend characteristic
/// check (DESIGN §3.10).
///
/// `#[non_exhaustive]` so future capabilities are additive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServiceDiscoveryCapability {
    /// Require the backend to evaluate metadata predicates server-side
    /// ([`ServiceDiscoveryFeatures::metadata_pushdown`] is `true`). The name
    /// mismatch with the feature flag is intentional (DESIGN §3.10): the
    /// consumer-facing capability is `MetadataFiltering`; the backend
    /// characteristic it checks is `metadata_pushdown`.
    MetadataFiltering,
}

/// Native capability flags a service-discovery backend declares via
/// [`ServiceDiscoveryBackend::features`](crate::discovery::ServiceDiscoveryBackend::features).
///
/// `#[non_exhaustive]` so future flags are additive; construct with
/// [`ServiceDiscoveryFeatures::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServiceDiscoveryFeatures {
    /// Whether the backend evaluates metadata predicates server-side. When
    /// `false`, consumers needing metadata filtering apply
    /// [`DiscoveryFilter::matches`] client-side over the unfiltered result.
    pub metadata_pushdown: bool,
}

impl ServiceDiscoveryFeatures {
    /// Creates a features descriptor.
    #[must_use]
    pub fn new(metadata_pushdown: bool) -> Self {
        Self { metadata_pushdown }
    }
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod types_tests;
