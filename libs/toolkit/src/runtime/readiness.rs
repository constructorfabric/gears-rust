//! Framework-managed readiness state for the `OoP` bootstrap (`cpt-cf-fr-eventual-readiness`).
//!
//! An `OoP` gear becomes *live* the moment its HTTP server binds (`/healthz`),
//! but only becomes *ready* (`/readyz`) once startup is complete, every critical
//! dependency has been resolved, **and** the gear's registered healthchecks
//! report it can serve traffic (Spring Boot-style health groups per
//! `cpt-cf-fr-eventual-readiness`).
//!
//! Readiness reuses the framework's standard healthcheck mechanism
//! ([`crate::healthcheck`]): a gear expresses readiness once, via
//! [`RestApiCapability::healthcheck`](crate::contracts::RestApiCapability::healthcheck),
//! and it is honored identically whether the gear is hosted in-process by the
//! `api-gateway` or run `OoP`. This module layers the three `OoP`-only concerns
//! the gateway path lacks — startup completion, critical-dependency resolution
//! gating, and the graceful-drain readiness flip — on top of that shared
//! healthcheck report.
//!
//! The healthcheck report itself is fanned out concurrently, timeout-bounded,
//! panic-isolated, and cached inside the [`RestHealthcheckRegistry`], so a burst
//! of probe traffic cannot storm the registered checks. The dependency and
//! draining state layered on top are cheap live reads.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::RwLock;

use crate::healthcheck::{HealthcheckReport, HealthcheckStatus, RestHealthcheckRegistry};

/// Sticky, dynamic dependency-resolution tracker shared across serving modes
/// (in-process host and `OoP`).
///
/// **Passive:** the resolution loop / consumer-wiring feeds it
/// ([`register_dep`](Self::register_dep), [`mark_resolved`](Self::mark_resolved));
/// it never runs a check itself — unlike the active
/// [`Healthcheck`](crate::healthcheck::Healthcheck) registry. It answers exactly
/// one question — *are all declared dependencies resolved?* — and carries the
/// process-wide draining flag both `/readyz` front-ends consult (the in-process
/// [`ReadinessHealthcheck`] leaf check and the `OoP` [`ReadinessState`]
/// aggregator).
///
/// Startup-gating and **sticky**: once a dependency is resolved it stays
/// resolved. A provider vanishing later does NOT revert `/readyz` — that is
/// runtime churn, handled lazily by the directory-resolving client at call time
/// (liveness is `/healthz`, not `/readyz`).
#[derive(Debug, Default)]
pub struct DependencyChecker {
    /// Graceful-shutdown flag; when set, `/readyz` reports `503` regardless of deps.
    draining: AtomicBool,
    /// `dep_gear` → resolved. Populated by the proxy-wiring / consumer-wiring
    /// phase from each `#[toolkit::consumes]` registration; flipped `true` once
    /// the provider endpoint resolves.
    deps: RwLock<BTreeMap<String, bool>>,
}

impl DependencyChecker {
    /// Create an empty checker (no deps → trivially resolved / ready).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a consumed dependency that gates readiness (idempotent; keeps an
    /// already-resolved entry resolved).
    pub fn register_dep(&self, dep_gear: impl Into<String>) {
        self.deps.write().entry(dep_gear.into()).or_insert(false);
    }

    /// Mark a previously-registered dependency as resolved. Unknown names are
    /// ignored. Returns `true` only on the `false → true` transition (so callers
    /// can log the resolution exactly once).
    pub fn mark_resolved(&self, dep_gear: &str) -> bool {
        if let Some(resolved) = self.deps.write().get_mut(dep_gear) {
            let was = *resolved;
            *resolved = true;
            !was
        } else {
            false
        }
    }

    /// Names of consumed dependency gears not yet resolved.
    #[must_use]
    pub fn unresolved_deps(&self) -> Vec<String> {
        self.deps
            .read()
            .iter()
            .filter(|(_, resolved)| !**resolved)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Whether every declared dependency has resolved (or none were declared).
    #[must_use]
    pub fn all_resolved(&self) -> bool {
        self.deps.read().values().all(|resolved| *resolved)
    }

    /// Begin (or clear) draining. Setting it makes `/readyz` report `503`
    /// regardless of dependency state so upstreams drain the instance.
    pub fn set_draining(&self, draining: bool) {
        self.draining.store(draining, Ordering::SeqCst);
    }

    /// Whether shutdown has begun.
    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// Whether the process is serving traffic: all deps resolved and not draining.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        !self.is_draining() && self.all_resolved()
    }
}

/// Bridges a [`DependencyChecker`] into the
/// [`RestHealthcheckRegistry`](crate::healthcheck::RestHealthcheckRegistry) as a
/// single leaf check, for the **in-process** (`api-gateway`-hosted) `/readyz`
/// path where readiness is one check among many in a shared registry.
///
/// The gateway `/readyz` reports `503` whenever any registered check is
/// `Unhealthy`, so mapping draining / unresolved-deps → `Unhealthy` gates the
/// probe exactly as ADR-0007 intends. `/healthz` (liveness) is a static handler
/// and is unaffected. The registry caches reports (~2s), so a readiness/drain
/// transition surfaces on `/readyz` with up to that lag.
///
/// The `OoP` path instead uses [`ReadinessState::evaluate`] directly (it owns
/// its registry), so this leaf adapter is not used there — avoiding the
/// re-aggregation recursion that owning the same registry would cause.
pub struct ReadinessHealthcheck {
    deps: Arc<DependencyChecker>,
}

impl ReadinessHealthcheck {
    /// Wrap the shared dependency checker.
    #[must_use]
    pub fn new(deps: Arc<DependencyChecker>) -> Self {
        Self { deps }
    }
}

#[async_trait::async_trait]
impl crate::healthcheck::Healthcheck for ReadinessHealthcheck {
    fn name(&self) -> &'static str {
        "readiness"
    }

    async fn check(&self) -> crate::healthcheck::HealthcheckResult {
        use crate::healthcheck::HealthcheckResult;
        if self.deps.is_draining() {
            return HealthcheckResult::unhealthy("draining").with_code("draining");
        }
        let unresolved = self.deps.unresolved_deps();
        if unresolved.is_empty() {
            HealthcheckResult::healthy()
        } else {
            HealthcheckResult::unhealthy(format!(
                "unresolved dependencies: {}",
                unresolved.join(", ")
            ))
            .with_code("deps_unresolved")
        }
    }
}

/// Default per-check timeout for readiness healthchecks.
///
/// Matches the `api-gateway` `healthcheck_timeout_ms` default so a gear's
/// healthcheck behaves identically in-process and `OoP`.
pub const DEFAULT_HEALTHCHECK_TIMEOUT: Duration = Duration::from_millis(500);

/// Lifecycle state reported on `/readyz` (`cpt-cf-adr-eventual-readiness`).
///
/// Serialized lowercase; the four variants are a stable wire contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadinessLifecycle {
    /// Not yet able to serve — startup is not complete, critical deps are
    /// unresolved, or a healthcheck is `Unhealthy`. Maps to `503`.
    Starting,
    /// Fully serving traffic. Maps to `200`.
    Ready,
    /// Serving with reduced functionality — a healthcheck reported `Degraded`
    /// (e.g. an optional backend is down but a fallback is acceptable). Kept in
    /// rotation: maps to `200`.
    Degraded,
    /// Graceful shutdown in progress; upstreams should stop routing. Maps to
    /// `503`.
    Draining,
}

/// The aggregate readiness report rendered as the `/readyz` response body.
///
/// Readiness still depends on the aggregated [`HealthcheckReport`] internally,
/// but the detailed per-component report is intentionally not echoed here; it
/// belongs on the separate `/health` endpoint (see `oop_serve.rs`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReadinessReport {
    /// Lifecycle state — the primary readiness signal (`cpt-cf-adr-eventual-readiness`).
    pub state: ReadinessLifecycle,
    /// Whether the gear is ready to receive traffic (`true` → `200`, else `503`).
    /// Convenience mirror of `state ∈ {ready, degraded}` for probes/clients that
    /// do not want to know the `state → status` mapping.
    pub ready: bool,
    /// Critical dependencies not yet resolved via `DirectoryService` / DNS.
    /// Non-empty only while `starting`. Omitted from the body when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unresolved_deps: Vec<String>,
}

/// Shared, framework-owned readiness state for an `OoP` gear instance.
///
/// Created by the `OoP` bootstrap with the gear's critical dependency names and
/// a shared [`RestHealthcheckRegistry`] (populated from each gear's
/// [`RestApiCapability::healthcheck`](crate::contracts::RestApiCapability::healthcheck)).
/// Cloned as an `Arc` into the probe router and the dependency-resolution task.
pub struct ReadinessState {
    /// Shared dependency-resolution core (deps + draining). Owned here for the
    /// `OoP` aggregator; the in-process path shares the same [`DependencyChecker`]
    /// via [`Self::dependency_checker`].
    deps: Arc<DependencyChecker>,
    /// Whether the gear has finished startup and is actually serving traffic.
    /// Remains `false` until the bootstrap publishes the composed routes.
    startup_complete: AtomicBool,
    /// Shared gear healthcheck registry; supplies the "custom checks" dimension
    /// of readiness (fan-out, timeout, panic isolation, and caching live here).
    healthchecks: Arc<RestHealthcheckRegistry>,
    /// Per-check timeout passed to [`RestHealthcheckRegistry::report`].
    check_timeout: Duration,
}

impl std::fmt::Debug for ReadinessState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadinessState")
            .field("unresolved_deps", &self.deps.unresolved_deps())
            .field("draining", &self.deps.is_draining())
            .field(
                "startup_complete",
                &self.startup_complete.load(Ordering::Relaxed),
            )
            .field("check_timeout", &self.check_timeout)
            .finish_non_exhaustive()
    }
}

impl ReadinessState {
    /// Create a new readiness state seeded with the gear's critical dependency
    /// names and the shared healthcheck registry. All listed deps start
    /// unresolved; the gear is not ready until startup is complete, each dep is
    /// marked resolved via [`mark_dep_resolved`](Self::mark_dep_resolved), and
    /// the healthchecks pass. Uses [`DEFAULT_HEALTHCHECK_TIMEOUT`] as the
    /// per-check timeout.
    #[must_use]
    pub fn new<I, S>(critical_deps: I, healthchecks: Arc<RestHealthcheckRegistry>) -> Arc<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::with_check_timeout(critical_deps, healthchecks, DEFAULT_HEALTHCHECK_TIMEOUT)
    }

    /// Like [`new`](Self::new) but with an explicit per-check timeout.
    #[must_use]
    pub fn with_check_timeout<I, S>(
        critical_deps: I,
        healthchecks: Arc<RestHealthcheckRegistry>,
        check_timeout: Duration,
    ) -> Arc<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let deps = Arc::new(DependencyChecker::new());
        for dep in critical_deps {
            deps.register_dep(dep);
        }
        Self::from_checker(deps, healthchecks, check_timeout)
    }

    /// Build an aggregator around an **existing** [`DependencyChecker`], so the
    /// in-process consumer-wiring path and the `OoP` `/readyz` aggregator share
    /// one dependency-resolution core (the checker is fed once, read by both).
    #[must_use]
    pub fn from_checker(
        deps: Arc<DependencyChecker>,
        healthchecks: Arc<RestHealthcheckRegistry>,
        check_timeout: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            deps,
            startup_complete: AtomicBool::new(false),
            healthchecks,
            check_timeout,
        })
    }

    /// The shared dependency-resolution core, e.g. to feed it from consumer
    /// wiring or to back an in-process [`ReadinessHealthcheck`] leaf.
    #[must_use]
    pub fn dependency_checker(&self) -> Arc<DependencyChecker> {
        Arc::clone(&self.deps)
    }

    /// Declare a critical dependency dynamically (idempotent; preserves an
    /// already-resolved entry). Complements the up-front `critical_deps` seed for
    /// deps discovered during consumer wiring.
    pub fn register_dep(&self, dep_gear: impl Into<String>) {
        self.deps.register_dep(dep_gear);
    }

    /// Mark startup as complete. Idempotent; subsequent calls are ignored.
    /// `/readyz` will not report `Ready` or `Degraded` until this is called.
    pub fn mark_startup_complete(&self) {
        self.startup_complete.store(true, Ordering::SeqCst);
    }

    /// Whether startup is complete.
    #[must_use]
    pub fn is_startup_complete(&self) -> bool {
        self.startup_complete.load(Ordering::SeqCst)
    }

    /// Mark a critical dependency as resolved. Idempotent; unknown names are
    /// ignored.
    pub fn mark_dep_resolved(&self, name: &str) {
        if self.deps.mark_resolved(name) {
            tracing::info!(dep = %name, "critical dependency resolved");
        }
    }

    /// Whether all critical dependencies have been resolved.
    #[must_use]
    pub fn all_deps_resolved(&self) -> bool {
        self.deps.all_resolved()
    }

    /// Set (or clear) the draining flag. Setting it flips `/readyz` to `503`
    /// immediately so upstreams pull the instance out of rotation while
    /// in-flight requests drain.
    pub fn set_draining(&self, draining: bool) {
        self.deps.set_draining(draining);
    }

    /// Whether the gear is currently draining.
    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.deps.is_draining()
    }

    /// Run the registered healthchecks and return the aggregated report.
    ///
    /// Used by `/health` to expose full per-component detail and by
    /// [`Self::evaluate`] to decide readiness state. The registry caches the
    /// report, so repeated calls within the cache window do not re-run checks.
    pub async fn health_report(&self) -> HealthcheckReport {
        self.healthchecks.report(self.check_timeout).await
    }

    /// Evaluate the aggregate readiness.
    ///
    /// The gear is ready when it is not draining, startup is complete, all
    /// critical deps are resolved, and the aggregated healthcheck report is not
    /// `Unhealthy`. `Degraded` healthchecks keep the gear ready (`state =
    /// degraded`, `ready = true`) but the detailed per-component messages belong
    /// on `/health`, not `/readyz`. The healthcheck fan-out is cached inside the
    /// registry, so repeated probe traffic does not re-run checks; dependency and
    /// draining state are read live so transitions take effect immediately.
    pub async fn evaluate(&self) -> ReadinessReport {
        let health = self.health_report().await;
        let draining = self.deps.is_draining();
        let startup_complete = self.is_startup_complete();
        let unresolved_deps: Vec<String> = self.deps.unresolved_deps();

        // Draining wins; then any not-ready condition (startup not complete,
        // unresolved deps, or an Unhealthy check) is `Starting`; then `Degraded`;
        // else `Ready`.
        let state = if draining {
            ReadinessLifecycle::Draining
        } else if !startup_complete
            || !unresolved_deps.is_empty()
            || health.status == HealthcheckStatus::Unhealthy
        {
            ReadinessLifecycle::Starting
        } else if health.status == HealthcheckStatus::Degraded {
            ReadinessLifecycle::Degraded
        } else {
            ReadinessLifecycle::Ready
        };

        let ready = matches!(
            state,
            ReadinessLifecycle::Ready | ReadinessLifecycle::Degraded
        );

        ReadinessReport {
            state,
            ready,
            unresolved_deps,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "readiness_tests.rs"]
mod tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod dep_checker_tests {
    use super::DependencyChecker;

    #[test]
    fn no_deps_is_ready() {
        let c = DependencyChecker::new();
        assert!(c.all_resolved());
        assert!(c.is_ready());
        assert!(c.unresolved_deps().is_empty());
    }

    #[test]
    fn unresolved_dep_lists_and_blocks() {
        let c = DependencyChecker::new();
        c.register_dep("billing");
        c.register_dep("inventory");
        assert!(!c.is_ready());
        assert_eq!(
            c.unresolved_deps(),
            vec!["billing".to_owned(), "inventory".to_owned()]
        );
    }

    #[test]
    fn ready_once_all_deps_resolved() {
        let c = DependencyChecker::new();
        c.register_dep("billing");
        c.register_dep("inventory");
        assert!(c.mark_resolved("billing")); // false -> true
        assert!(!c.is_ready());
        assert!(c.mark_resolved("inventory"));
        assert!(c.is_ready());
    }

    #[test]
    fn register_dep_is_idempotent_and_preserves_resolved() {
        let c = DependencyChecker::new();
        c.register_dep("billing");
        assert!(c.mark_resolved("billing"));
        c.register_dep("billing"); // must NOT reset to unresolved
        assert!(c.is_ready());
        assert!(!c.mark_resolved("billing")); // already resolved -> no transition
    }

    #[test]
    fn mark_resolved_unknown_dep_is_noop() {
        let c = DependencyChecker::new();
        assert!(!c.mark_resolved("nope")); // no panic, no transition, no entry
        assert!(c.is_ready());
    }

    #[test]
    fn draining_overrides_ready() {
        let c = DependencyChecker::new();
        assert!(c.is_ready());
        c.set_draining(true);
        assert!(c.is_draining());
        assert!(!c.is_ready());
        c.set_draining(false);
        assert!(c.is_ready());
    }

    #[tokio::test]
    async fn readiness_healthcheck_maps_state_to_status() {
        use super::ReadinessHealthcheck;
        use crate::healthcheck::{Healthcheck, HealthcheckStatus};
        use std::sync::Arc;

        let checker = Arc::new(DependencyChecker::new());
        checker.register_dep("billing");
        let hc = ReadinessHealthcheck::new(checker.clone());
        assert_eq!(hc.name(), "readiness");

        // Unresolved dep → Unhealthy (503), stable code, dep listed.
        let starting = hc.check().await;
        assert_eq!(starting.status, HealthcheckStatus::Unhealthy);
        assert_eq!(starting.code.as_deref(), Some("deps_unresolved"));
        assert!(starting.message.unwrap().contains("billing"));

        // Resolved → Healthy (200).
        checker.mark_resolved("billing");
        assert_eq!(hc.check().await.status, HealthcheckStatus::Healthy);

        // Draining wins regardless of deps.
        checker.set_draining(true);
        let draining = hc.check().await;
        assert_eq!(draining.status, HealthcheckStatus::Unhealthy);
        assert_eq!(draining.code.as_deref(), Some("draining"));
    }
}
