//! Operator-facing configuration schema for the service-discovery gear.

use std::collections::HashMap;
use std::time::Duration;

/// Default lease TTL and grace: a crashed instance stops being
/// preferred within `ttl` and becomes unobservable within `ttl + grace`.
const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(30);
const DEFAULT_LEASE_GRACE: Duration = Duration::from_secs(30);

const fn default_lease_ttl() -> Duration {
    DEFAULT_LEASE_TTL
}
const fn default_lease_grace() -> Duration {
    DEFAULT_LEASE_GRACE
}

/// Instance-lease policy (`gears.service-discovery.config.lease`,
/// `cpt-cf-adr-directory-persistence-and-replication`).
///
/// Durations are humantime strings (e.g. `30s`). Both default to 30s.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseConfig {
    /// How long a registration or heartbeat keeps an instance observable.
    #[serde(default = "default_lease_ttl", with = "humantime_serde")]
    pub ttl: Duration,
    /// Extra window after `ttl` during which the instance reads as
    /// `Quarantined` before it becomes unobservable.
    #[serde(default = "default_lease_grace", with = "humantime_serde")]
    pub grace: Duration,
}

impl Default for LeaseConfig {
    fn default() -> Self {
        Self {
            ttl: DEFAULT_LEASE_TTL,
            grace: DEFAULT_LEASE_GRACE,
        }
    }
}

/// The gear's config section (`gears.service-discovery.config`).
///
/// `deny_unknown_fields` so a misspelled key (e.g. `trusted_registrar`) fails at
/// startup instead of silently deserializing to an empty set.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceDiscoveryConfig {
    /// Peer identities permitted to act on any gear's registration (see
    /// [`crate::server::make_directory_service`]). Empty by default.
    #[serde(default)]
    pub trusted_registrars: Vec<String>,
    /// Platform-controlled Kubernetes namespaces a `ServiceAccount` peer must
    /// live in to register.
    ///
    /// Empty **disables** the namespace check (name-only fallback): a
    /// `ServiceAccount` named `billing` from *any* namespace is then authorized
    /// for gear `billing`. Set this in any cluster where untrusted workloads can
    /// mint tokens, or a tenant SA sharing a gear's name could take over its
    /// registration.
    #[serde(default)]
    pub platform_namespaces: Vec<String>,
    /// SPIFFE trust domains a workload peer must belong to.
    ///
    /// Empty **disables** the trust-domain check (name-only fallback): a workload
    /// named `billing` from *any* trust domain is then authorized for gear
    /// `billing`. Same guidance as `platform_namespaces`.
    #[serde(default)]
    pub trust_domains: Vec<String>,
    /// Service-name -> gear ownership, loaded before directory serving.
    /// Configure application/`OoP` names to protect them before registration.
    /// Compiled-in owners are merged later, overriding conflicting entries.
    /// Unconfigured names use first-registration ownership. Empty by default;
    /// caller authorization is checked separately.
    #[serde(default)]
    pub grpc_service_owners: HashMap<String, String>,
    /// Instance-lease policy. Both `ttl` and `grace` default to 30s.
    #[serde(default)]
    pub lease: LeaseConfig,
}
