//! Operator-facing configuration schema for the service-discovery gear.

use std::collections::HashMap;
use std::time::Duration;

/// Default lease TTL and grace: a crashed instance stops being
/// preferred within `ttl` and becomes unobservable within `ttl + grace`.
const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(30);
const DEFAULT_LEASE_GRACE: Duration = Duration::from_secs(30);

/// Smallest lease TTL a client can renew reliably. Clients pace renewals at
/// `ttl / 3`, floored at 1s so a tiny TTL cannot turn the renewal loop into a
/// hot loop (`PresenceManager::renewal_interval`). Below 3s that floor would
/// make the first renewal land at or after the lease expires, so a healthy
/// worker could vanish between heartbeats; reject such a TTL at startup rather
/// than let it flap in production.
const MIN_LEASE_TTL: Duration = Duration::from_secs(3);

/// Largest lease TTL and grace accepted. An hour is already far past anything
/// useful: a TTL beyond it keeps a crashed instance preferred for that long and
/// defeats liveness detection, and a grace beyond it (the sweeper cadence)
/// leaves dead records resident. Rejecting at startup turns an operator typo
/// (`30h` for `30s`) or an absurd humantime value into a loud failure rather
/// than silently degraded liveness.
const MAX_LEASE_TTL: Duration = Duration::from_secs(3600);
const MAX_LEASE_GRACE: Duration = Duration::from_secs(3600);

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

impl LeaseConfig {
    /// Reject a lease policy a client cannot renew before it expires, or one
    /// large enough to break liveness or overflow the directory's clock.
    ///
    /// A `ttl` below [`MIN_LEASE_TTL`] cannot be renewed in time given the
    /// client's `ttl / 3` cadence and its 1s renewal floor, so a healthy worker
    /// would disappear between heartbeats. A `ttl` above [`MAX_LEASE_TTL`] or a
    /// `grace` above [`MAX_LEASE_GRACE`] is operationally useless — a crashed
    /// instance stays preferred, or dead records never sweep — and usually a
    /// units typo. Caught at startup so a misconfigured cluster fails loudly
    /// instead of silently degrading liveness.
    ///
    /// # Errors
    /// Returns an error naming the offending value when `ttl` is below the
    /// supported minimum or above the maximum, or when `grace` exceeds its
    /// maximum.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.ttl >= MIN_LEASE_TTL,
            "gears.service-discovery.config.lease.ttl is {:?}, below the {:?} minimum: \
             clients renew at ttl/3 (floored at 1s), so a smaller TTL expires between \
             heartbeats and a healthy worker would be dropped",
            self.ttl,
            MIN_LEASE_TTL,
        );
        anyhow::ensure!(
            self.ttl <= MAX_LEASE_TTL,
            "gears.service-discovery.config.lease.ttl is {:?}, above the {:?} maximum: \
             a crashed instance would stay preferred for that long, defeating liveness",
            self.ttl,
            MAX_LEASE_TTL,
        );
        anyhow::ensure!(
            self.grace <= MAX_LEASE_GRACE,
            "gears.service-discovery.config.lease.grace is {:?}, above the {:?} maximum: \
             dead records would stay resident for that long before the sweeper reclaims them",
            self.grace,
            MAX_LEASE_GRACE,
        );
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::{LeaseConfig, MAX_LEASE_GRACE, MAX_LEASE_TTL, MIN_LEASE_TTL};
    use std::time::Duration;

    #[test]
    fn lease_default_is_valid() {
        LeaseConfig::default()
            .validate()
            .expect("the default lease policy must be valid");
    }

    #[test]
    fn lease_rejects_a_ttl_below_the_minimum() {
        let cfg = LeaseConfig {
            ttl: Duration::from_millis(500),
            grace: Duration::ZERO,
        };
        let err = cfg
            .validate()
            .expect_err("a sub-second TTL clients cannot renew must be rejected");
        assert!(
            err.to_string().contains("lease.ttl"),
            "the error should name the offending field, got: {err}"
        );
    }

    #[test]
    fn lease_accepts_the_minimum_ttl() {
        LeaseConfig {
            ttl: MIN_LEASE_TTL,
            grace: Duration::ZERO,
        }
        .validate()
        .expect("exactly the minimum TTL is supported");
    }

    #[test]
    fn lease_rejects_a_ttl_above_the_maximum() {
        // A TTL past the maximum (e.g. a `30h`-for-`30s` typo) keeps a crashed
        // instance preferred far too long; reject it at startup.
        let cfg = LeaseConfig {
            ttl: MAX_LEASE_TTL + Duration::from_secs(1),
            grace: Duration::ZERO,
        };
        let err = cfg
            .validate()
            .expect_err("a TTL past the maximum must be rejected");
        assert!(
            err.to_string().contains("lease.ttl"),
            "the error should name the offending field, got: {err}"
        );
    }

    #[test]
    fn lease_rejects_a_grace_above_the_maximum() {
        let cfg = LeaseConfig {
            ttl: MIN_LEASE_TTL,
            grace: MAX_LEASE_GRACE + Duration::from_secs(1),
        };
        let err = cfg
            .validate()
            .expect_err("a grace past the maximum must be rejected");
        assert!(
            err.to_string().contains("lease.grace"),
            "the error should name the offending field, got: {err}"
        );
    }

    #[test]
    fn lease_accepts_the_maximum() {
        LeaseConfig {
            ttl: MAX_LEASE_TTL,
            grace: MAX_LEASE_GRACE,
        }
        .validate()
        .expect("exactly the maximum TTL and grace are supported");
    }
}
