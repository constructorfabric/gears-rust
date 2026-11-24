#![cfg(feature = "postgres")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::cast_precision_loss,
    clippy::print_stdout
)]
//! Opt-in `PostgreSQL` load test of the lease cycle (issue #5577).
//!
//! Each worker loops: idempotency pre-read, as the gear does before an
//! acquisition, then acquire, then commit or release (alternating). Three
//! layouts: every worker on one shared Quota; one Quota per worker under one
//! tenant and metric (one shared capacity row); one tenant per worker. It
//! reports per-operation throughput and latency, refusals, completed cycles,
//! and the database work per cycle from `pg_stat_database` and
//! `pg_stat_statements`. Acquire is not retried, as in the gear; settlement
//! retries contention refusals so no lease is left behind.
//!
//! A measurement tool, not a perf gate: it asserts only sanity. Requires
//! Docker. Run with
//! `cargo test -p cf-gears-quota-enforcement-storage-plugin --features postgres --test lease_load_pg -- --ignored --nocapture --test-threads 1`.
//!
//! Env: `LOAD_WORKERS` (16), `LOAD_CYCLES` (200 per worker),
//! `LOAD_CONTENTION_TIMEOUT_MS` (1000), `LOAD_LEASE_CAP` (8 per tenant),
//! `LOAD_POOL` (workers + 4 connections), `LOAD_HOLD_MS` (0, how long a
//! worker holds each lease before settling it), `LOAD_FSYNC` (`on`: a
//! disk-backed data directory with `fsync=on`; `off`: tmpfs and `fsync=off`).

mod common;

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use gts::GtsTypeId;
use quota_enforcement_sdk::engine::PolicySchemaSnapshot;
use quota_enforcement_sdk::testing::consumption_quota_draft;
use quota_enforcement_sdk::{
    ApplicableQuotas, AttributionDigest, EvaluatedMutation, IdempotencyScope,
    IdempotencySubjectKey, IdempotencyWrite, LeaseToken, MetricId, OperationType,
    PartialIdempotencyWrite, PayloadHash, PeriodType, PolicyDraft, PolicyScope, StorageError,
    SubjectRef, TenantId,
};
use sea_orm::ActiveValue::Set;
use sea_orm_migration::MigratorTrait;
use testcontainers::core::ExecCommand;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::outbox::OutboxHandle;
use toolkit_db::{ConnectOpts, connect_db};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use quota_enforcement_storage_plugin::domain::ports::LeaseStore;
use quota_enforcement_storage_plugin::infra::storage::Migrator;
use quota_enforcement_storage_plugin::infra::storage::config_cache::{
    ConfigCache, DEFAULT_REFRESH,
};
use quota_enforcement_storage_plugin::infra::storage::entity::{
    contention_timeout_config, lease_capacity_config,
};
use quota_enforcement_storage_plugin::{
    Actor, ConsumptionStore, NotificationOutbox, QeOutbox, QuotaStore, SqlConsumptionStore,
    SqlPolicyStore, SqlQuotaStore, start_undelivered_outbox,
};

const USER_PROJECTION: &str = "gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~";
const METRIC_TOKENS: &str = "gts.cf.core.qe.metric_type.v1~cf.genai.llm_gateway.ai_tokens_input.v1";
const RESERVED: u64 = 10;
const COMMITTED: u64 = 7;
/// Past `PostgreSQL`'s idle stats flush interval (10 s), so counters are complete.
const STATS_FLUSH: Duration = Duration::from_secs(11);
const SEP: &str = "\u{1f}";
const SETTLE_ATTEMPTS: usize = 100;

fn env_or<T: FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[derive(Clone, Copy)]
struct Config {
    workers: usize,
    cycles: usize,
    contention_timeout_ms: i64,
    lease_cap: i32,
    pool: u32,
    hold: Duration,
    fsync: bool,
}

impl Config {
    fn from_env() -> Self {
        let workers = env_or("LOAD_WORKERS", 16_usize);
        Self {
            workers,
            cycles: env_or("LOAD_CYCLES", 200),
            contention_timeout_ms: env_or("LOAD_CONTENTION_TIMEOUT_MS", 1000),
            lease_cap: env_or("LOAD_LEASE_CAP", 8),
            pool: env_or("LOAD_POOL", u32::try_from(workers).unwrap_or(u32::MAX) + 4),
            hold: Duration::from_millis(env_or("LOAD_HOLD_MS", 0)),
            fsync: env_or("LOAD_FSYNC", "on".to_owned()) != "off",
        }
    }
}

fn tenant(slot: usize) -> TenantId {
    TenantId::new(Uuid::from_u128(0x10ad_0000 + slot as u128))
}

fn ctx(tenant: TenantId) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x5eed))
        .subject_tenant_id(tenant.as_uuid())
        .build()
        .expect("security context")
}

fn subject(id: &str) -> SubjectRef {
    SubjectRef {
        projection_type: GtsTypeId::try_new(USER_PROJECTION).expect("type id"),
        subject_id: id.to_owned(),
    }
}

fn applicable(tenant: TenantId, subject_id: &str) -> ApplicableQuotas {
    ApplicableQuotas {
        tenant_id: tenant,
        subjects: vec![subject(subject_id)],
        metric: MetricId::parse(METRIC_TOKENS).expect("metric"),
    }
}

#[derive(Clone, Copy)]
enum Layout {
    /// Every worker on one Quota.
    SharedQuota,
    /// One Quota per worker, all under one tenant and metric.
    OneTenantDistinctQuotas,
    /// One tenant, and so one Quota, per worker.
    Independent,
}

impl Layout {
    fn label(self) -> &'static str {
        match self {
            Self::SharedQuota => "shared quota",
            Self::OneTenantDistinctQuotas => "one tenant+metric, distinct quotas",
            Self::Independent => "independent tenants",
        }
    }

    /// The tenant slot and subject of worker `id`.
    fn of(self, id: usize) -> (usize, String) {
        match self {
            Self::SharedQuota => (0, "load-user".to_owned()),
            Self::OneTenantDistinctQuotas => (0, format!("load-user-{id}")),
            Self::Independent => (id, "load-user".to_owned()),
        }
    }

    fn tenants(self, workers: usize) -> usize {
        match self {
            Self::Independent => workers,
            Self::SharedQuota | Self::OneTenantDistinctQuotas => 1,
        }
    }

    fn quotas(self, workers: usize) -> Vec<(usize, String)> {
        match self {
            Self::SharedQuota => vec![self.of(0)],
            Self::OneTenantDistinctQuotas | Self::Independent => {
                (0..workers).map(|id| self.of(id)).collect()
            }
        }
    }
}

struct Harness {
    db: toolkit_db::Db,
    store: Arc<SqlConsumptionStore>,
    quotas: SqlQuotaStore,
    outbox: OutboxHandle,
    container: ContainerAsync<Postgres>,
}

impl Harness {
    async fn up(config: Config) -> Self {
        // The helper mounts a tmpfs on /var/lib/postgresql, where fsync costs
        // nothing. An fsync-on run moves PGDATA off it, onto the container's disk.
        let request = if config.fsync {
            test_containers::postgres().with_env_var("PGDATA", "/var/lib/pgdata")
        } else {
            test_containers::postgres()
        };
        let fsync = if config.fsync {
            "fsync=on"
        } else {
            "fsync=off"
        };
        let container = request
            .with_env_var("POSTGRES_PASSWORD", "pass")
            .with_env_var("POSTGRES_USER", "user")
            .with_env_var("POSTGRES_DB", "app")
            .with_cmd([
                "-c",
                fsync,
                "-c",
                "shared_preload_libraries=pg_stat_statements",
                "-c",
                "max_connections=200",
            ])
            .start()
            .await
            .expect("start postgres");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("mapped port");
        let dsn = format!("postgres://user:pass@127.0.0.1:{port}/app");
        let opts = ConnectOpts {
            max_conns: Some(config.pool),
            ..ConnectOpts::default()
        };
        let mut db = None;
        for _ in 0..40 {
            if let Ok(connected) = connect_db(&dsn, opts.clone()).await {
                db = Some(connected);
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let db = db.expect("connect to postgres");
        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .expect("migrations");
        let outbox = start_undelivered_outbox(db.clone()).await.expect("outbox");
        let bound = Arc::new(QeOutbox::new());
        bound.bind(Arc::clone(outbox.outbox())).expect("bind once");
        let enqueuer: Arc<dyn NotificationOutbox> = bound;
        // One configuration cache for both stores, as the gear wires them.
        let config_cache = Arc::new(ConfigCache::new(DEFAULT_REFRESH));
        let harness = Self {
            db: db.clone(),
            store: Arc::new(
                SqlConsumptionStore::new(db.clone(), Arc::clone(&enqueuer))
                    .with_config(Arc::clone(&config_cache)),
            ),
            quotas: SqlQuotaStore::new(db, Arc::clone(&enqueuer)).with_config(config_cache),
            outbox,
            container,
        };
        harness.seed_policy(enqueuer).await;
        harness
            .seed_contention_budget(config.contention_timeout_ms)
            .await;
        harness
            .psql("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
            .await;
        harness
    }

    async fn seed_policy(&self, enqueuer: Arc<dyn NotificationOutbox>) {
        SqlPolicyStore::new(self.db.clone(), enqueuer)
            .create_policy(
                &ctx(tenant(0)),
                PolicyDraft {
                    scope: PolicyScope::Global,
                    engine_id: "cel".to_owned(),
                    engine_config: serde_json::json!({}),
                    timeout_ms: Some(200),
                    description: None,
                    comment: None,
                    created_by: "load".to_owned(),
                    schema_snapshot: PolicySchemaSnapshot::default(),
                },
                &[],
            )
            .await
            .expect("seed the global policy");
    }

    async fn seed_contention_budget(&self, timeout_ms: i64) {
        let conn = self.db.conn().expect("conn");
        toolkit_db::secure::secure_insert::<contention_timeout_config::Entity>(
            contention_timeout_config::ActiveModel {
                metric_key: Set(METRIC_TOKENS.to_owned()),
                timeout_ms: Set(timeout_ms),
                updated_at: Set(OffsetDateTime::now_utc()),
            },
            &AccessScope::allow_all(),
            &conn,
        )
        .await
        .expect("configure the contention budget");
    }

    /// One consumption Quota of `subject_id` under the tenant of `slot`.
    async fn seed_quota(&self, slot: usize, subject_id: &str) {
        let tenant = tenant(slot);
        let mut draft = consumption_quota_draft(subject(subject_id), None, PeriodType::Day, vec![]);
        draft.tenant_id = tenant;
        draft.metric = MetricId::parse(METRIC_TOKENS).expect("metric");
        let actor = Actor {
            subject_id: Uuid::from_u128(0x5eed),
            subject_type: None,
        };
        self.quotas
            .create_quota(
                &actor,
                &AccessScope::for_tenant(tenant.as_uuid()),
                draft,
                &[],
            )
            .await
            .expect("quota");
    }

    /// The active-lease cap of the tenant of `slot` (I7).
    async fn seed_cap(&self, slot: usize, cap: i32) {
        let tenant = tenant(slot);
        let conn = self.db.conn().expect("conn");
        toolkit_db::secure::secure_insert::<lease_capacity_config::Entity>(
            lease_capacity_config::ActiveModel {
                tenant_key: Set(tenant.as_uuid().to_string()),
                metric_key: Set(METRIC_TOKENS.to_owned()),
                max_active_leases: Set(cap),
                updated_at: Set(OffsetDateTime::now_utc()),
            },
            &AccessScope::allow_all(),
            &conn,
        )
        .await
        .expect("configure the lease cap");
    }

    async fn consumed(&self, slot: usize, subject_id: &str) -> u64 {
        let tenant = tenant(slot);
        self.store
            .read_quota_snapshot(
                &ctx(tenant),
                &AccessScope::for_tenant(tenant.as_uuid()),
                &applicable(tenant, subject_id),
            )
            .await
            .expect("snapshot")
            .iter()
            .map(|snapshot| snapshot.consumed)
            .sum()
    }

    /// Run `sql` through `psql` in the container; rows are lines, fields split by [`SEP`].
    async fn psql(&self, sql: &str) -> String {
        let args = [
            "psql",
            "-v",
            "ON_ERROR_STOP=1",
            "-U",
            "user",
            "-d",
            "app",
            "-At",
            "-F",
            SEP,
            "-c",
            sql,
        ];
        let mut output = self
            .container
            .exec(ExecCommand::new(args.map(str::to_owned)))
            .await
            .expect("psql runs in the container");
        let stdout = output.stdout_to_vec().await.expect("psql output");
        String::from_utf8_lossy(&stdout).into_owned()
    }

    async fn db_counters(&self) -> DbCounters {
        let line = self
            .psql(
                "SELECT xact_commit, xact_rollback, tup_returned, tup_fetched, tup_inserted, \
                 tup_updated, tup_deleted FROM pg_stat_database WHERE datname = 'app'",
            )
            .await;
        let values: Vec<f64> = line
            .trim()
            .split(SEP)
            .map(|field| field.parse().expect("counter"))
            .collect();
        DbCounters(values.try_into().expect("seven counters"))
    }

    async fn statement_calls(&self) -> f64 {
        self.psql(
            "SELECT coalesce(sum(calls), 0) FROM pg_stat_statements \
             WHERE dbid = (SELECT oid FROM pg_database WHERE datname = 'app')",
        )
        .await
        .trim()
        .parse()
        .expect("calls")
    }

    /// Calls, mean execution ms and query text of the most called statements.
    async fn top_statements(&self) -> Vec<(f64, f64, String)> {
        self.psql(
            "SELECT calls, mean_exec_time, left(regexp_replace(query, '\\s+', ' ', 'g'), 110) \
             FROM pg_stat_statements \
             WHERE dbid = (SELECT oid FROM pg_database WHERE datname = 'app') \
             ORDER BY calls DESC LIMIT 25",
        )
        .await
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, SEP);
            Some((
                fields.next()?.parse().expect("calls"),
                fields.next()?.parse().expect("mean"),
                fields.next()?.to_owned(),
            ))
        })
        .collect()
    }

    async fn active_leases(&self) -> u64 {
        self.psql("SELECT count(*) FROM qe_leases WHERE state = 'active'")
            .await
            .trim()
            .parse()
            .expect("count")
    }
}

const COUNTER_NAMES: [&str; 7] = [
    "xact_commit",
    "xact_rollback",
    "tup_returned",
    "tup_fetched",
    "tup_inserted",
    "tup_updated",
    "tup_deleted",
];

#[derive(Clone, Copy)]
struct DbCounters([f64; 7]);

impl DbCounters {
    fn minus(self, other: Self) -> Self {
        Self(std::array::from_fn(|i| self.0[i] - other.0[i]))
    }

    fn scaled(self, by: f64) -> Self {
        Self(self.0.map(|value| value * by))
    }
}

#[derive(Default)]
struct OpStats {
    ok: Vec<Duration>,
    contention: u64,
    cap: u64,
    denied: u64,
    other: BTreeMap<String, u64>,
}

impl OpStats {
    fn record(&mut self, started: Instant, outcome: Result<(), &StorageError>) {
        match outcome {
            Ok(()) => self.ok.push(started.elapsed()),
            Err(StorageError::LeaseContentionTimeout) => self.contention += 1,
            Err(StorageError::LeaseInflightLimitExceeded) => self.cap += 1,
            Err(error) => *self.other.entry(error.to_string()).or_default() += 1,
        }
    }

    fn merge(&mut self, other: Self) {
        self.ok.extend(other.ok);
        self.contention += other.contention;
        self.cap += other.cap;
        self.denied += other.denied;
        for (kind, count) in other.other {
            *self.other.entry(kind).or_default() += count;
        }
    }
}

#[derive(Default)]
struct WorkerReport {
    lookup: OpStats,
    acquire: OpStats,
    commit: OpStats,
    release: OpStats,
    committed: u64,
    completed: u64,
    settle_retries: u64,
    unsettled: u64,
}

impl WorkerReport {
    fn merge(&mut self, other: Self) {
        self.lookup.merge(other.lookup);
        self.acquire.merge(other.acquire);
        self.commit.merge(other.commit);
        self.release.merge(other.release);
        self.committed += other.committed;
        self.completed += other.completed;
        self.settle_retries += other.settle_retries;
        self.unsettled += other.unsettled;
    }
}

/// Live leases per tenant as the workers see them: a lower bound of the database's count,
/// near empty at `LOAD_HOLD_MS=0`; `concurrent_acquisitions_admit_exactly_the_lease_cap` proves the cap.
struct Gauge {
    live: AtomicI64,
    max: AtomicI64,
}

impl Gauge {
    fn up(&self) {
        let now = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(now, Ordering::SeqCst);
    }

    fn down(&self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Worker {
    id: usize,
    tenant: TenantId,
    subject: String,
    cycles: usize,
    hold: Duration,
    store: Arc<SqlConsumptionStore>,
    gauge: Arc<Gauge>,
}

impl Worker {
    async fn run(self) -> WorkerReport {
        let mut report = WorkerReport::default();
        let ctx = ctx(self.tenant);
        let scope = AccessScope::for_tenant(self.tenant.as_uuid());
        let applicable = applicable(self.tenant, &self.subject);
        let null = serde_json::Value::Null;
        for cycle in 0..self.cycles {
            let write = IdempotencyWrite {
                scope: IdempotencyScope {
                    tenant_id: self.tenant,
                    subject_key: IdempotencySubjectKey::of(&applicable.subjects),
                    operation_type: OperationType::Reserve,
                    key: format!("w{}-c{cycle}", self.id),
                },
                payload_hash: PayloadHash::from_bytes([1; 32]),
            };
            let started = Instant::now();
            let replay = self.store.lookup_idempotency(&write.scope).await;
            report.lookup.record(started, replay.as_ref().map(|_| ()));
            assert!(matches!(replay, Ok(None)), "a fresh key replayed");
            let started = Instant::now();
            let acquired = self
                .store
                .acquire_lease(
                    &ctx,
                    &scope,
                    &EvaluatedMutation {
                        applicable: &applicable,
                        amount: RESERVED,
                        request: &null,
                        resource: &null,
                        user_projection: None,
                        limits: common::limits(),
                        idempotency: &write,
                        authorized: AttributionDigest::from_bytes([7; 32]),
                        evaluate: common::allow_all(),
                    },
                    Duration::from_mins(5),
                )
                .await;
            report
                .acquire
                .record(started, acquired.as_ref().map(|_| ()));
            let token = match acquired {
                Ok(outcome) => outcome.get().token,
                Err(_) => continue,
            };
            let Some(token) = token else {
                report.acquire.denied += 1;
                continue;
            };
            self.gauge.up();
            if !self.hold.is_zero() {
                tokio::time::sleep(self.hold).await;
            }
            self.gauge.down();
            self.settle(&mut report, &ctx, &scope, token, cycle).await;
        }
        report
    }

    /// Commit on even cycles, release on odd; a contention refusal retries,
    /// up to [`SETTLE_ATTEMPTS`] in all.
    async fn settle(
        &self,
        report: &mut WorkerReport,
        ctx: &SecurityContext,
        scope: &AccessScope,
        token: LeaseToken,
        cycle: usize,
    ) {
        let commit = cycle.is_multiple_of(2);
        let write = PartialIdempotencyWrite {
            tenant_id: self.tenant,
            key: format!("s-w{}-c{cycle}", self.id),
            payload_hash: PayloadHash::from_bytes([9; 32]),
        };
        for attempt in 1..=SETTLE_ATTEMPTS {
            let started = Instant::now();
            let settled = if commit {
                self.store
                    .commit_lease(ctx, scope, token, Some(COMMITTED), &write, &[])
                    .await
            } else {
                self.store
                    .release_lease(ctx, scope, token, &write, &[])
                    .await
            };
            let stats = if commit {
                &mut report.commit
            } else {
                &mut report.release
            };
            stats.record(started, settled.as_ref().map(|_| ()));
            match settled {
                Ok(_) => {
                    report.committed += u64::from(commit);
                    report.completed += 1;
                    return;
                }
                Err(StorageError::LeaseContentionTimeout) if attempt < SETTLE_ATTEMPTS => {
                    report.settle_retries += 1;
                }
                Err(_) => break,
            }
        }
        report.unsettled += 1;
    }
}

fn percentile(sorted: &[Duration], p: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = (sorted.len() * p).div_ceil(100).saturating_sub(1);
    sorted[index.min(sorted.len() - 1)].as_secs_f64() * 1000.0
}

fn print_op(name: &str, stats: &mut OpStats, wall: Duration) {
    stats.ok.sort_unstable();
    let other: u64 = stats.other.values().sum();
    println!(
        "| {name:<8} | {:>7} | {:>8.0} | {:>7.2} | {:>7.2} | {:>7.2} | {:>10} | {:>9} | {:>6} | {:>5} |",
        stats.ok.len(),
        stats.ok.len() as f64 / wall.as_secs_f64(),
        percentile(&stats.ok, 50),
        percentile(&stats.ok, 95),
        percentile(&stats.ok, 99),
        stats.contention,
        stats.cap,
        stats.denied,
        other,
    );
}

fn print_report(layout: Layout, config: Config, report: &mut WorkerReport, wall: Duration) {
    let secs = wall.as_secs_f64();
    println!(
        "\n## {}: {} workers x {} cycles, {} tenant(s), {} quota(s), contention budget {} ms, \
         lease cap {}, hold {} ms, pool {}, fsync {}",
        layout.label(),
        config.workers,
        config.cycles,
        layout.tenants(config.workers),
        layout.quotas(config.workers).len(),
        config.contention_timeout_ms,
        config.lease_cap,
        config.hold.as_millis(),
        config.pool,
        if config.fsync {
            "on (disk)"
        } else {
            "off (tmpfs)"
        },
    );
    let acquire = &report.acquire;
    let acquire_other: u64 = acquire.other.values().sum();
    let failed = acquire.contention + acquire.cap + acquire.denied + acquire_other;
    println!(
        "cycles: attempted {}, completed {} ({:.0}/s over {secs:.2} s); failed at acquire {failed} \
         (contention {}, cap {}, denied {}, other {acquire_other}; acquire is not retried); \
         settle retries {}, settle failures {}",
        config.workers * config.cycles,
        report.completed,
        report.completed as f64 / secs,
        acquire.contention,
        acquire.cap,
        acquire.denied,
        report.settle_retries,
        report.unsettled,
    );
    println!(
        "| op       |      ok |    ok/s  | p50 ms  | p95 ms  | p99 ms  | contention | cap (I7)  | denied | other |"
    );
    println!(
        "|----------|---------|----------|---------|---------|---------|------------|-----------|--------|-------|"
    );
    print_op("lookup", &mut report.lookup, wall);
    print_op("acquire", &mut report.acquire, wall);
    print_op("commit", &mut report.commit, wall);
    print_op("release", &mut report.release, wall);
    let ops = [
        &report.lookup,
        &report.acquire,
        &report.commit,
        &report.release,
    ];
    for (kind, count) in ops.iter().flat_map(|stats| &stats.other) {
        println!("other error: {count} x {kind}");
    }
}

fn print_db(
    per_cycle: DbCounters,
    idle_per_sec: DbCounters,
    statements: f64,
    idle_statements: f64,
) {
    println!(
        "\nDB work per completed cycle (pg_stat_database, net of idle background, failed attempts included):"
    );
    for (name, value) in COUNTER_NAMES.iter().zip(per_cycle.0) {
        println!("  {name:<13} {value:>8.2}");
    }
    let idle: Vec<String> = COUNTER_NAMES
        .iter()
        .zip(idle_per_sec.0)
        .map(|(name, value)| format!("{name}={value:.1}"))
        .collect();
    println!("  idle background per second: {}", idle.join(" "));
    println!(
        "  statements per completed cycle (pg_stat_statements): {statements:.2} (idle background {idle_statements:.1}/s)"
    );
}

/// Background cost of an idle database (outbox, autovacuum), per second.
async fn measure_idle(h: &Harness) -> (DbCounters, DbCounters, f64) {
    tokio::time::sleep(STATS_FLUSH).await;
    h.psql("SELECT pg_stat_statements_reset()").await;
    let start = h.db_counters().await;
    let clock = Instant::now();
    tokio::time::sleep(STATS_FLUSH).await;
    let end = h.db_counters().await;
    let secs = clock.elapsed().as_secs_f64();
    let statements = h.statement_calls().await / secs;
    (end, end.minus(start).scaled(1.0 / secs), statements)
}

async fn drive(h: &Harness, config: Config, layout: Layout, gauges: &[Arc<Gauge>]) -> WorkerReport {
    let handles: Vec<_> = (0..config.workers)
        .map(|id| {
            let (slot, subject) = layout.of(id);
            tokio::spawn(
                Worker {
                    id,
                    tenant: tenant(slot),
                    subject,
                    cycles: config.cycles,
                    hold: config.hold,
                    store: Arc::clone(&h.store),
                    gauge: Arc::clone(&gauges[slot]),
                }
                .run(),
            )
        })
        .collect();
    let mut report = WorkerReport::default();
    for handle in handles {
        report.merge(handle.await.expect("worker"));
    }
    report
}

/// Progress, no unexpected error kinds or denials, the gauge under the cap, every lease settled, no drift.
async fn check_sanity(
    h: &Harness,
    config: Config,
    layout: Layout,
    report: &WorkerReport,
    gauges: &[Arc<Gauge>],
) {
    let ops = [
        &report.lookup,
        &report.acquire,
        &report.commit,
        &report.release,
    ];
    assert!(
        ops.iter().all(|stats| stats.other.is_empty()),
        "unexpected error kinds"
    );
    assert!(report.completed > 0, "no lease cycle completed");
    assert_eq!(report.acquire.denied, 0, "the allow-all policy denied");
    assert_eq!(report.unsettled, 0, "a lease never settled");
    for (slot, gauge) in gauges.iter().enumerate() {
        let max = gauge.max.load(Ordering::SeqCst);
        assert!(
            max <= i64::from(config.lease_cap),
            "slot {slot}: the gauge saw {max} live leases"
        );
    }
    assert_eq!(h.active_leases().await, 0, "leases left active");
    let mut consumed = 0;
    for (slot, subject_id) in layout.quotas(config.workers) {
        consumed += h.consumed(slot, &subject_id).await;
    }
    assert_eq!(
        consumed,
        report.committed * COMMITTED,
        "consumption drifted"
    );
}

async fn run(layout: Layout) {
    let config = Config::from_env();
    let h = Harness::up(config).await;
    for (slot, subject_id) in layout.quotas(config.workers) {
        h.seed_quota(slot, &subject_id).await;
    }
    let tenants = layout.tenants(config.workers);
    for slot in 0..tenants {
        h.seed_cap(slot, config.lease_cap).await;
    }
    let gauges: Vec<Arc<Gauge>> = (0..tenants)
        .map(|_| {
            Arc::new(Gauge {
                live: AtomicI64::new(0),
                max: AtomicI64::new(0),
            })
        })
        .collect();
    let (baseline, idle_per_sec, idle_statements) = measure_idle(&h).await;

    h.psql("SELECT pg_stat_statements_reset()").await;
    let window_clock = Instant::now();
    let mut report = drive(&h, config, layout, &gauges).await;
    let wall = window_clock.elapsed();
    let statements = h.statement_calls().await;
    let top = h.top_statements().await;
    tokio::time::sleep(STATS_FLUSH).await;
    let run_counters = h.db_counters().await.minus(baseline);
    let window = window_clock.elapsed().as_secs_f64();

    print_report(layout, config, &mut report, wall);
    if report.completed == 0 {
        println!("\nNo cycle completed, so no per-cycle figures.");
    } else {
        let cycles = report.completed as f64;
        print_db(
            run_counters
                .minus(idle_per_sec.scaled(window))
                .scaled(1.0 / cycles),
            idle_per_sec,
            (statements - idle_statements * wall.as_secs_f64()) / cycles,
            idle_statements,
        );
        println!("\nTop statements (calls per completed cycle, mean execution ms):");
        for (calls, mean, query) in top {
            println!("  {:>6.2}  {mean:>7.3}  {query}", calls / cycles);
        }
    }
    check_sanity(&h, config, layout, &report, &gauges).await;
    h.outbox.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load test: run on demand with --ignored --nocapture"]
async fn lease_cycle_load_on_one_shared_quota() {
    run(Layout::SharedQuota).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load test: run on demand with --ignored --nocapture"]
async fn lease_cycle_load_on_distinct_quotas_of_one_tenant() {
    run(Layout::OneTenantDistinctQuotas).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load test: run on demand with --ignored --nocapture"]
async fn lease_cycle_load_on_independent_quotas() {
    run(Layout::Independent).await;
}
