//! REST readiness healthcheck infrastructure for probing gear health.
//!
//! This module provides concurrent healthcheck execution with timeout protection.
//! Gears implement [`Healthcheck`] and register themselves via [`RestHealthcheckRegistry`].
//! The API Gateway calls [`report()`](RestHealthcheckRegistry::report) on `/health` and `/readyz`
//! requests to aggregate per-component readiness status.
//!
//! # Usage
//!
//! ```rust,ignore
//! use async_trait::async_trait;
//! use std::sync::Arc;
//! use toolkit::{Healthcheck, HealthcheckResult, contracts::RestApiCapability};
//!
//! struct MyHealthcheck;
//!
//! #[async_trait]
//! impl Healthcheck for MyHealthcheck {
//!     fn name(&self) -> &'static str {
//!         "my-gear-readiness"
//!     }
//!
//!     async fn check(&self) -> HealthcheckResult {
//!         HealthcheckResult::healthy()
//!     }
//! }
//!
//! impl RestApiCapability for MyGear {
//!     fn healthcheck(
//!         &self,
//!         _ctx: &toolkit::context::GearCtx,
//!     ) -> Option<Arc<dyn Healthcheck>> {
//!         Some(Arc::new(MyHealthcheck))
//!     }
//!
//!     fn register_rest(
//!         &self,
//!         ctx: &toolkit::context::GearCtx,
//!         router: axum::Router,
//!         openapi: &dyn toolkit::contracts::OpenApiRegistry,
//!     ) -> anyhow::Result<axum::Router> {
//!         Ok(router)
//!     }
//! }
//! ```
//!
//! # Kubernetes probe configuration
//!
//! ```yaml
//! livenessProbe:
//!   httpGet:
//!     path: /healthz
//!     port: http
//!   periodSeconds: 10
//!   timeoutSeconds: 2
//!   failureThreshold: 3
//!
//! readinessProbe:
//!   httpGet:
//!     path: /readyz
//!     port: http
//!   periodSeconds: 5
//!   timeoutSeconds: 2
//!   failureThreshold: 3
//! ```
//!
//! `/healthz` is liveness — always shallow, never runs user checks.
//! `/readyz` is readiness — runs user REST healthchecks and removes the pod
//! from traffic when unhealthy without triggering a restart.

use async_trait::async_trait;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Status of a single healthcheck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthcheckStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

/// Aggregate readiness status across all registered healthchecks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthcheckAggregateStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

/// Result returned by a single [`Healthcheck::check`] call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthcheckResult {
    pub status: HealthcheckStatus,
    pub message: Option<String>,
}

impl HealthcheckResult {
    #[must_use]
    pub fn healthy() -> Self {
        Self {
            status: HealthcheckStatus::Healthy,
            message: None,
        }
    }

    #[must_use]
    pub fn degraded(message: impl Into<String>) -> Self {
        Self {
            status: HealthcheckStatus::Degraded,
            message: Some(message.into()),
        }
    }

    #[must_use]
    pub fn unhealthy(message: impl Into<String>) -> Self {
        Self {
            status: HealthcheckStatus::Unhealthy,
            message: Some(message.into()),
        }
    }
}

impl Default for HealthcheckResult {
    fn default() -> Self {
        Self::healthy()
    }
}

/// Trait implemented by gears that want to participate in readiness probing.
///
/// The default implementation always returns healthy, so a gear only needs to
/// override `check` when it actually has something to probe.
///
/// # Contract
///
/// - `check` must not panic; panics are caught by [`RestHealthcheckRegistry`] and
///   converted to [`HealthcheckStatus::Unhealthy`].
/// - `check` must complete within the configured per-check timeout
///   (default 500 ms); timeouts are converted to [`HealthcheckStatus::Unhealthy`].
/// - `check` must never leak secrets, passwords, DSNs, or stack traces in its
///   message; the registry sanitises messages but defence-in-depth is preferred.
#[async_trait]
pub trait Healthcheck: Send + Sync + 'static {
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    async fn check(&self) -> HealthcheckResult {
        HealthcheckResult::healthy()
    }
}

/// Per-component readiness report entry.
///
/// Represents the result of a single gear's healthcheck execution, including
/// elapsed time and outcome status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthcheckComponentReport {
    /// Gear name.
    pub gear: String,
    /// Check name from [`Healthcheck::name`].
    pub check: String,
    /// Health status (Healthy, Degraded, or Unhealthy).
    pub status: HealthcheckStatus,
    /// Optional message (sanitized for security).
    pub message: Option<String>,
    /// Elapsed time in milliseconds.
    pub latency_ms: u64,
}

/// Aggregate readiness report produced by [`RestHealthcheckRegistry::report`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthcheckReport {
    pub status: HealthcheckAggregateStatus,
    pub components: Vec<HealthcheckComponentReport>,
}

impl HealthcheckReport {
    /// Returns `true` when the service is ready to receive traffic.
    ///
    /// Both `Healthy` and `Degraded` are considered ready; only `Unhealthy` removes
    /// the pod from the load-balancer pool without triggering a restart.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.status != HealthcheckAggregateStatus::Unhealthy
    }
}

struct RegistryEntry {
    gear: &'static str,
    check: Arc<dyn Healthcheck>,
}

/// Registry of REST healthchecks populated during the REST wiring phase.
///
/// Create one `Arc<RestHealthcheckRegistry>`, register it in `ClientHub`, then
/// call [`register`](Self::register) for each REST gear that provides a
/// [`Healthcheck`]. The API Gateway retrieves the same `Arc` and calls
/// [`report`](Self::report) on every `/readyz` and `/health` request.
#[derive(Default)]
pub struct RestHealthcheckRegistry {
    entries: RwLock<Vec<RegistryEntry>>,
}

impl RestHealthcheckRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a healthcheck for the given gear.
    pub fn register(&self, gear: &'static str, check: Arc<dyn Healthcheck>) {
        self.entries.write().push(RegistryEntry { gear, check });
    }

    /// Run all registered healthchecks concurrently and return an aggregate report.
    ///
    /// Each check runs in its own `tokio::task::spawn` so panics are caught.
    /// Checks that exceed `timeout_per_check` are cancelled and reported as
    /// [`HealthcheckStatus::Unhealthy`].
    pub async fn report(&self, timeout_per_check: Duration) -> HealthcheckReport {
        let entries: Vec<(_, _)> = {
            let r = self.entries.read();
            r.iter().map(|e| (e.gear, e.check.clone())).collect()
        };

        if entries.is_empty() {
            return HealthcheckReport {
                status: HealthcheckAggregateStatus::Healthy,
                components: vec![],
            };
        }

        let handles: Vec<_> = entries
            .into_iter()
            .map(|(gear, check)| tokio::spawn(run_one_check(gear, check, timeout_per_check)))
            .collect();

        let mut components = Vec::with_capacity(handles.len());
        for h in handles {
            match h.await {
                Ok(component) => components.push(component),
                Err(_join_err) => {
                    // The outer spawn panicked; record a generic unhealthy entry.
                    components.push(HealthcheckComponentReport {
                        gear: "unknown".to_owned(),
                        check: "unknown".to_owned(),
                        status: HealthcheckStatus::Unhealthy,
                        message: Some("health check failed".to_owned()),
                        latency_ms: 0,
                    });
                }
            }
        }

        let aggregate = compute_aggregate(&components);
        HealthcheckReport {
            status: aggregate,
            components,
        }
    }
}

async fn run_one_check(
    gear: &'static str,
    check: Arc<dyn Healthcheck>,
    timeout_per_check: Duration,
) -> HealthcheckComponentReport {
    let name = check.name();
    let start = Instant::now();

    let mut handle = tokio::spawn(async move { check.check().await });

    let (status, message) = tokio::select! {
        result = &mut handle => match result {
            Ok(r) => (r.status, r.message.as_deref().map(sanitize_message)),
            Err(_join_err) => (
                HealthcheckStatus::Unhealthy,
                Some("health check failed".to_owned()),
            ),
        },
        () = tokio::time::sleep(timeout_per_check) => {
            handle.abort();
            let _result = handle.await;
            (
                HealthcheckStatus::Unhealthy,
                Some("health check timed out".to_owned()),
            )
        }
    };

    HealthcheckComponentReport {
        gear: gear.to_owned(),
        check: name.to_owned(),
        status,
        message,
        latency_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}

fn compute_aggregate(components: &[HealthcheckComponentReport]) -> HealthcheckAggregateStatus {
    let mut status = HealthcheckAggregateStatus::Healthy;
    for c in components {
        match c.status {
            HealthcheckStatus::Unhealthy => return HealthcheckAggregateStatus::Unhealthy,
            HealthcheckStatus::Degraded => {
                status = HealthcheckAggregateStatus::Degraded;
            }
            HealthcheckStatus::Healthy => {}
        }
    }
    status
}

const MAX_MESSAGE_LEN: usize = 256;

static SUSPICIOUS_SUBSTRINGS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "bearer",
    "authorization",
    "postgres://",
    "postgresql://",
    "mysql://",
    "sqlite://",
    "tenant",
    "select ",
    "insert ",
    "update ",
    "delete ",
    "panic",
    "stack backtrace",
];

fn sanitize_message(msg: &str) -> String {
    if msg.len() > MAX_MESSAGE_LEN {
        return "health check failed".to_owned();
    }
    let lower = msg.to_lowercase();
    for sub in SUSPICIOUS_SUBSTRINGS {
        if lower.contains(sub) {
            return "health check failed".to_owned();
        }
    }
    msg.to_owned()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::sync::Notify;

    struct AlwaysHealthy;
    #[async_trait]
    impl Healthcheck for AlwaysHealthy {
        fn name(&self) -> &'static str {
            "always-healthy"
        }
    }

    struct AlwaysDegraded;
    #[async_trait]
    impl Healthcheck for AlwaysDegraded {
        fn name(&self) -> &'static str {
            "always-degraded"
        }
        async fn check(&self) -> HealthcheckResult {
            HealthcheckResult::degraded("cache warming")
        }
    }

    struct AlwaysUnhealthy;
    #[async_trait]
    impl Healthcheck for AlwaysUnhealthy {
        fn name(&self) -> &'static str {
            "always-unhealthy"
        }
        async fn check(&self) -> HealthcheckResult {
            HealthcheckResult::unhealthy("database unreachable")
        }
    }

    struct SlowCheck;
    #[async_trait]
    impl Healthcheck for SlowCheck {
        fn name(&self) -> &'static str {
            "slow-check"
        }
        async fn check(&self) -> HealthcheckResult {
            tokio::time::sleep(Duration::from_secs(10)).await;
            HealthcheckResult::healthy()
        }
    }

    struct AbortTrackingCheck {
        entered: Arc<Notify>,
        dropped: Arc<AtomicBool>,
    }

    #[async_trait]
    impl Healthcheck for AbortTrackingCheck {
        fn name(&self) -> &'static str {
            "abort-tracking-check"
        }
        async fn check(&self) -> HealthcheckResult {
            struct DropFlag(Arc<AtomicBool>);
            impl Drop for DropFlag {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }

            let _drop_flag = DropFlag(self.dropped.clone());
            self.entered.notify_one();
            std::future::pending::<HealthcheckResult>().await
        }
    }

    struct PanickingCheck;
    #[async_trait]
    impl Healthcheck for PanickingCheck {
        fn name(&self) -> &'static str {
            "panicking-check"
        }
        async fn check(&self) -> HealthcheckResult {
            panic!("intentional panic in healthcheck");
        }
    }

    #[test]
    fn healthcheck_result_default_is_healthy() {
        let r = HealthcheckResult::default();
        assert_eq!(r.status, HealthcheckStatus::Healthy);
        assert!(r.message.is_none());
    }

    #[tokio::test]
    async fn default_healthcheck_check_returns_healthy() {
        let r = AlwaysHealthy.check().await;
        assert_eq!(r.status, HealthcheckStatus::Healthy);
    }

    #[tokio::test]
    async fn empty_registry_report_is_healthy_and_ready() {
        let registry = RestHealthcheckRegistry::new();
        let report = registry.report(Duration::from_millis(500)).await;
        assert_eq!(report.status, HealthcheckAggregateStatus::Healthy);
        assert!(report.is_ready());
        assert!(report.components.is_empty());
    }

    #[tokio::test]
    async fn degraded_component_makes_aggregate_degraded_and_still_ready() {
        let registry = RestHealthcheckRegistry::new();
        registry.register("my-gear", Arc::new(AlwaysDegraded));
        let report = registry.report(Duration::from_millis(500)).await;
        assert_eq!(report.status, HealthcheckAggregateStatus::Degraded);
        assert!(report.is_ready());
    }

    #[tokio::test]
    async fn unhealthy_component_makes_aggregate_unhealthy_and_not_ready() {
        let registry = RestHealthcheckRegistry::new();
        registry.register("my-gear", Arc::new(AlwaysUnhealthy));
        let report = registry.report(Duration::from_millis(500)).await;
        assert_eq!(report.status, HealthcheckAggregateStatus::Unhealthy);
        assert!(!report.is_ready());
    }

    #[tokio::test]
    async fn slow_check_times_out_and_becomes_unhealthy() {
        let registry = RestHealthcheckRegistry::new();
        registry.register("slow-gear", Arc::new(SlowCheck));
        let report = registry.report(Duration::from_millis(100)).await;
        assert_eq!(report.status, HealthcheckAggregateStatus::Unhealthy);
        let comp = &report.components[0];
        assert_eq!(comp.status, HealthcheckStatus::Unhealthy);
    }

    #[tokio::test]
    async fn timed_out_check_is_aborted() {
        let registry = RestHealthcheckRegistry::new();
        let entered = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        registry.register(
            "slow-gear",
            Arc::new(AbortTrackingCheck {
                entered: entered.clone(),
                dropped: dropped.clone(),
            }),
        );

        let report_task =
            tokio::spawn(async move { registry.report(Duration::from_millis(10)).await });
        entered.notified().await;
        let report = report_task.await.expect("report task panicked");

        assert_eq!(report.status, HealthcheckAggregateStatus::Unhealthy);
        assert!(
            dropped.load(Ordering::SeqCst),
            "timed-out healthcheck future must be dropped after abort"
        );
    }

    #[tokio::test]
    async fn panicking_check_becomes_unhealthy_without_panicking_caller() {
        let registry = RestHealthcheckRegistry::new();
        registry.register("panic-gear", Arc::new(PanickingCheck));
        let report = registry.report(Duration::from_millis(500)).await;
        assert_eq!(report.status, HealthcheckAggregateStatus::Unhealthy);
    }

    #[test]
    fn sensitive_message_is_sanitized() {
        assert_eq!(
            sanitize_message("connection to postgres://user:pass@host/db failed"),
            "health check failed"
        );
        assert_eq!(
            sanitize_message("invalid token in header"),
            "health check failed"
        );
        assert_eq!(
            sanitize_message("SELECT * FROM users failed"),
            "health check failed"
        );
        assert_eq!(
            sanitize_message("thread panicked at some_file.rs:42"),
            "health check failed"
        );
    }

    #[test]
    fn long_message_is_sanitized() {
        let long = "x".repeat(257);
        assert_eq!(sanitize_message(&long), "health check failed");
    }

    #[test]
    fn clean_message_passes_through() {
        assert_eq!(
            sanitize_message("upstream service unavailable"),
            "upstream service unavailable"
        );
    }
}
