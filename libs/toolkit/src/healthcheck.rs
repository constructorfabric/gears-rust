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
//! `/health` is the detailed diagnostic endpoint — its JSON body includes per-component
//! check names and messages.
//!
//! Probe port and path depend on the serving mode (`api-gateway` `health.serve`): with `main`
//! or `both` the endpoints ride the main gateway listener (target the main `http` port) and
//! inherit `prefix_path` like any other route (e.g. `/cf/healthz`); with `separate` they are
//! served only on the health listener (`health.bind_addr`) at unprefixed paths, so probes must
//! target that port and the bare `/healthz`/`/readyz`/`/health` paths.
//!
//! # Threat model
//!
//! All three endpoints are **unauthenticated in every serving mode**: on the main listener
//! they are registered as explicit public routes so the auth layer waves them through without
//! a bearer token, and the separate listener runs no auth at all. They are protected by
//! deployment controls, not credentials: a `separate` health listener is expected to sit on a
//! private/management network, and `main`-mounted endpoints inherit whatever network placement
//! the main gateway has.
//! Because `/health` can expose per-component names and messages without auth, check messages
//! are sanitized (see [`sanitize_message`]) so a secret, DSN, credential, or panic backtrace
//! cannot leak to an unauthenticated caller.

use async_trait::async_trait;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

/// Single-check and aggregate readiness status.
/// Serialized lowercase into `/health`/`/readyz`; variants are a stable API contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthcheckStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

/// Result of one [`Healthcheck::check`]; fields are part of the stable `/health` JSON contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthcheckResult {
    pub status: HealthcheckStatus,
    pub message: Option<String>,
    /// Stable, machine-readable code (e.g. `"db_unreachable"`), surfaced on `/health`. Lets
    /// operators alert on a stable signal even when the human-readable `message` is collapsed
    /// by sanitization. Because `/health` is unauthenticated, the code is constrained (see
    /// [`sanitize_code`]) to a short `[a-z0-9_.-]` identifier and dropped if it does not
    /// conform — it must never carry secrets or free-form detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl HealthcheckResult {
    #[must_use]
    pub fn healthy() -> Self {
        Self {
            status: HealthcheckStatus::Healthy,
            message: None,
            code: None,
        }
    }

    #[must_use]
    pub fn degraded(message: impl Into<String>) -> Self {
        Self {
            status: HealthcheckStatus::Degraded,
            message: Some(message.into()),
            code: None,
        }
    }

    #[must_use]
    pub fn unhealthy(message: impl Into<String>) -> Self {
        Self {
            status: HealthcheckStatus::Unhealthy,
            message: Some(message.into()),
            code: None,
        }
    }

    /// Attach a stable machine-readable code (see [`HealthcheckResult::code`]).
    #[must_use]
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }
}

impl Default for HealthcheckResult {
    fn default() -> Self {
        Self::healthy()
    }
}

/// Readiness probe implemented by a gear.
///
/// `name` must be an explicit human-readable id (no type paths); it is exposed on
/// `/health`. `check` must be cancellation-safe and should not leak secrets in its
/// message (the registry sanitises anyway); panics and per-check timeouts are caught
/// and mapped to [`HealthcheckStatus::Unhealthy`]. Default `check` returns healthy.
#[async_trait]
pub trait Healthcheck: Send + Sync + 'static {
    /// Human-readable check name, exposed verbatim in `/health` JSON.
    fn name(&self) -> &'static str;

    async fn check(&self) -> HealthcheckResult {
        HealthcheckResult::healthy()
    }
}

/// One gear's healthcheck result. All fields are part of the stable `/health` JSON contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthcheckComponentReport {
    pub gear: String,
    pub check: String,
    pub status: HealthcheckStatus,
    /// Sanitized (see [`sanitize_message`]).
    pub message: Option<String>,
    /// Stable machine-readable code from [`HealthcheckResult::code`]; passed through unsanitized.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub latency_ms: u64,
}

/// Aggregate report from [`RestHealthcheckRegistry::report`]; stable `/health`/`/readyz` JSON contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthcheckReport {
    pub status: HealthcheckStatus,
    pub components: Vec<HealthcheckComponentReport>,
}

impl HealthcheckReport {
    /// Ready unless `Unhealthy` (`Healthy` and `Degraded` both keep the pod in rotation).
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.status != HealthcheckStatus::Unhealthy
    }
}

struct RegistryEntry {
    gear: String,
    check: Arc<dyn Healthcheck>,
}

/// Bursty probes within this window reuse the last report instead of re-running checks.
/// Kept > the default per-check timeout (500 ms) so a timed-out check still buffers.
const REPORT_CACHE_TTL: Duration = Duration::from_secs(2);

/// Holds the REST healthchecks registered during REST wiring; the gateway calls
/// [`report`](Self::report) on every `/readyz` and `/health` request.
#[derive(Default)]
pub struct RestHealthcheckRegistry {
    entries: RwLock<Vec<RegistryEntry>>,
    cached_report: AsyncMutex<Option<(Instant, HealthcheckReport)>>,
    /// Runtime shutdown token; aborts in-flight checks. Default never fires.
    cancel: CancellationToken,
}

impl RestHealthcheckRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry whose in-flight checks are aborted when `cancel` fires.
    #[must_use]
    pub fn with_cancellation(cancel: CancellationToken) -> Self {
        Self {
            cancel,
            ..Self::default()
        }
    }

    /// Register a healthcheck for the given gear.
    pub fn register(&self, gear: impl Into<String>, check: Arc<dyn Healthcheck>) {
        self.entries.write().push(RegistryEntry {
            gear: gear.into(),
            check,
        });
    }

    /// Run all checks concurrently, aggregate, and cache for [`REPORT_CACHE_TTL`].
    /// Panics and per-check timeouts map to [`HealthcheckStatus::Unhealthy`]. The cache
    /// lock is held across the compute so concurrent cache-misses coalesce into one fan-out.
    pub async fn report(&self, timeout_per_check: Duration) -> HealthcheckReport {
        let mut guard = self.cached_report.lock().await;
        if let Some((ts, cached)) = guard.as_ref()
            && ts.elapsed() < REPORT_CACHE_TTL
        {
            return cached.clone();
        }

        let entries: Vec<(_, _)> = {
            let r = self.entries.read();
            r.iter()
                .map(|e| (e.gear.clone(), e.check.clone()))
                .collect()
        };

        let report = if entries.is_empty() {
            HealthcheckReport {
                status: HealthcheckStatus::Healthy,
                components: vec![],
            }
        } else {
            let cancel = &self.cancel;
            let checks = entries
                .into_iter()
                .map(|(gear, check)| run_one_check(gear, check, timeout_per_check, cancel));
            let components = futures_util::future::join_all(checks).await;
            let aggregate = compute_aggregate(&components);
            HealthcheckReport {
                status: aggregate,
                components,
            }
        };

        *guard = Some((Instant::now(), report.clone()));
        report
    }
}

async fn run_one_check(
    gear: String,
    check: Arc<dyn Healthcheck>,
    timeout_per_check: Duration,
    cancel: &CancellationToken,
) -> HealthcheckComponentReport {
    let name = check.name();
    let start = Instant::now();

    let mut handle = tokio::spawn(async move { check.check().await });

    let (status, message, code) = tokio::select! {
        result = &mut handle => match result {
            Ok(r) => (
                r.status,
                r.message.as_deref().map(sanitize_message),
                r.code.as_deref().and_then(sanitize_code),
            ),
            Err(join_err) => {
                tracing::error!(gear = %gear, check = name, error = %join_err, "healthcheck task panicked");
                (
                    HealthcheckStatus::Unhealthy,
                    Some("health check failed".to_owned()),
                    None,
                )
            }
        },
        () = tokio::time::sleep(timeout_per_check) => {
            handle.abort();
            if let Err(join_err) = handle.await
                && !join_err.is_cancelled()
            {
                tracing::warn!(gear = %gear, check = name, error = %join_err, "healthcheck task join error after timeout abort");
            }
            tracing::warn!(gear = %gear, check = name, timeout_ms = timeout_per_check.as_millis(), "healthcheck timed out");
            (
                HealthcheckStatus::Unhealthy,
                Some("health check timed out".to_owned()),
                None,
            )
        }
        () = cancel.cancelled() => {
            handle.abort();
            if let Err(join_err) = handle.await
                && !join_err.is_cancelled()
            {
                tracing::warn!(gear = %gear, check = name, error = %join_err, "healthcheck task join error after shutdown abort");
            }
            tracing::warn!(gear = %gear, check = name, "healthcheck cancelled by runtime shutdown");
            (
                HealthcheckStatus::Unhealthy,
                Some("health check cancelled".to_owned()),
                None,
            )
        }
    };

    HealthcheckComponentReport {
        gear,
        check: name.to_owned(),
        status,
        message,
        code,
        latency_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}

fn compute_aggregate(components: &[HealthcheckComponentReport]) -> HealthcheckStatus {
    let mut status = HealthcheckStatus::Healthy;
    for c in components {
        match c.status {
            HealthcheckStatus::Unhealthy => return HealthcheckStatus::Unhealthy,
            HealthcheckStatus::Degraded => {
                status = HealthcheckStatus::Degraded;
            }
            HealthcheckStatus::Healthy => {}
        }
    }
    status
}

const MAX_MESSAGE_LEN: usize = 256;

// Blocklist targeting the actual threat: `/health` is unauthenticated (protected only by
// private listener/network placement), so a check message must never carry a secret, DSN,
// credential, or panic backtrace to an anonymous caller. Deliberately excludes operator-useful
// terms (tenant ids, SQL verbs, "private" network hints) that are not secrets — keeping them
// readable makes the endpoint actually diagnostic. Matched substrings collapse the message to
// the generic string.
static SUSPICIOUS_SUBSTRINGS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "bearer",
    "authorization",
    "api_key",
    "apikey",
    "private_key",
    "credential",
    "access_key",
    "aws_",
    "client_secret",
    "postgres://",
    "postgresql://",
    "mysql://",
    "sqlite://",
    "mongodb://",
    "redis://",
    "amqp://",
    "jdbc:",
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

const MAX_CODE_LEN: usize = 64;

/// Validate a health `code` for exposure on the unauthenticated `/health` endpoint.
///
/// A code must be a short machine identifier, not free text — so unlike `message` (which is
/// scrubbed by a blocklist), `code` is validated by an allowlist: non-empty, `<= MAX_CODE_LEN`,
/// and only `[a-z0-9_.-]`. Non-conforming codes are dropped (`None`) rather than surfaced,
/// closing the hole where a permissive code could smuggle a secret past `sanitize_message`.
fn sanitize_code(code: &str) -> Option<String> {
    let ok = !code.is_empty()
        && code.len() <= MAX_CODE_LEN
        && code.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
        });
    if ok {
        Some(code.to_owned())
    } else {
        tracing::debug!(
            code,
            "health check code dropped: not a [a-z0-9_.-] identifier"
        );
        None
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::sync::Notify;

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
    async fn empty_registry_report_is_healthy_and_ready() {
        let registry = RestHealthcheckRegistry::new();
        let report = registry.report(Duration::from_millis(500)).await;
        assert_eq!(report.status, HealthcheckStatus::Healthy);
        assert!(report.is_ready());
        assert!(report.components.is_empty());
    }

    #[tokio::test]
    async fn unhealthy_component_makes_aggregate_unhealthy_and_not_ready() {
        let registry = RestHealthcheckRegistry::new();
        registry.register("my-gear", Arc::new(AlwaysUnhealthy));
        let report = registry.report(Duration::from_millis(500)).await;
        assert_eq!(report.status, HealthcheckStatus::Unhealthy);
        assert!(!report.is_ready());
    }

    #[tokio::test]
    async fn mixed_degraded_and_unhealthy_aggregate_is_unhealthy() {
        let registry = RestHealthcheckRegistry::new();
        registry.register("degraded-gear", Arc::new(AlwaysDegraded));
        registry.register("unhealthy-gear", Arc::new(AlwaysUnhealthy));
        let report = registry.report(Duration::from_millis(500)).await;
        assert_eq!(
            report.status,
            HealthcheckStatus::Unhealthy,
            "unhealthy must take priority over degraded"
        );
        assert!(!report.is_ready());
    }

    #[tokio::test]
    async fn slow_check_times_out_and_becomes_unhealthy() {
        let registry = RestHealthcheckRegistry::new();
        registry.register("slow-gear", Arc::new(SlowCheck));
        let report = registry.report(Duration::from_millis(100)).await;
        assert_eq!(report.status, HealthcheckStatus::Unhealthy);
        let comp = &report.components[0];
        assert_eq!(comp.status, HealthcheckStatus::Unhealthy);
        assert_eq!(comp.message.as_deref(), Some("health check timed out"));
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

        assert_eq!(report.status, HealthcheckStatus::Unhealthy);
        assert!(
            dropped.load(Ordering::SeqCst),
            "timed-out healthcheck future must be dropped after abort"
        );
    }

    #[tokio::test]
    async fn cancelled_registry_aborts_in_flight_check_and_reports_unhealthy() {
        let cancel = CancellationToken::new();
        let registry = RestHealthcheckRegistry::with_cancellation(cancel.clone());
        let entered = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        registry.register(
            "slow-gear",
            Arc::new(AbortTrackingCheck {
                entered: entered.clone(),
                dropped: dropped.clone(),
            }),
        );

        // Long per-check timeout so cancellation, not the timeout, ends the check.
        let report_task =
            tokio::spawn(async move { registry.report(Duration::from_mins(1)).await });
        entered.notified().await;
        cancel.cancel();
        let report = report_task.await.expect("report task panicked");

        assert_eq!(report.status, HealthcheckStatus::Unhealthy);
        assert_eq!(
            report.components[0].message.as_deref(),
            Some("health check cancelled")
        );
        assert!(
            dropped.load(Ordering::SeqCst),
            "cancelled healthcheck future must be dropped after abort"
        );
    }

    #[tokio::test]
    async fn panicking_check_becomes_unhealthy_without_panicking_caller() {
        let registry = RestHealthcheckRegistry::new();
        registry.register("panic-gear", Arc::new(PanickingCheck));
        let report = registry.report(Duration::from_millis(500)).await;
        assert_eq!(report.status, HealthcheckStatus::Unhealthy);
    }

    #[tokio::test]
    async fn second_call_within_ttl_reuses_cached_report_without_rerunning_checks() {
        use std::sync::atomic::AtomicUsize;

        struct CountingCheck(Arc<AtomicUsize>);
        #[async_trait]
        impl Healthcheck for CountingCheck {
            fn name(&self) -> &'static str {
                "counting-check"
            }
            async fn check(&self) -> HealthcheckResult {
                self.0.fetch_add(1, Ordering::SeqCst);
                HealthcheckResult::healthy()
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let registry = RestHealthcheckRegistry::new();
        registry.register("counted-gear", Arc::new(CountingCheck(calls.clone())));

        let first = registry.report(Duration::from_millis(500)).await;
        let second = registry.report(Duration::from_millis(500)).await;

        assert_eq!(first.status, HealthcheckStatus::Healthy);
        assert_eq!(second.status, HealthcheckStatus::Healthy);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "second report() within REPORT_CACHE_TTL must reuse the cached result"
        );
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
            sanitize_message("thread panicked at some_file.rs:42"),
            "health check failed"
        );
        assert_eq!(
            sanitize_message("invalid api_key provided"),
            "health check failed"
        );
        assert_eq!(
            sanitize_message("connection to mongodb://host/db failed"),
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

    #[test]
    fn operator_useful_messages_pass_through() {
        // These are not secrets and are useful when diagnosing an unhealthy pod, so the
        // narrowed blocklist deliberately leaves them intact.
        for msg in [
            "tenant acme-corp migration pending",
            "SELECT on read replica timed out",
            "private subnet route unreachable",
        ] {
            assert_eq!(sanitize_message(msg), msg);
        }
    }

    #[test]
    fn code_allowlist_accepts_identifiers_and_drops_the_rest() {
        assert_eq!(
            sanitize_code("db_unreachable"),
            Some("db_unreachable".to_owned())
        );
        assert_eq!(sanitize_code("v1.2-beta_3"), Some("v1.2-beta_3".to_owned()));
        // Rejected: empty, uppercase, whitespace/free text, over length.
        assert_eq!(sanitize_code(""), None);
        assert_eq!(sanitize_code("DB_UNREACHABLE"), None);
        assert_eq!(sanitize_code("postgres://user:pw@host/db"), None);
        assert_eq!(sanitize_code("connection failed"), None);
        assert_eq!(sanitize_code(&"a".repeat(MAX_CODE_LEN + 1)), None);
    }

    #[tokio::test]
    async fn component_report_carries_conforming_code() {
        struct CodedUnhealthy;
        #[async_trait]
        impl Healthcheck for CodedUnhealthy {
            fn name(&self) -> &'static str {
                "coded"
            }
            async fn check(&self) -> HealthcheckResult {
                HealthcheckResult::unhealthy("database unreachable").with_code("db_unreachable")
            }
        }

        let registry = RestHealthcheckRegistry::new();
        registry.register("gear", Arc::new(CodedUnhealthy));
        let report = registry.report(Duration::from_millis(500)).await;
        assert_eq!(report.components[0].code.as_deref(), Some("db_unreachable"));
    }

    #[tokio::test]
    async fn component_report_drops_nonconforming_code() {
        struct BadCode;
        #[async_trait]
        impl Healthcheck for BadCode {
            fn name(&self) -> &'static str {
                "bad-code"
            }
            async fn check(&self) -> HealthcheckResult {
                // A code carrying free-form detail (a DSN) must never reach the report.
                HealthcheckResult::unhealthy("down").with_code("postgres://u:p@host/db")
            }
        }

        let registry = RestHealthcheckRegistry::new();
        registry.register("gear", Arc::new(BadCode));
        let report = registry.report(Duration::from_millis(500)).await;
        assert!(report.components[0].code.is_none());
    }
}
