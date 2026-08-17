//! The gear's composite readiness healthcheck (DESIGN-DEPLOYABLE-GEAR §4.4).
//!
//! Cluster is the coordination dependency of nearly every gear, so its readiness
//! is a fleet-wide gate. ADR-0005 owns the four states and the
//! [`ReadinessReport`](toolkit::runtime::ReadinessReport) bodies; cluster supplies
//! exactly one dimension of them — the health verdict — through
//! [`Healthcheck::check`]. It shapes no body and picks no HTTP status.
//!
//! The mapping the framework applies to what this check returns
//! (`runtime/readiness.rs`), and therefore the whole of §4.4's table:
//!
//! | This check returns | `/readyz` state | HTTP |
//! |---|---|---|
//! | `Unhealthy` | `starting` | 503 |
//! | `Degraded` | `degraded` | 200 |
//! | `Healthy` | `ready` | 200 |
//!
//! `Draining` is not ours: the SIGTERM handler sets it and it outranks every
//! health verdict (§4.8).
//!
//! # Why one bad profile is `Degraded` and not `Unhealthy`
//!
//! Evicting the pod because one DSN is unreachable would take coordination down
//! for *every* profile, which is the failure the verdict exists to contain. The
//! per-profile consequence is delivered elsewhere: a failing probe writes
//! [`ProfileHealth::Degraded`] onto the profile, `DescribeProfiles` serves it, and
//! the SDK's consumer-side contributor takes *that profile's* consumers to 503
//! while consumers of healthy profiles keep serving. That is the gap §4.4 opens
//! and this module is one half of closing it.
//!
//! The same argument decides the case §4.4's table leaves open — **every** profile
//! unreachable — in favour of `Degraded` too, and it is not a lenient reading. A
//! 503 on `/readyz` removes the pod from its Service endpoints, which takes the
//! *gRPC* port out with it, and `DescribeProfiles` is how a consumer learns its
//! profile is degraded. Reporting `Unhealthy` would therefore suppress the very
//! signal that lets consumers degrade precisely, and would do it at the moment the
//! signal matters most. Nothing is gained in exchange: no restart fixes an
//! unreachable database, and readiness triggers no restart anyway.
//!
//! # The budget, and why this check times out rather than the framework
//!
//! The framework bounds each check by `oop_http.healthcheck_timeout_ms` (500 ms by
//! default) and maps a check that overruns to **`Unhealthy`** — a hung probe would
//! otherwise report `starting`/503 for the whole pod, exactly the verdict the
//! paragraph above rules out. So this check keeps its own, shorter budget
//! ([`READINESS_PROBE_BUDGET`]) and converts an overrun into `Degraded` itself.
//! The framework's timeout is the backstop, not the mechanism.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::{
    ClusterCacheBackend, ClusterError, DistributedLockBackend, LeaderElectionBackend, ProfileHealth,
};
use toolkit::{Healthcheck, HealthcheckResult};

use crate::domain::registry::{BoundProfile, InstanceId, ProfileRegistry};

/// The wall-clock budget one backend probe gets before the profile behind it is
/// reported [`ProfileHealth::Degraded`].
///
/// Half the framework's 500 ms default per-check timeout. The margin is not
/// decoration: probes run concurrently, so a round costs roughly one budget
/// regardless of profile count, and the remainder covers the registry read, the
/// fan-out and the framework's own bookkeeping. Overrunning the framework's bound
/// instead of this one would report the pod `Unhealthy`/`starting` rather than
/// `Degraded` — the wrong verdict, arrived at by accident.
///
/// A constant rather than config: `oop_http.healthcheck_timeout_ms` belongs to
/// the host, not to cluster, and inventing a cluster key that has to be kept
/// consistent with it would be a second source of truth for one deadline. An
/// operator who lowers the host timeout below this value gets the framework's
/// timeout first, and that is worth knowing rather than defending against.
pub const READINESS_PROBE_BUDGET: Duration = Duration::from_millis(250);

/// Cluster's contribution to `/readyz`, `/health` and `/healthz` — see the
/// [module docs](self).
///
/// Captures the [`ProfileRegistry`], never a backend: the gear's healthcheck is
/// collected before `start` runs any wiring (§4.2's lifecycle constraint), so
/// there is no backend to capture yet. The registry it holds is the empty one
/// created in `init`, and it becomes populated under this check's feet.
pub struct ClusterReadiness {
    /// The index this check reads on every probe round.
    profiles: Arc<ProfileRegistry>,
    /// The profile names operator config declares.
    ///
    /// Needed because a registry alone cannot report a profile *missing*, and
    /// §4.4's `Ready` row is over "every configured profile". A snapshot that
    /// omits a configured name is the `Starting` row's second clause.
    configured: BTreeSet<String>,
    /// Per-probe budget; [`READINESS_PROBE_BUDGET`] unless overridden.
    budget: Duration,
}

/// One backend instance to probe, holding the typed `Arc` so the right trait
/// method is called.
///
/// Dedup is by [`InstanceId`], so an instance shared between primitives or
/// between profiles is probed once per round. That is what §4.4's "every backend
/// *instance's* probe" asks for, and it is what keeps the round's cost tied to
/// the number of real backends rather than to `profiles x primitives`.
enum ProbeTarget {
    Cache(Arc<dyn ClusterCacheBackend>),
    Lock(Arc<dyn DistributedLockBackend>),
    LeaderElection(Arc<dyn LeaderElectionBackend>),
}

impl ProbeTarget {
    async fn probe(&self) -> Result<(), ClusterError> {
        match self {
            Self::Cache(backend) => backend.probe().await,
            Self::Lock(backend) => backend.probe().await,
            Self::LeaderElection(backend) => backend.probe().await,
        }
    }

    /// The `primitive` label for logs — bounded, per invariant I15.
    const fn primitive(&self) -> &'static str {
        match self {
            Self::Cache(_) => "cache",
            Self::Lock(_) => "lock",
            Self::LeaderElection(_) => "leader_election",
        }
    }
}

impl ClusterReadiness {
    /// The check over `profiles`, expecting the profile names in `configured`.
    #[must_use]
    pub fn new<I, S>(profiles: Arc<ProfileRegistry>, configured: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            profiles,
            configured: configured.into_iter().map(Into::into).collect(),
            budget: READINESS_PROBE_BUDGET,
        }
    }

    /// [`new`](Self::new) with an explicit per-probe budget, for tests that need a
    /// deadline they can outrun deterministically.
    #[must_use]
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// Probes every distinct instance once, records each profile's verdict on the
    /// profile itself, and returns the degraded profile names in name order.
    async fn probe_round(
        &self,
        profiles: &BTreeMap<&'static str, Arc<BoundProfile>>,
    ) -> Vec<&'static str> {
        let mut targets: BTreeMap<InstanceId, ProbeTarget> = BTreeMap::new();
        for profile in profiles.values() {
            targets
                .entry(profile.instances.cache)
                .or_insert_with(|| ProbeTarget::Cache(Arc::clone(&profile.cache)));
            targets
                .entry(profile.instances.lock)
                .or_insert_with(|| ProbeTarget::Lock(Arc::clone(&profile.lock)));
            targets
                .entry(profile.instances.leader_election)
                .or_insert_with(|| {
                    ProbeTarget::LeaderElection(Arc::clone(&profile.leader_election))
                });
        }

        // Concurrently, each bounded by the budget on its own, so one slow
        // instance costs the round one budget rather than serialising behind the
        // others — and so a partial failure still attributes to the right
        // instances instead of collapsing the whole round to "timed out".
        let outcomes =
            futures_util::future::join_all(targets.iter().map(|(id, target)| async move {
                let verdict = tokio::time::timeout(self.budget, target.probe()).await;
                let serving = match verdict {
                    Ok(Ok(())) => true,
                    Ok(Err(error)) => {
                        // The error text stays in the log, never in the health
                        // message: a provider error can carry a DSN, and `/health` is
                        // unauthenticated (invariant I15's cardinality split doubles
                        // as the disclosure split here).
                        tracing::warn!(
                            primitive = target.primitive(),
                            %error,
                            "cluster backend probe failed"
                        );
                        false
                    }
                    Err(_elapsed) => {
                        tracing::warn!(
                            primitive = target.primitive(),
                            budget_ms = self.budget.as_millis(),
                            "cluster backend probe exceeded its budget"
                        );
                        false
                    }
                };
                (*id, serving)
            }))
            .await;

        let serving: BTreeMap<InstanceId, bool> = outcomes.into_iter().collect();
        // An instance missing from the map cannot happen (every id was probed);
        // reading a miss as *not* serving keeps the fail-safe direction.
        let instance_ok = |id: InstanceId| serving.get(&id).copied().unwrap_or(false);

        let mut degraded = Vec::new();
        for (name, profile) in profiles {
            let refs = profile.instances;
            let health = if instance_ok(refs.cache)
                && instance_ok(refs.lock)
                && instance_ok(refs.leader_election)
            {
                ProfileHealth::Serving
            } else {
                degraded.push(*name);
                ProfileHealth::Degraded
            };
            // Published on the profile, not through `ProfileRegistry::publish`:
            // health must not bump the registry generation (see
            // `BoundProfile::health`).
            let previous = profile.set_health(health);
            if previous != health {
                tracing::warn!(
                    profile = *name,
                    from = ?previous,
                    to = ?health,
                    "cluster profile health changed"
                );
            }
        }
        degraded
    }

    /// The configured profiles absent from `snapshot`, in name order.
    fn unbound<'a>(&'a self, profiles: &BTreeMap<&'static str, Arc<BoundProfile>>) -> Vec<&'a str> {
        self.configured
            .iter()
            .map(String::as_str)
            .filter(|name| !profiles.contains_key(*name))
            .collect()
    }
}

#[async_trait]
impl Healthcheck for ClusterReadiness {
    fn name(&self) -> &'static str {
        // An explicit id, not a type path: it is exposed verbatim on `/health`.
        "cluster-readiness"
    }

    async fn check(&self) -> HealthcheckResult {
        // One snapshot for the whole check, so the profiles probed are the
        // profiles reported on.
        let snapshot = self.profiles.snapshot();

        // Generation 0 is the one state that means "`start` has not published":
        // `publish` always bumps it, so an empty snapshot at a later generation is
        // a deliberately empty profile set, not an unfinished startup.
        if snapshot.generation == 0 {
            return HealthcheckResult::unhealthy("cluster start has not completed")
                .with_code("starting");
        }

        let unbound = self.unbound(&snapshot.profiles);
        if !unbound.is_empty() {
            return HealthcheckResult::unhealthy(format!(
                "profiles configured but not bound: {}",
                unbound.join(", ")
            ))
            .with_code("profiles_unbound");
        }

        let degraded = self.probe_round(&snapshot.profiles).await;
        if degraded.is_empty() {
            HealthcheckResult::healthy()
        } else {
            // Profile names only. They are operator-authored, bounded, and
            // already readable on `/admin/profiles`; the failure detail that is
            // not safe to publish stayed in the log.
            HealthcheckResult::degraded(format!("profiles degraded: {}", degraded.join(", ")))
                .with_code("profile_degraded")
        }
    }
}

#[cfg(test)]
#[path = "health_tests.rs"]
mod health_tests;
