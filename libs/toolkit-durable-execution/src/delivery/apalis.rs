//! `PostgreSQL` delivery transport. The authoritative execution state remains in
//! the secure journal; queue payloads contain only run ID, activity ID and delivery generation.

use std::sync::Arc;

use anyhow::Context as _;
use apalis::prelude::*;
use apalis_core::{backend::TaskSink, error::WorkerError};
use apalis_postgres::PostgresStorage;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// Queue payload that identifies one activity delivery for a run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Delivery {
    /// Structured activity instance being delivered.
    pub instance: crate::ActivityInstanceId,
    /// Run UUID in string form; unparsable values are dropped as poison messages.
    pub run_id: String,
    /// Journal delivery generation; stale generations are ignored.
    pub generation: i64,
    /// Canonical key; must agree with the structured instance.
    pub activity_id: String,
}

/// Executes deliveries taken from a queue.
#[async_trait]
pub trait DeliveryHandler: Send + Sync {
    /// Business failures must be persisted in the journal and return `Ok`.
    /// An error here is a transport failure; journal reconciliation retries it.
    async fn execute(&self, delivery: Delivery, shutdown: CancellationToken) -> anyhow::Result<()>;
}

// ADR 0001 permits SQLx only for the official adapter's private apalis schema.
// Journal and catalog queries must remain scoped toolkit-db operations.
/// Named Apalis queue backed by `PostgreSQL` storage.
#[allow(
    unknown_lints,
    de0706_no_direct_sqlx,
    reason = "ADR 0001 permits the official Apalis SQLx adapter for its private schema; the custom lint is absent on native toolchains."
)]
#[derive(Clone)]
pub struct Queue {
    pool: sqlx::PgPool,
    storage: PostgresStorage<Delivery>,
}

#[allow(
    unknown_lints,
    de0706_no_direct_sqlx,
    reason = "ADR 0001 permits the official Apalis SQLx adapter for its private schema; the custom lint is absent on native toolchains."
)]
impl Queue {
    /// Connects to `PostgreSQL`, applies the Apalis schema and opens `queue`.
    /// `concurrency` must be in `1..=128`.
    pub async fn connect(url: &str, queue: &str, concurrency: usize) -> anyhow::Result<Self> {
        anyhow::ensure!(concurrency > 0 && concurrency <= 128, "invalid concurrency");
        anyhow::ensure!(
            url.starts_with("postgres:") || url.starts_with("postgresql:"),
            "durable queue requires PostgreSQL"
        );
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(url)
            .await?;
        PostgresStorage::setup(&pool).await?;
        Ok(Self::from_pool(pool, queue, concurrency))
    }

    pub(super) fn from_pool(pool: sqlx::PgPool, queue: &str, concurrency: usize) -> Self {
        let config = apalis_postgres::Config::default()
            .queue(queue)
            .batch_size(concurrency);
        let storage = PostgresStorage::new(&pool).with_config(config);
        Self { pool, storage }
    }

    /// Returns a handle to queue `name` that shares this connection pool.
    pub fn named(&self, name: &str, concurrency: usize) -> Self {
        Self::from_pool(self.pool.clone(), name, concurrency)
    }
    #[cfg(all(test, feature = "integration"))]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub async fn push(&mut self, delivery: Delivery) -> anyhow::Result<()> {
        self.push_at(delivery, chrono::Utc::now()).await
    }

    /// Enqueues a single-attempt delivery runnable at `due`, rounded up to whole seconds.
    pub async fn push_at(
        &mut self,
        delivery: Delivery,
        due: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<()> {
        // Apalis stores seconds. Round upward so Retry-After is never shortened.
        let at = u64::try_from(
            due.timestamp()
                .saturating_add(i64::from(due.timestamp_subsec_nanos() != 0))
                .max(0),
        )
        .map_err(|_| anyhow::anyhow!("queue timestamp out of range"))?;
        let task = apalis_core::task::builder::TaskBuilder::new(delivery)
            .run_at_timestamp(at)
            .max_attempts(1)
            .build();
        self.storage
            .push_task(task)
            .await
            .context("queue enqueue failed")?;
        Ok(())
    }

    /// Consumes deliveries with `handler` until `shutdown` is cancelled.
    pub async fn run(
        self,
        handler: Arc<dyn DeliveryHandler>,
        shutdown: CancellationToken,
        concurrency: usize,
    ) -> anyhow::Result<()> {
        let data = HandlerData {
            handler,
            shutdown: shutdown.clone(),
        };
        let signal = async move {
            shutdown.cancelled().await;
            Ok::<(), WorkerError>(())
        };
        WorkerBuilder::new(format!("durable-{}", uuid::Uuid::new_v4()))
            // A persisted delivery must be discovered even when its enqueue
            // happened before this worker existed. Bound idle wakeups rather
            // than relying only on a transient notification/readiness wake.
            .backend(
                self.storage
                    .poll_with_interval(std::time::Duration::from_secs(5)),
            )
            .concurrency(concurrency)
            .data(data)
            .build(deliver)
            .run_until(signal)
            .await?;
        Ok(())
    }
}

#[derive(Clone)]
struct HandlerData {
    handler: Arc<dyn DeliveryHandler>,
    shutdown: CancellationToken,
}

async fn deliver(delivery: Delivery, data: Data<HandlerData>) -> Result<(), BoxDynError> {
    data.handler
        .execute(delivery, data.shutdown.child_token())
        .await
        .context("durable delivery handler failed")
        .map_err(|source| {
            Box::new(DeliveryFailure {
                source: source.into_boxed_dyn_error(),
            }) as BoxDynError
        })
}

// Apalis may persist either Display or Debug. Keep both redacted while retaining
// the backend chain for trusted internal inspection through Error::source().
struct DeliveryFailure {
    source: BoxDynError,
}
impl std::fmt::Display for DeliveryFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("durable delivery failed")
    }
}
impl std::fmt::Debug for DeliveryFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeliveryFailure").finish_non_exhaustive()
    }
}
impl std::error::Error for DeliveryFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/apalis_tests.rs"]
mod tests;

#[cfg(all(test, feature = "integration"))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/integration/apalis_tests.rs"]
mod integration_tests;

#[cfg(all(test, feature = "integration"))]
#[cfg_attr(coverage_nightly, coverage(off))]
impl Queue {
    pub(crate) fn test_pool(&self) -> sqlx::PgPool {
        self.pool.clone()
    }
}
