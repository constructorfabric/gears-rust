#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverApi;
use axum::Router;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistryImpl;
use toolkit::client_hub::ClientHub;
use toolkit::contracts::{DatabaseCapability, RestApiCapability};
use toolkit::{Gear, GearCtx};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::{ConnectOpts, DBProvider, DbError};
use types_registry_sdk::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery, TypesRegistryClient,
};
use uuid::Uuid;

use super::{
    AbortOnDrop, BssRatingGear, GEAR_STOP_TIMEOUT, LEADER_JOIN_BUDGET, record_sweep,
    retry_with_backoff, run_leader_jobs, sweep_every,
};
use crate::authz::ServiceGrant;
use crate::domain::error::DomainError;
use crate::domain::exception::{ExceptionSweeper, SweepReport};
use crate::domain::ports::{Dependency, FailureCode, RatingMetricsPort, SweepOutcome, TenantGrant};
use crate::infra::storage::repo::exception_repo::OrmExceptionRepo;
use crate::test_providers::{FakeClusterLeaders, FakePdp, MetricSample, RecordingMetrics};
use crate::test_support::LeadersWiring;

#[test]
fn the_migrations_create_the_foundation_tables_then_the_outbox() {
    let names: Vec<String> = BssRatingGear::default()
        .migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    assert_eq!(
        names.first().map(String::as_str),
        Some("m20261019_000001_create_rating_foundation")
    );
    assert!(
        names.iter().any(|n| n.contains("bss_rating__outbox")),
        "{names:?}"
    );
    assert!(!names.iter().any(|n| n.starts_with("invalid_")));
}

#[tokio::test]
async fn the_migrations_apply_on_sqlite() {
    let db = crate::test_support::migrated_db().await;
    let conn = db.conn().unwrap();
    let scope = toolkit_security::AccessScope::for_tenant(Uuid::from_u128(1));
    assert!(
        crate::infra::storage::repo::audit_repo::list_for_target(&conn, &scope, "k", "i")
            .await
            .unwrap()
            .is_empty()
    );
}

/// Records the authz label registration, or refuses it; every other registry call is
/// unexpected.
#[derive(Default)]
struct Registry {
    registered: Mutex<Vec<serde_json::Value>>,
    refuse: bool,
}

fn unexpected() -> CanonicalError {
    CanonicalError::internal("unexpected registry call").create()
}

#[async_trait]
impl TypesRegistryClient for Registry {
    async fn register(
        &self,
        entities: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        if self.refuse {
            return Ok(vec![RegisterResult::Err {
                gts_id: Some(crate::authz::labels::USAGE.to_owned()),
                error: CanonicalError::internal("schema refused").create(),
            }]);
        }
        self.registered.lock().unwrap().extend(entities);
        Ok(Vec::new())
    }
    async fn register_type_schemas(
        &self,
        _type_schemas: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        Err(unexpected())
    }
    async fn get_type_schema(&self, _type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        Err(unexpected())
    }
    async fn get_type_schema_by_uuid(
        &self,
        _type_uuid: Uuid,
    ) -> Result<GtsTypeSchema, CanonicalError> {
        Err(unexpected())
    }
    async fn get_type_schemas(
        &self,
        _type_ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        HashMap::new()
    }
    async fn get_type_schemas_by_uuid(
        &self,
        _type_uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        HashMap::new()
    }
    async fn list_type_schemas(
        &self,
        _query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        Err(unexpected())
    }
    async fn register_instances(
        &self,
        _instances: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        Err(unexpected())
    }
    async fn get_instance(&self, _id: &str) -> Result<GtsInstance, CanonicalError> {
        Err(unexpected())
    }
    async fn get_instance_by_uuid(&self, _uuid: Uuid) -> Result<GtsInstance, CanonicalError> {
        Err(unexpected())
    }
    async fn get_instances(
        &self,
        _ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, CanonicalError>> {
        HashMap::new()
    }
    async fn get_instances_by_uuid(
        &self,
        _uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        HashMap::new()
    }
    async fn list_instances(
        &self,
        _query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        Err(unexpected())
    }
}

struct Config(serde_json::Value);

/// No configuration for any gear.
struct Nothing;

impl toolkit::config::ConfigProvider for Nothing {
    fn get_gear_config(&self, _name: &str) -> Option<&serde_json::Value> {
        None
    }
}

impl toolkit::config::ConfigProvider for Config {
    fn get_gear_config(&self, name: &str) -> Option<&serde_json::Value> {
        (name == "bss-rating").then_some(&self.0)
    }
}

/// A gear context over a file-backed SQLite database (the outbox needs more than one
/// connection), the fake PDP and the recording registry.
struct Boot {
    ctx: GearCtx,
    hub: Arc<ClientHub>,
    registry: Arc<Registry>,
    _dir: tempfile::TempDir,
}

async fn boot(config: serde_json::Value) -> Boot {
    boot_with(config, Arc::new(Registry::default())).await
}

async fn boot_with(config: serde_json::Value, registry: Arc<Registry>) -> Boot {
    let dir = tempfile::Builder::new()
        .prefix("bss-rating-gear-")
        .tempdir()
        .unwrap();
    let dsn = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("gear.sqlite3").display()
    );
    let db = toolkit_db::connect_db(
        &dsn,
        ConnectOpts {
            max_conns: Some(8),
            min_conns: Some(1),
            ..ConnectOpts::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        BssRatingGear::default().migrations(),
    )
    .await
    .unwrap();
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn AuthZResolverApi>(FakePdp::new());
    hub.register::<dyn TypesRegistryClient>(Arc::clone(&registry) as Arc<dyn TypesRegistryClient>);
    let ctx = GearCtx::new(
        "bss-rating",
        Uuid::new_v4(),
        Arc::new(Config(serde_json::json!({ "config": config }))),
        Arc::clone(&hub),
        CancellationToken::new(),
    )
    .with_db(DBProvider::<DbError>::new(db));
    Boot {
        ctx,
        hub,
        registry,
        _dir: dir,
    }
}

#[tokio::test]
async fn init_registers_every_authz_label_and_the_release_route() {
    let boot = boot(serde_json::json!({})).await;
    let gear = BssRatingGear::default();
    gear.init(&boot.ctx).await.unwrap();
    let registered = { boot.registry.registered.lock().unwrap().clone() };
    assert_eq!(registered, crate::authz::authz_label_type_schemas());
    let openapi = OpenApiRegistryImpl::new();
    let _router = gear
        .register_rest(&boot.ctx, Router::new(), &openapi)
        .unwrap();
    let routes: Vec<String> = openapi
        .operation_specs
        .iter()
        .map(|e| e.key().clone())
        .collect();
    assert_eq!(
        routes,
        vec!["POST:/bss-rating/v1/inbox/{id}/release".to_owned()]
    );
}

#[tokio::test]
async fn an_event_source_enabled_before_its_contract_is_refused_at_init() {
    let boot = boot(serde_json::json!({
        "event_intake": {"sources": {"fact": {"enabled": true, "producer": "bss-subscriptions"}}}
    }))
    .await;
    let err = BssRatingGear::default().init(&boot.ctx).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("no event contract is adopted"),
        "{err:#}"
    );
}

#[tokio::test]
async fn an_election_timing_the_cluster_sdk_refuses_fails_init() {
    let boot = boot(serde_json::json!({"leadership": {"max_missed_renewals": 0}})).await;
    let err = BssRatingGear::default().init(&boot.ctx).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("max_missed_renewals"),
        "{err:#}"
    );
}

/// Waits until `metrics` holds a sample `wanted` accepts.
async fn sample(metrics: &RecordingMetrics, wanted: impl Fn(&MetricSample) -> bool) {
    for _ in 0..500 {
        if metrics.samples().iter().any(&wanted) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no matching sample in {:?}", metrics.samples());
}

#[tokio::test]
async fn serve_runs_the_queues_and_the_exception_sweeper_leadership_until_stopped() {
    let boot = boot(serde_json::json!({})).await;
    let leaders = FakeClusterLeaders::new();
    let wiring = LeadersWiring::bind(&leaders, &boot.hub);
    let metrics = Arc::new(RecordingMetrics::default());
    let gear = Arc::new(BssRatingGear {
        metrics: Some(Arc::clone(&metrics) as Arc<dyn RatingMetricsPort>),
        ..BssRatingGear::default()
    });
    gear.init(&boot.ctx).await.unwrap();
    let stop = CancellationToken::new();
    let serving = tokio::spawn(Arc::clone(&gear).serve(stop.clone()));
    leaders
        .wait_for_candidates("bss-rating/exception-sweep", 1)
        .await;
    leaders.grant("bss-rating/exception-sweep", 0).await;
    // Elected, the instance sweeps at once: the sweep is counted before the gear stops.
    sample(&metrics, |s| {
        matches!(s, MetricSample::ExceptionSweep(SweepOutcome::Completed, _))
    })
    .await;
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(30), serving)
        .await
        .expect("serve stops when cancelled")
        .unwrap()
        .unwrap();
    wiring.stop().await;
}

#[tokio::test]
async fn with_leadership_disabled_no_leader_job_runs() {
    let boot = boot(serde_json::json!({"leadership": {"enabled": false}})).await;
    let leaders = FakeClusterLeaders::new();
    let wiring = LeadersWiring::bind(&leaders, &boot.hub);
    let gear = BssRatingGear::default();
    gear.init(&boot.ctx).await.unwrap();
    let runtime = gear.runtime.get().cloned().unwrap();
    assert!(runtime.leader_timing.is_none());
    // The leader jobs end on their own, with `stop` never fired; the timeout only keeps a
    // regression from hanging the job.
    tokio::time::timeout(
        Duration::from_secs(5),
        run_leader_jobs(runtime, CancellationToken::new()),
    )
    .await
    .expect("with leadership disabled the leader jobs return at once");
    assert_eq!(
        leaders.candidate_count("bss-rating/exception-sweep").await,
        0,
        "no instance joins the election"
    );
    wiring.stop().await;
}

#[tokio::test(start_paused = true)]
async fn leader_jobs_that_outlive_their_join_budget_are_aborted() {
    let (dropped, mut aborted) = tokio::sync::oneshot::channel::<()>();
    let stuck = AbortOnDrop(tokio::spawn(async move {
        // Held until the task is dropped by its abort.
        let _dropped = dropped;
        std::future::pending::<()>().await;
    }));
    let started = tokio::time::Instant::now();
    stuck.join_within(LEADER_JOIN_BUDGET).await.unwrap();
    assert_eq!(started.elapsed(), LEADER_JOIN_BUDGET);
    // The abort dropped the task, and with it the sender.
    assert!(
        tokio::time::timeout(Duration::from_secs(1), &mut aborted)
            .await
            .unwrap()
            .is_err()
    );
}

#[test]
fn the_leader_join_budget_fits_inside_the_gears_stop_budget() {
    assert!(LEADER_JOIN_BUDGET < GEAR_STOP_TIMEOUT);
    assert_eq!(BssRatingGear::default().stop_timeout(), GEAR_STOP_TIMEOUT);
}

#[tokio::test]
async fn a_refused_label_registration_fails_init() {
    let registry = Arc::new(Registry {
        refuse: true,
        ..Registry::default()
    });
    let boot = boot_with(serde_json::json!({}), registry).await;
    let err = BssRatingGear::default().init(&boot.ctx).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("failed to register authz label"),
        "{err:#}"
    );
}

#[tokio::test]
async fn the_sweep_loop_sweeps_at_once_then_every_interval_and_counts_each_sweep() {
    // The database first: its pool times out under a paused clock.
    let db = crate::test_support::migrated_db().await;
    tokio::time::pause();
    let metrics = RecordingMetrics::default();
    let grant: Arc<dyn TenantGrant> = Arc::new(ServiceGrant::new(FakePdp::new().enforcer()));
    // No retrier: each sweep is complete without reading a row.
    let sweeper = ExceptionSweeper::new(Vec::new(), grant, Arc::new(OrmExceptionRepo));
    let term = CancellationToken::new();
    let interval = Duration::from_mins(15);
    let looping = sweep_every(&sweeper, &db, &metrics, interval, &term);
    let stopper = async {
        tokio::time::sleep(interval * 2 + Duration::from_secs(1)).await;
        term.cancel();
    };
    tokio::join!(looping, stopper);
    let sweeps: Vec<MetricSample> = metrics.samples();
    assert_eq!(
        sweeps,
        vec![MetricSample::ExceptionSweep(SweepOutcome::Completed, SweepReport::default()); 3]
    );
}

#[test]
fn a_failed_sweep_is_counted_against_the_dependency_that_failed() {
    let metrics = RecordingMetrics::default();
    let pdp_down = Err(DomainError::AuthzUnavailable(
        "PDP evaluation failed".to_owned(),
        None,
    ));
    record_sweep(&metrics, &pdp_down, Duration::from_secs(1), false);
    let storage_down = Err(DomainError::Database("page read".to_owned(), None));
    record_sweep(&metrics, &storage_down, Duration::from_secs(1), false);
    // A corrupt row is the stored data's fault, not a dependency's: no dependency sample.
    let corrupt = Err(DomainError::CorruptRow(
        "bss_rating__exception.subject_ref".to_owned(),
    ));
    record_sweep(&metrics, &corrupt, Duration::from_secs(1), false);
    let report = SweepReport {
        tenants: 1,
        retried: 2,
        failed: 0,
    };
    record_sweep(&metrics, &Ok(report), Duration::from_secs(1), true);
    assert_eq!(
        metrics.samples(),
        vec![
            MetricSample::ExceptionSweep(SweepOutcome::Failed, SweepReport::default()),
            MetricSample::DependencyError(Dependency::Pdp, FailureCode::EvaluationFailed),
            MetricSample::ExceptionSweep(SweepOutcome::Failed, SweepReport::default()),
            MetricSample::DependencyError(Dependency::Database, FailureCode::StorageFailed),
            MetricSample::ExceptionSweep(SweepOutcome::Failed, SweepReport::default()),
            MetricSample::ExceptionSweep(SweepOutcome::Cancelled, report),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn the_retry_pause_doubles_with_jitter_up_to_its_cap_and_starts_over_after_a_long_attempt() {
    let metrics = RecordingMetrics::default();
    let stop = CancellationToken::new();
    let cap = Duration::from_secs(8);
    let started: Mutex<Vec<tokio::time::Instant>> = Mutex::new(Vec::new());
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let attempt = || async {
        let n = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        started.lock().unwrap().push(tokio::time::Instant::now());
        // The seventh attempt holds up longer than the cap before it fails.
        if n == 6 {
            tokio::time::sleep(cap + Duration::from_secs(1)).await;
        }
        if n == 9 {
            stop.cancel();
            return Ok(());
        }
        Err(DomainError::Coordination("no backend".to_owned(), None))
    };
    retry_with_backoff(&stop, cap, &metrics, attempt).await;
    let started = started.into_inner().unwrap();
    assert_eq!(started.len(), 10);
    let gaps: Vec<Duration> = started.windows(2).map(|w| w[1] - w[0]).collect();
    // Pauses 1, 2, 4, 8, 8, 8 s, each scaled into [0.5, 1]; the seventh attempt itself takes
    // 9 s; after it the pause starts over at 1 s.
    let expected = [1, 2, 4, 8, 8, 8];
    for (gap, full) in gaps.iter().zip(expected) {
        let full = Duration::from_secs(full);
        assert!(
            *gap >= full / 2 && *gap <= full,
            "{gap:?} outside [{:?}, {full:?}]",
            full / 2
        );
    }
    let after_long = gaps[6].checked_sub(cap + Duration::from_secs(1)).unwrap();
    assert!(after_long <= Duration::from_secs(1), "{after_long:?}");
    assert_eq!(
        metrics
            .samples()
            .iter()
            .filter(|s| **s
                == MetricSample::DependencyError(
                    Dependency::Cluster,
                    FailureCode::LeadershipUnavailable
                ))
            .count(),
        9
    );
}

#[tokio::test(start_paused = true)]
async fn an_attempt_that_ends_cleanly_before_stop_is_paused_after_too() {
    let metrics = RecordingMetrics::default();
    let stop = CancellationToken::new();
    let started: Mutex<Vec<tokio::time::Instant>> = Mutex::new(Vec::new());
    let attempt = || async {
        let mut started = started.lock().unwrap();
        started.push(tokio::time::Instant::now());
        if started.len() == 3 {
            stop.cancel();
        }
        Ok(())
    };
    retry_with_backoff(&stop, Duration::from_secs(8), &metrics, attempt).await;
    let started = started.into_inner().unwrap();
    assert_eq!(started.len(), 3);
    for gap in started.windows(2).map(|w| w[1] - w[0]) {
        assert!(gap >= Duration::from_millis(500), "{gap:?}: no hot loop");
    }
    assert!(
        metrics.samples().is_empty(),
        "a clean end is no dependency failure"
    );
}

#[tokio::test]
async fn an_unconfigured_gear_stays_inactive() {
    let hub = Arc::new(ClientHub::new());
    let ctx = GearCtx::new(
        "bss-rating",
        Uuid::new_v4(),
        Arc::new(Nothing),
        hub,
        CancellationToken::new(),
    );
    let gear = BssRatingGear::default();
    gear.init(&ctx).await.unwrap();
    let openapi = OpenApiRegistryImpl::new();
    let _router = gear.register_rest(&ctx, Router::new(), &openapi).unwrap();
    assert!(openapi.operation_specs.is_empty());
}
