//! Tests for the readiness subsystem.

use super::*;
use crate::healthcheck::{Healthcheck, HealthcheckResult, HealthcheckStatus};
use async_trait::async_trait;

/// A healthcheck that always returns a fixed result.
struct StaticCheck {
    name: &'static str,
    result: HealthcheckResult,
}

#[async_trait]
impl Healthcheck for StaticCheck {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn check(&self) -> HealthcheckResult {
        self.result.clone()
    }
}

fn empty_registry() -> Arc<RestHealthcheckRegistry> {
    Arc::new(RestHealthcheckRegistry::new())
}

/// A registry seeded with a single gear healthcheck.
fn registry_with(
    gear: &'static str,
    name: &'static str,
    result: HealthcheckResult,
) -> Arc<RestHealthcheckRegistry> {
    let registry = Arc::new(RestHealthcheckRegistry::new());
    registry.register(gear, Arc::new(StaticCheck { name, result }));
    registry
}

#[tokio::test]
async fn no_deps_no_checks_is_ready() {
    let state = ReadinessState::new(Vec::<String>::new(), empty_registry());

    let report = state.evaluate().await;
    assert!(!report.ready);
    assert_eq!(report.state, ReadinessLifecycle::Starting);

    state.mark_startup_complete();
    let report = state.evaluate().await;
    let health = state.health_report().await;
    assert!(report.ready);
    assert_eq!(report.state, ReadinessLifecycle::Ready);
    assert!(report.unresolved_deps.is_empty());
    assert!(health.components.is_empty());
    assert_eq!(health.status, HealthcheckStatus::Healthy);
}

#[tokio::test]
async fn unresolved_deps_block_readiness_until_resolved() {
    let state = ReadinessState::new(["billing", "catalog"], empty_registry());

    let report = state.evaluate().await;
    assert!(!report.ready);
    assert_eq!(report.state, ReadinessLifecycle::Starting);
    assert_eq!(report.unresolved_deps, vec!["billing", "catalog"]);

    state.mark_dep_resolved("billing");
    let report = state.evaluate().await;
    assert!(!report.ready);
    assert_eq!(report.unresolved_deps, vec!["catalog"]);

    state.mark_dep_resolved("catalog");
    state.mark_startup_complete();
    let report = state.evaluate().await;
    assert!(report.ready);
    assert!(report.unresolved_deps.is_empty());
}

#[tokio::test]
async fn mark_unknown_dep_is_ignored() {
    let state = ReadinessState::new(["billing"], empty_registry());
    state.mark_dep_resolved("does-not-exist");
    assert!(!state.all_deps_resolved());
    let report = state.evaluate().await;
    assert!(!report.ready);
    assert_eq!(report.unresolved_deps, vec!["billing"]);
}

#[tokio::test]
async fn unhealthy_check_forces_not_ready() {
    let state = ReadinessState::new(
        Vec::<String>::new(),
        registry_with(
            "cache",
            "cache-warm",
            HealthcheckResult::unhealthy("cache cold"),
        ),
    );

    state.mark_startup_complete();
    let report = state.evaluate().await;
    let health = state.health_report().await;
    assert!(!report.ready);
    assert_eq!(report.state, ReadinessLifecycle::Starting);
    assert_eq!(health.status, HealthcheckStatus::Unhealthy);
    assert_eq!(health.components.len(), 1);
    assert_eq!(health.components[0].check, "cache-warm");
}

#[tokio::test]
async fn degraded_check_stays_ready_but_reported() {
    let state = ReadinessState::new(
        Vec::<String>::new(),
        registry_with(
            "indexer",
            "index",
            HealthcheckResult::degraded("rebuilding index"),
        ),
    );

    state.mark_startup_complete();
    let report = state.evaluate().await;
    let health = state.health_report().await;
    assert!(report.ready, "degraded must still be ready (200)");
    assert_eq!(report.state, ReadinessLifecycle::Degraded);
    assert_eq!(health.status, HealthcheckStatus::Degraded);
    assert_eq!(health.components.len(), 1);
    assert_eq!(health.components[0].check, "index");
}

#[tokio::test]
async fn unresolved_dep_beats_healthy_check() {
    let state = ReadinessState::new(
        ["billing"],
        registry_with("gear", "ok", HealthcheckResult::healthy()),
    );
    state.mark_startup_complete();
    let report = state.evaluate().await;
    let health = state.health_report().await;
    assert!(
        !report.ready,
        "an unresolved dep must gate readiness even when checks are healthy"
    );
    assert_eq!(health.status, HealthcheckStatus::Healthy);
}

#[tokio::test]
async fn draining_flips_to_not_ready() {
    let state = ReadinessState::new(Vec::<String>::new(), empty_registry());
    state.mark_startup_complete();
    assert!(state.evaluate().await.ready);

    state.set_draining(true);
    let report = state.evaluate().await;
    assert!(!report.ready);
    assert_eq!(report.state, ReadinessLifecycle::Draining);
}
