#![cfg(feature = "postgres")]
// Shared across test binaries: not every binary uses every fixture, and these
// fixtures panic on invalid test input by design.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]
//! Shared `TimescaleDB` testcontainer harness. Requires Docker.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rust_decimal::Decimal;
use sqlx::PgPool;
use testcontainers::core::WaitFor;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use usage_collector_sdk::{
    IdempotencyKey, MetadataKey, ResourceRef, SubjectRef, UsageKind, UsageRecord, UsageType,
    UsageTypeGtsId,
};

use timescaledb_usage_collector_plugin::config::TimescaleDbPluginConfig;
use timescaledb_usage_collector_plugin::infra::metrics::Metrics;
use timescaledb_usage_collector_plugin::infra::storage::catalog_store::PgCatalogStore;
use timescaledb_usage_collector_plugin::infra::storage::pool::{
    MIGRATOR, apply_post_migration_setup, build_pool,
};
use timescaledb_usage_collector_plugin::infra::storage::record_store::PgRecordStore;

/// Monotonic seed so each raw record insert gets a distinct `id` and
/// `idempotency_key` (the workspace `uuid` crate has no `v4` feature, so we
/// mint deterministic-but-unique ids via [`Uuid::from_u128`]).
static RAW_RECORD_SEQ: AtomicU64 = AtomicU64::new(1);

pub struct TsHarness {
    pub pool: PgPool,
    _container: ContainerAsync<GenericImage>,
}

/// Retention window for harnesses whose fixtures are deliberately backdated to a
/// fixed instant (`fixture_usage_record`'s `created_at`, 2023-11-14).
///
/// `apply_post_migration_setup` registers a REAL `policy_retention` job, and the
/// image's background scheduler fires it on its own roughly 3 seconds after
/// registration — i.e. while the test body is still running. At the production
/// default window (365 days) the backdated fixtures are ~2.5 years past the
/// cutoff and share one 7-day chunk, so that first scheduled run drops the chunk
/// out from under the test. A stalled insert loop (parallel containers on a
/// loaded CI box) then sees rows vanish mid-test: one aggregation bucket short,
/// a `count` off by one, a cursor walk ending early.
///
/// At the config's documented ceiling (`MAX_RETENTION_SECS`, `src/config.rs` —
/// private, so the value is repeated here) the cutoff lands in 1926, no chunk is
/// ever drop-eligible, and the registered policy is a structural no-op: still
/// registered, still scheduled, still run — it just finds nothing to delete.
pub const NO_DROP_RETENTION_SECS: u64 = 100 * 365 * 86_400;

/// The production default (365 days). Only for tests whose subject IS retention.
pub const REAL_RETENTION_SECS: u64 = 365 * 86_400;

pub async fn bring_up() -> anyhow::Result<TsHarness> {
    // Default pool bounds and statement timeout (mirrors the config defaults),
    // plus a retention window that cannot reach the backdated fixtures.
    bring_up_with(30, 2, 16, NO_DROP_RETENTION_SECS).await
}

/// Like [`bring_up`] but with the production 365-day retention window, and with
/// the registered job's background schedule disabled.
///
/// For tests whose subject is retention itself: they insert genuinely aged rows
/// and fire the policy by hand (`CALL run_job`). Leaving the job scheduled would
/// let the background scheduler drop those rows first, both flaking the
/// "exists before retention runs" precondition and letting the assertion pass
/// for the wrong reason (background run did the work, the manual one was a
/// no-op). Unscheduling makes the explicit `run_job` the only deleter, so the
/// test observes exactly the policy it registered.
pub async fn bring_up_real_retention() -> anyhow::Result<TsHarness> {
    let h = bring_up_with(30, 2, 16, REAL_RETENTION_SECS).await?;
    // `alter_job(scheduled => false)` keeps the job row (so the schema test's
    // "a policy is registered" assertion still holds) and leaves `run_job`
    // working; it only stops the scheduler from firing it unprompted.
    let unscheduled = sqlx::query(
        "SELECT alter_job(job_id, scheduled => false) \
         FROM timescaledb_information.jobs \
         WHERE proc_name = 'policy_retention' AND hypertable_name = 'usage_records'",
    )
    .execute(&h.pool)
    .await?
    .rows_affected();
    // A `WHERE` that matches nothing is not an error, so an unschedule that
    // quietly hit zero rows would leave the job scheduled and re-arm the very
    // race this harness exists to remove — and the retention test would then
    // pass on the background run's work. Fail loudly instead, so a TimescaleDB
    // bump that reshapes `timescaledb_information.jobs` surfaces here.
    anyhow::ensure!(
        unscheduled == 1,
        "expected to unschedule exactly 1 policy_retention job on usage_records, \
         matched {unscheduled}"
    );
    Ok(h)
}

/// Like [`bring_up`] but with an explicit request-path `statement_timeout` (secs),
/// pool bounds, and retention window. Used to assert the init path does not leak
/// a modified `statement_timeout` onto pooled connections: pass a value distinct
/// from any the init path might set, and a small fixed pool so every connection
/// can be inspected.
///
/// `retention_secs` is threaded into the config JSON rather than left to
/// `#[serde(default)]`: the default is the production 365-day window, which arms
/// a live chunk-dropper against the backdated fixtures (see
/// [`NO_DROP_RETENTION_SECS`]).
pub async fn bring_up_with(
    statement_timeout_secs: u64,
    pool_size_min: u32,
    pool_size_max: u32,
    retention_secs: u64,
) -> anyhow::Result<TsHarness> {
    // The tag lives in `test_containers::TIMESCALEDB_TAG`; keep that constant
    // in sync with `TimescaleDbSidecar.IMAGE` in `testing/e2e/lib/sidecars.py`.
    // A skew means these migrations are validated against a different
    // PostgreSQL major than E2E runs.
    let image = test_containers::timescaledb()
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_DB", "app");
    let container = image.start().await?;
    let port = container.get_host_port_ipv4(5432).await?;

    // The test container serves no TLS; `sslmode=disable` is the deliberate
    // opt-out that `build_pool` honors (production DSNs without an explicit
    // sslmode are upgraded to `require` — see `connect_options`). Built by
    // deserialization because the secret-wrapped `database_url` has no public
    // literal constructor (the production path is always serde + expand-vars).
    let cfg: TimescaleDbPluginConfig = serde_json::from_str(&format!(
        r#"{{ "database_url": "postgres://user:pass@127.0.0.1:{port}/app?sslmode=disable",
              "statement_timeout_secs": {statement_timeout_secs},
              "pool_size_min": {pool_size_min}, "pool_size_max": {pool_size_max},
              "retention_period_secs": {retention_secs} }}"#
    ))
    .expect("valid test config json");

    let mut pool = None;
    let mut last = None;
    for _ in 0..20 {
        match build_pool(&cfg).await {
            Ok(p) => {
                pool = Some(p);
                break;
            }
            Err(e) => {
                last = Some(e);
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
    let pool = pool.ok_or_else(|| anyhow::anyhow!("pool connect failed: {last:?}"))?;

    MIGRATOR.run(&pool).await?;
    apply_post_migration_setup(&pool, cfg.retention_period_secs).await?;
    Ok(TsHarness {
        pool,
        _container: container,
    })
}

/// Build a fresh metric inventory over `pool`.
///
/// The stores now take an `Arc<Metrics>`; tests only need a live handle, not to
/// assert on it, so each call mints its own inventory against the global meter
/// provider (recording is a no-op without an exporter installed).
#[must_use]
pub fn metrics(pool: &PgPool) -> Arc<Metrics> {
    Arc::new(Metrics::new(pool.clone()))
}

/// Convenience builder for a [`PgRecordStore`] with its own metric handle.
#[must_use]
pub fn record_store(pool: &PgPool) -> PgRecordStore {
    PgRecordStore::new(pool.clone(), metrics(pool), CancellationToken::new())
}

/// Convenience builder for a [`PgCatalogStore`] with its own metric handle.
#[must_use]
pub fn catalog_store(pool: &PgPool) -> PgCatalogStore {
    PgCatalogStore::new(pool.clone(), metrics(pool), CancellationToken::new())
}

/// Build a valid [`UsageTypeGtsId`] from a raw string.
///
/// `UsageTypeGtsId::new` validates against the reserved GTS base
/// [`UsageTypeGtsId::USAGE_RECORD_BASE`]
/// (`gts.cf.core.uc.usage_record.v1~`), so callers must pass a fully-formed
/// derived instance id (e.g.
/// `gts.cf.core.uc.usage_record.v1~cf.compute._.vcpu_hours.v1`).
#[must_use]
pub fn fixture_gts_id(gts: &str) -> UsageTypeGtsId {
    UsageTypeGtsId::new(gts).expect("fixture gts_id must be a valid usage-type GTS instance id")
}

/// Build a [`UsageType`] fixture from raw parts.
///
/// `kind` is `"counter"` / `"gauge"` (parsed via the SDK `FromStr`); `fields`
/// become validated [`MetadataKey`]s.
#[must_use]
pub fn fixture_usage_type(gts: &str, kind: &str, fields: &[&str]) -> UsageType {
    let kind: UsageKind = kind.parse().expect("fixture kind must be counter/gauge");
    let metadata_fields = fields
        .iter()
        .map(|field| MetadataKey::new(*field).expect("fixture metadata field must be valid"))
        .collect();
    UsageType {
        gts_id: fixture_gts_id(gts),
        kind,
        metadata_fields,
    }
}

/// Build a minimal [`UsageRecord`] fixture referencing `gts_id`.
///
/// `id` is minted from `seq` via [`Uuid::from_u128`] (the workspace `uuid`
/// crate has no `v4` feature); `created_at` is a fixed post-epoch instant so
/// assertions stay deterministic and round-trip through Postgres `timestamptz`
/// without sub-microsecond drift. `corrects_id` and `subject_ref` are absent;
/// `metadata` is empty. Mutate the returned record's fields directly in the
/// test for the compensation case (set a negative `value` + `corrects_id`).
#[must_use]
pub fn fixture_usage_record(
    gts: &str,
    tenant_id: Uuid,
    idem: &str,
    value: Decimal,
    seq: u128,
) -> UsageRecord {
    UsageRecord {
        id: Uuid::from_u128(seq),
        gts_id: fixture_gts_id(gts),
        tenant_id,
        resource_ref: ResourceRef::new("res-1", "compute.vm")
            .expect("fixture resource_ref must be valid"),
        subject_ref: None,
        metadata: std::collections::BTreeMap::new(),
        value,
        idempotency_key: IdempotencyKey::new(idem).expect("fixture idempotency_key must be valid"),
        corrects_id: None,
        status: usage_collector_sdk::UsageRecordStatus::Active,
        // A fixed whole-second instant (no sub-microsecond component) so the
        // value persisted by Postgres equals the value asserted in tests.
        //
        // 2023-11-14 — years outside any realistic retention window, so a
        // harness must NOT arm a droppable retention policy against it or the
        // registered `policy_retention` job deletes these rows mid-test. See
        // `NO_DROP_RETENTION_SECS`.
        created_at: OffsetDateTime::from_unix_timestamp(1_700_000_000)
            .expect("fixture created_at must be a valid unix timestamp"),
    }
}

/// Build a [`UsageRecord`] fixture with a caller-chosen `resource_id`.
///
/// [`fixture_usage_record`] hard-codes `resource_id = "res-1"`; the aggregation
/// group-by-resource test needs records spread across distinct resource ids, so
/// this variant rebuilds [`UsageRecord::resource_ref`] via
/// [`ResourceRef::new`] (keeping the same `resource_type`). All other fields
/// match [`fixture_usage_record`].
#[must_use]
pub fn fixture_usage_record_with_resource(
    gts: &str,
    tenant_id: Uuid,
    idem: &str,
    value: Decimal,
    seq: u128,
    resource_id: &str,
) -> UsageRecord {
    let mut rec = fixture_usage_record(gts, tenant_id, idem, value, seq);
    rec.resource_ref =
        ResourceRef::new(resource_id, "compute.vm").expect("fixture resource_ref must be valid");
    rec
}

/// Build a [`UsageRecord`] fixture carrying a `subject_ref`.
///
/// [`fixture_usage_record`] leaves `subject_ref` absent; the subject-dimension
/// aggregation and the subject round-trip tests need records that actually
/// persist a subject, so this variant sets [`UsageRecord::subject_ref`] via
/// [`SubjectRef::new`]. `subject_type` is optional. All other fields match
/// [`fixture_usage_record`].
#[must_use]
pub fn fixture_usage_record_with_subject(
    gts: &str,
    tenant_id: Uuid,
    idem: &str,
    value: Decimal,
    seq: u128,
    subject_id: &str,
    subject_type: Option<&str>,
) -> UsageRecord {
    let mut rec = fixture_usage_record(gts, tenant_id, idem, value, seq);
    rec.subject_ref =
        Some(SubjectRef::new(subject_id, subject_type).expect("fixture subject_ref must be valid"));
    rec
}

/// Insert a raw `usage_records` row referencing `gts_id`, bypassing the
/// (not-yet-implemented) record store.
///
/// Used by the FK-referenced-delete test to create a child row. `status`,
/// `metadata`, and `ingested_at` take their column defaults. The `id` and
/// `idempotency_key` are minted from a process-wide counter so repeated calls
/// do not collide on the primary key or the dedup unique constraint.
///
/// # Errors
///
/// Returns any `sqlx` error from the `INSERT`.
pub async fn insert_raw_usage_record(
    pool: &PgPool,
    gts_id: &str,
    tenant_id: Uuid,
) -> anyhow::Result<()> {
    let seq = RAW_RECORD_SEQ.fetch_add(1, Ordering::Relaxed);
    let id = Uuid::from_u128(u128::from(seq));
    let idempotency_key = format!("raw-idem-{seq}");

    sqlx::query(
        "INSERT INTO usage_records \
         (id, tenant_id, gts_id, value, created_at, resource_id, resource_type, idempotency_key) \
         VALUES ($1, $2, $3, $4, now(), $5, $6, $7)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(gts_id)
    .bind(Decimal::ONE)
    .bind("res-1")
    .bind("compute.vm")
    .bind(&idempotency_key)
    .execute(pool)
    .await?;

    Ok(())
}
