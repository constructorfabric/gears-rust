//! `TimescaleDB` usage-collector plugin module.
//!
//! Registers a GTS plugin instance in the types registry and exposes
//! [`usage_collector_sdk::UsageCollectorPluginClientV1`] backed by a `TimescaleDB` connection pool.
//!
//! The framework lint banning raw sqlx is suppressed in this file because the
//! plugin bootstrap is the only place that legitimately constructs the pool and
//! issues the TimescaleDB-specific liveness probe; everything else flows
//! through `infra::*` ports.
//
// MODKIT-DEVIATION: de0706_no_direct_sqlx (a.k.a. MODKIT-DB-002 / MODKIT-SEC-001).
// The allow is file-wide because the bootstrap + supervised background tasks all
// need direct sqlx access (PgPoolOptions, `SELECT 1` liveness probe, cleanup
// loop). The substitute authorization boundary is the scope-fragment translator
// in `domain/scope.rs` — see the "Architectural deviation" section in
// `README.md` and ADR-0003 (`../../docs/ADR/0003-cpt-cf-usage-collector-adr-timescaledb-plugin-raw-sqlx.md`)
// for the full rationale and the conditions under which a SecureConn-equivalent
// would be preferred. Maintainers extending this file MUST consult those docs
// before adding sqlx code paths outside the bootstrap + supervisor concerns.
#![allow(unknown_lints, de0706_no_direct_sqlx)]

use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use modkit::Module;
use modkit::client_hub::ClientScope;
use modkit::context::ModuleCtx;
use modkit::gts::BaseModkitPluginV1;
use opentelemetry::KeyValue;
use opentelemetry::global;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::future::Future;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use types_registry_sdk::{RegisterResult, TypesRegistryClient};
use usage_collector_sdk::{UsageCollectorPluginClientV1, UsageCollectorPluginSpecV1};

use crate::config::TimescaleDbConfig;
use crate::domain::client::TimescaleDbPluginClient;
use crate::infra::continuous_aggregate::setup_continuous_aggregate;
use crate::infra::migrations::run_migrations;
use crate::infra::otel_metrics::OtelPluginMetrics;
use crate::infra::pg_insert_port::PgInsertPort;
use crate::infra::pg_query_port::PgQueryPort;
use crate::infra::retention::{cleanup_idempotency_keys, setup_retention_policy};

/// `TimescaleDB` production storage plugin for the usage-collector gateway.
///
/// `deps` declares the runtime startup-order — the gateway and types-registry
/// modules must initialize before this plugin so it can register its spec and
/// the produced client trait. This is intentionally distinct from the Cargo
/// dependency graph (which only references `usage-collector-sdk`, not the
/// `usage-collector` binary crate).
#[modkit::module(
    name = "timescaledb-usage-collector-plugin",
    deps = ["types-registry", "usage-collector"]
)]
#[derive(Default)]
struct TimescaleDbUsageCollectorPlugin;

#[async_trait]
impl Module for TimescaleDbUsageCollectorPlugin {
    // @cpt-dod:cpt-cf-usage-collector-dod-production-storage-plugin-plugin-crate:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-production-storage-plugin-encryption-and-gts:p1
    // @cpt-flow:cpt-cf-usage-collector-flow-production-storage-plugin-operator-schema-migration:p1
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        // @cpt-begin:cpt-cf-usage-collector-flow-production-storage-plugin-operator-schema-migration:p1:inst-flow-smig-1
        // Entry point: platform operator has invoked plugin startup (CLI or gateway startup flag).
        // @cpt-end:cpt-cf-usage-collector-flow-production-storage-plugin-operator-schema-migration:p1:inst-flow-smig-1

        // @cpt-begin:cpt-cf-usage-collector-dod-production-storage-plugin-plugin-crate:p1:inst-validate-config
        // Errors propagate with `.context(..)` so a single log point at the module
        // loader records each failure once. We do not also `inspect_err(..)` here:
        // double-logging the same error at multiple layers inflates operator-facing
        // log volume during startup failures.
        let cfg: TimescaleDbConfig = ctx
            .config()
            .context("TimescaleDB plugin configuration load failed")?;
        cfg.validate()
            .context("TimescaleDB plugin configuration validation failed")?;
        // @cpt-end:cpt-cf-usage-collector-dod-production-storage-plugin-plugin-crate:p1:inst-validate-config

        // @cpt-begin:cpt-cf-usage-collector-dod-production-storage-plugin-encryption-and-gts:p1:inst-build-secure-conn
        // TLS is enforced: database_url validated above to contain sslmode=require.
        // The URL is captured here and never written to logs or error messages.
        let database_url = cfg.database_url.clone();
        // @cpt-end:cpt-cf-usage-collector-dod-production-storage-plugin-encryption-and-gts:p1:inst-build-secure-conn

        // @cpt-begin:cpt-cf-usage-collector-dod-production-storage-plugin-plugin-crate:p1:inst-build-pool
        let pool = PgPoolOptions::new()
            .min_connections(cfg.pool_size_min)
            .max_connections(cfg.pool_size_max)
            .acquire_timeout(cfg.connection_timeout)
            .connect(&database_url)
            .await
            .map_err(|e| anyhow::Error::from(e).context("connection pool initialization failed"))?;
        // @cpt-end:cpt-cf-usage-collector-dod-production-storage-plugin-plugin-crate:p1:inst-build-pool

        // @cpt-begin:cpt-cf-usage-collector-flow-production-storage-plugin-operator-schema-migration:p1:inst-flow-smig-2
        // @cpt-begin:cpt-cf-usage-collector-dod-production-storage-plugin-plugin-crate:p1:inst-run-migrations
        // @cpt-begin:cpt-cf-usage-collector-flow-production-storage-plugin-operator-schema-migration:p1:inst-flow-smig-4
        run_migrations(&pool)
            .await
            .map_err(|e| anyhow::Error::from(e).context("schema migration failed"))?;
        // @cpt-end:cpt-cf-usage-collector-flow-production-storage-plugin-operator-schema-migration:p1:inst-flow-smig-4
        // @cpt-end:cpt-cf-usage-collector-dod-production-storage-plugin-plugin-crate:p1:inst-run-migrations
        // @cpt-end:cpt-cf-usage-collector-flow-production-storage-plugin-operator-schema-migration:p1:inst-flow-smig-2

        // @cpt-begin:cpt-cf-usage-collector-flow-production-storage-plugin-operator-schema-migration:p1:inst-flow-smig-3
        info!("TimescaleDB schema migration completed; all schema objects are present");
        // @cpt-end:cpt-cf-usage-collector-flow-production-storage-plugin-operator-schema-migration:p1:inst-flow-smig-3

        // @cpt-begin:cpt-cf-usage-collector-flow-production-storage-plugin-operator-schema-migration:p1:inst-flow-smig-3a
        // @cpt-begin:cpt-cf-usage-collector-dod-production-storage-plugin-plugin-crate:p1:inst-setup-continuous-aggregate
        // @cpt-begin:cpt-cf-usage-collector-flow-production-storage-plugin-operator-schema-migration:p1:inst-flow-smig-3c
        setup_continuous_aggregate(&pool)
            .await
            .map_err(|e| anyhow::Error::from(e).context("continuous aggregate setup failed"))?;
        // @cpt-end:cpt-cf-usage-collector-flow-production-storage-plugin-operator-schema-migration:p1:inst-flow-smig-3c
        // @cpt-end:cpt-cf-usage-collector-dod-production-storage-plugin-plugin-crate:p1:inst-setup-continuous-aggregate
        // @cpt-end:cpt-cf-usage-collector-flow-production-storage-plugin-operator-schema-migration:p1:inst-flow-smig-3a

        // @cpt-begin:cpt-cf-usage-collector-flow-production-storage-plugin-operator-schema-migration:p1:inst-flow-smig-3b
        info!("TimescaleDB continuous aggregate setup complete; returning success to operator");
        // @cpt-end:cpt-cf-usage-collector-flow-production-storage-plugin-operator-schema-migration:p1:inst-flow-smig-3b

        setup_retention_policy(&pool, cfg.retention_default, cfg.idempotency_retention)
            .await
            .map_err(|e| anyhow::Error::from(e).context("retention policy setup failed"))?;

        let instance_id = UsageCollectorPluginSpecV1::gts_make_instance_id(
            "cf.core._.timescaledb_usage_collector_plugin.v1",
        );

        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let instance = BaseModkitPluginV1::<UsageCollectorPluginSpecV1> {
            id: instance_id.clone(),
            vendor: "virtuozzo".to_owned(),
            priority: 10,
            properties: UsageCollectorPluginSpecV1,
        };
        let instance_json =
            serde_json::to_value(&instance).context("GTS instance JSON encoding failed")?;

        // @cpt-begin:cpt-cf-usage-collector-dod-production-storage-plugin-encryption-and-gts:p1:inst-register-gts
        let results = registry
            .register(vec![instance_json])
            .await
            .map_err(|e| anyhow::Error::from(e).context("GTS registration failed"))?;
        RegisterResult::ensure_all_ok(&results)
            .context("GTS registration rejected for TimescaleDB plugin")?;
        info!(%instance_id, "GTS registration successful for TimescaleDB plugin");
        // @cpt-end:cpt-cf-usage-collector-dod-production-storage-plugin-encryption-and-gts:p1:inst-register-gts

        let insert_port: Arc<dyn crate::domain::insert_port::InsertPort> =
            Arc::new(PgInsertPort::new(pool.clone()));
        let query_port: Arc<dyn crate::domain::query_port::QueryPort> =
            Arc::new(PgQueryPort::new(pool.clone()));
        let metrics: Arc<dyn crate::domain::metrics::PluginMetrics> =
            Arc::new(OtelPluginMetrics::new());
        let client = TimescaleDbPluginClient::new(insert_port, query_port, metrics);
        let api: Arc<dyn UsageCollectorPluginClientV1> = Arc::new(client);

        // @cpt-begin:cpt-cf-usage-collector-dod-production-storage-plugin-plugin-crate:p1:inst-register-client
        ctx.client_hub()
            .register_scoped::<dyn UsageCollectorPluginClientV1>(
                ClientScope::gts_id(&instance_id),
                api,
            );
        // @cpt-end:cpt-cf-usage-collector-dod-production-storage-plugin-plugin-crate:p1:inst-register-client

        info!(
            %instance_id,
            "TimescaleDB usage-collector plugin started successfully"
        );

        // The background loops terminate cleanly on cancellation; no JoinHandles
        // are retained on the module struct because the runtime shuts the tasks
        // down with the module. To avoid silently losing panics or unexpected
        // exits, each task is wrapped by a supervisor that awaits its
        // JoinHandle and emits a tracing event on completion / panic — see
        // `supervise_background_task` (RUST-OBS-001 / RUST-ASYNC-001).
        supervise_background_task(
            "storage_health_check",
            run_health_check_loop(pool.clone(), ctx.cancellation_token().clone()),
        );
        supervise_background_task(
            "idempotency_cleanup",
            run_idempotency_cleanup_loop(
                pool,
                cfg.idempotency_retention,
                ctx.cancellation_token().clone(),
            ),
        );

        Ok(())
    }
}

/// Spawns `task` and a supervisor that observes its completion.
///
/// The supervisor awaits the inner `JoinHandle` and emits a structured log on
/// completion: `info` for a clean exit, `error` for a panic, `warn` for a
/// cancellation from outside the task. Without this wrapper, a panicking
/// background loop is swallowed by tokio and the gauge / cleanup work stops
/// with no operator-visible signal.
fn supervise_background_task<F>(name: &'static str, task: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    let handle = tokio::spawn(task);
    tokio::spawn(async move {
        match handle.await {
            Ok(()) => info!(task = name, "background task exited"),
            Err(e) if e.is_cancelled() => {
                warn!(task = name, "background task cancelled by runtime");
            }
            Err(e) => {
                // tokio captures the panic payload in `JoinError`; surface it
                // so the operator sees why the loop stopped instead of just
                // observing a stale gauge.
                error!(task = name, error = %e, "background task panicked");
            }
        }
    });
}

// OpenTelemetry gauges retain their last recorded value indefinitely on the
// reader side, so once this loop exits with `reason=pool_closed` the metric
// keeps that value until the process restarts. Downstream dashboards/alerts
// that need to distinguish "process exited" from "process still reporting"
// should monitor the absence of fresh readings (staleness), not the gauge
// value alone. See finding RUST-ASYNC-001.
async fn run_health_check_loop(pool: PgPool, cancel: CancellationToken) {
    let meter = global::meter("timescaledb-usage-collector-plugin");
    let gauge = meter.f64_gauge("storage_health_status").build();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
    // Default Burst behavior would fire a flurry of catch-up ticks after a
    // runtime pause (debug breakpoint, GC pause, host suspend/resume),
    // emitting a burst of `SELECT 1` probes and gauge readings. Delay paces
    // the next tick to a full period after the resume instead.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Tracks the most recently observed reason so failure logs are emitted on
    // transition only: a sustained outage would otherwise produce ~120
    // `error!` lines per hour at identical content, drowning out unrelated
    // signal in operator logs. The gauge still records every tick so dashboards
    // detect outages via staleness, not via log volume.
    let mut last_reason: Option<&'static str> = None;
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = interval.tick() => {}
        }
        if pool.is_closed() {
            // Distinct from a probe failure: the pool has been shut down and no further
            // queries will succeed. Tagged so dashboards can separate shutdown from outage.
            gauge.record(0.0_f64, &[KeyValue::new("reason", "pool_closed")]);
            break;
        }
        let (_, reason, error_detail) = health_check(&pool, &gauge).await;
        log_health_transition(last_reason, reason, error_detail.as_deref());
        last_reason = Some(reason);
    }
}

/// Severity decision for a health-check tick given the prior tick's reason.
/// Pulled out as a pure function so it can be unit-tested without the
/// tracing-macro expansion that drove the loop's cognitive-complexity warning.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum HealthLog {
    InitialFailure,
    PersistentFailure,
    Recovery,
    Healthy,
}

fn classify_health_transition(
    last_reason: Option<&'static str>,
    reason: &'static str,
) -> HealthLog {
    match (last_reason, reason) {
        (Some("probe_failed"), "probe_failed") => HealthLog::PersistentFailure,
        (_, "probe_failed") => HealthLog::InitialFailure,
        (Some("probe_failed"), "healthy") => HealthLog::Recovery,
        _ => HealthLog::Healthy,
    }
}

// The four `tracing` macros below each expand to enough code that clippy
// flags this small match as "complex"; the same allow is used on
// `run_idempotency_cleanup_loop` above for the same reason.
#[allow(clippy::cognitive_complexity)]
fn log_health_transition(
    last_reason: Option<&'static str>,
    reason: &'static str,
    error_detail: Option<&str>,
) {
    let detail = error_detail.unwrap_or("");
    match classify_health_transition(last_reason, reason) {
        HealthLog::InitialFailure => {
            error!(error_chain = %detail, "TimescaleDB health check failed");
        }
        HealthLog::PersistentFailure => {
            debug!("TimescaleDB health check still failing");
        }
        HealthLog::Recovery => {
            info!("TimescaleDB health check recovered");
        }
        HealthLog::Healthy => {
            debug!("TimescaleDB health check passed");
        }
    }
}

/// Interval between successive idempotency-key cleanup passes.
///
/// One hour is far less frequent than ingest pressure on the table but easily
/// frequent enough to keep growth bounded across multi-day process lifetimes.
const IDEMPOTENCY_CLEANUP_INTERVAL: std::time::Duration = std::time::Duration::from_hours(1);

/// Periodically deletes expired rows from `usage_idempotency_keys`.
///
/// `setup_retention_policy` runs the same cleanup once at startup; this task
/// keeps it from accumulating on long-running processes since the plain table
/// has no `TimescaleDB` retention job attached to it.
#[allow(clippy::cognitive_complexity)]
async fn run_idempotency_cleanup_loop(
    pool: PgPool,
    retention: std::time::Duration,
    cancel: CancellationToken,
) {
    let mut interval = tokio::time::interval(IDEMPOTENCY_CLEANUP_INTERVAL);
    // Pace the next tick a full period after a runtime pause instead of
    // bursting catch-up DELETEs. Same rationale as the health-check loop.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Skip the immediate first tick — startup already issued the same DELETE.
    interval.tick().await;
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = interval.tick() => {}
        }
        if pool.is_closed() {
            info!(
                task = "idempotency_cleanup",
                "pool closed; exiting cleanup loop"
            );
            break;
        }
        match cleanup_idempotency_keys(&pool, retention).await {
            Ok(rows) => {
                debug!(rows_deleted = rows, "idempotency-key cleanup completed");
            }
            Err(e) => {
                error!(error = %e, "idempotency-key cleanup failed");
            }
        }
    }
}

/// Executes a liveness probe against the pool and emits the `storage_health_status` gauge.
///
/// Emits `1.0` (with `reason=healthy`) on success and `0.0` (with `reason=probe_failed`) on
/// transient probe failure. A separate `reason=pool_closed` reading is emitted by the loop
/// when the pool itself has been shut down.
///
/// Returns `(value, reason, error_detail)` so the caller decides log level: the
/// loop suppresses repeated `error!` lines during sustained outages (see
/// `run_health_check_loop`'s `last_reason` tracking), while
/// `health_check_for_tests` asserts on the `(value, reason)` contract directly.
/// `error_detail` carries the full source chain (top-level `Display` + nested
/// causes) so SQLSTATE survives into the log line when the caller does log.
async fn health_check(
    pool: &PgPool,
    gauge: &opentelemetry::metrics::Gauge<f64>,
) -> (f64, &'static str, Option<String>) {
    match sqlx::query("SELECT 1").execute(pool).await {
        Ok(_) => {
            gauge.record(1.0_f64, &[KeyValue::new("reason", "healthy")]);
            (1.0, "healthy", None)
        }
        Err(e) => {
            gauge.record(0.0_f64, &[KeyValue::new("reason", "probe_failed")]);
            // Walk the source chain so the SQLSTATE (carried by the
            // sqlx::Error::Database arm) and any further-nested cause survive
            // into the operator-facing log line — without this the operator
            // would only see the top-level Display ("pool timed out", "encode
            // error", …) and could not triage probe failures from logs alone.
            let chain = render_error_chain(&e);
            let detail = format!("{e}: {chain}");
            (0.0, "probe_failed", Some(detail))
        }
    }
}

/// Walks `std::error::Error::source()` for `err` and joins each link with `: `,
/// mirroring the formatting used by `domain::error::render_source_chain` so
/// log readers see the same shape across the plugin.
fn render_error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut rendered = err.to_string();
    let mut current = err.source();
    while let Some(src) = current {
        rendered.push_str(": ");
        rendered.push_str(&src.to_string());
        current = src.source();
    }
    rendered
}

/// Test-only wrapper around [`health_check`] that constructs its own gauge,
/// so integration tests can pin the `(value, reason)` contract emitted to
/// `storage_health_status` for healthy and probe-failed states without
/// touching the global OpenTelemetry exporter.
#[cfg(feature = "integration")]
pub async fn health_check_for_tests(pool: &PgPool) -> (f64, &'static str) {
    let meter = global::meter("timescaledb-usage-collector-plugin-test");
    let gauge = meter.f64_gauge("storage_health_status").build();
    if pool.is_closed() {
        gauge.record(0.0_f64, &[KeyValue::new("reason", "pool_closed")]);
        return (0.0, "pool_closed");
    }
    let (value, reason, _) = health_check(pool, &gauge).await;
    (value, reason)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "module_tests.rs"]
mod module_tests;
