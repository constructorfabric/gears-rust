//! Worker-side delivery loop. The embedding gear must apply the journal
//! migrations and register definitions before starting this runtime.
use std::{collections::BTreeMap, sync::Arc, time::Duration};

use crate::RunId;
use async_trait::async_trait;
use chrono::Utc;
use tokio_util::sync::CancellationToken;

use crate::{
    delivery::apalis::{Delivery, DeliveryHandler, Queue},
    engine::executor::Executor,
    error::domain::DomainError,
    store::StoreError,
};

/// Internal adapter: neither transport credentials nor payloads escape the engine.
struct Handler(Arc<Executor>, Arc<super::shutdown::ShutdownBudget>);

#[async_trait]
impl DeliveryHandler for Handler {
    async fn execute(&self, delivery: Delivery, shutdown: CancellationToken) -> anyhow::Result<()> {
        // Malformed transport data cannot identify a legitimate run. Do not
        // retry a poison message or log its content.
        let Ok(id) = uuid::Uuid::parse_str(&delivery.run_id) else {
            return Ok(());
        };
        if delivery.activity_id != delivery.instance.key() {
            return Ok(());
        }
        self.0
            .execute_target(
                RunId(id),
                delivery.generation,
                (!delivery.activity_id.is_empty()).then_some(delivery.activity_id.as_str()),
                shutdown,
                &self.1,
            )
            .await?;
        Ok(())
    }
}

/// Worker runtime that owns the delivery queues and the delivery outbox.
pub struct Runtime {
    executor: Arc<Executor>,
    queues: BTreeMap<String, Queue>,
    outbox: Option<toolkit_db::outbox::OutboxHandle>,
}

/// Typed failure of the delivery runtime; the engine maps each variant to one
/// `LifecycleErrorKind`.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// A definition lacks handlers for some of its activities.
    #[error("durable handlers are incomplete")]
    Bindings(#[source] DomainError),
    /// The queue database cannot be reached or prepared.
    #[error("durable queue is unavailable")]
    Queue(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// Journal storage or the delivery outbox failed.
    #[error("durable storage failed")]
    Storage(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A queue worker stopped unexpectedly.
    #[error("durable queue worker stopped")]
    Worker(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// Work did not drain within the shutdown budget.
    #[error("durable runtime shutdown timed out")]
    ShutdownTimedOut,
}

impl Runtime {
    /// Applies the official queue adapter's migrations before consuming work.
    /// Journal migrations are applied by the embedding gear's lifecycle.
    pub async fn prepare(executor: Arc<Executor>, url: &str) -> Result<Self, RuntimeError> {
        executor
            .registry
            .validate_bindings()
            .map_err(RuntimeError::Bindings)?;
        let default_concurrency = executor
            .config
            .queues
            .get(&executor.config.default_queue)
            .copied()
            .ok_or_else(|| RuntimeError::Queue("default queue is not declared".into()))?;
        let transport = Queue::connect(url, &executor.config.default_queue, default_concurrency)
            .await
            .map_err(|error| RuntimeError::Queue(error.into()))?;
        let mut queues = BTreeMap::new();
        for (name, concurrency) in &executor.config.queues {
            queues.insert(name.clone(), transport.named(name, *concurrency));
        }
        let outbox = executor
            .store
            .start_outbox(crate::delivery::outbox::DeliverToApalis {
                store: executor.store.clone(),
                queues: queues.clone(),
                config: executor.config.clone(),
                #[cfg(all(test, feature = "integration"))]
                probe: None,
            })
            .await
            .map_err(|error| RuntimeError::Storage(error.into()))?;
        Ok(Self {
            executor,
            queues,
            outbox: Some(outbox),
        })
    }

    /// Runs queue workers, dispatch and the outbox until `shutdown` is cancelled.
    pub async fn run(self, shutdown: CancellationToken) -> Result<(), RuntimeError> {
        Box::pin(self.run_inner(shutdown)).await
    }
    async fn run_inner(mut self, shutdown: CancellationToken) -> Result<(), RuntimeError> {
        let outbox = self
            .outbox
            .take()
            .ok_or_else(|| RuntimeError::Storage("durable runtime outbox not prepared".into()))?;
        let local_stop = shutdown.child_token();
        let budget = Arc::new(super::shutdown::ShutdownBudget::new(Duration::from_secs(
            self.executor.config.shutdown_timeout_secs,
        )));
        let worker = async {
            let mut workers = tokio::task::JoinSet::new();
            if self.executor.config.execute_activities {
                for (name, queue) in &self.queues {
                    let queue = queue.clone();
                    let concurrency = self.executor.config.queues.get(name).copied().unwrap_or(1);
                    let handler = Arc::new(Handler(self.executor.clone(), budget.clone()));
                    let stop = local_stop.clone();
                    workers.spawn(async move { queue.run(handler, stop, concurrency).await });
                }
                let first = workers.join_next().await;
                local_stop.cancel();
                let worker_failure =
                    |joined: Result<anyhow::Result<()>, tokio::task::JoinError>| match joined {
                        Ok(Ok(())) => Ok(()),
                        Ok(Err(error)) => Err(RuntimeError::Worker(error.into())),
                        Err(error) => Err(RuntimeError::Worker(error.into())),
                    };
                let mut result = first.map_or(Ok(()), worker_failure);
                while let Some(joined) = workers.join_next().await {
                    let next = worker_failure(joined);
                    result = keep_first(result, next);
                }
                result
            } else {
                local_stop.cancelled().await;
                Ok(())
            }
        };
        let dispatcher = async {
            self.dispatch(local_stop.clone())
                .await
                .map_err(|error| RuntimeError::Storage(error.into()))
        };
        Box::pin(run_components(
            worker,
            dispatcher,
            local_stop.clone(),
            &budget,
            outbox.stop(),
        ))
        .await
    }

    async fn dispatch(&self, shutdown: CancellationToken) -> Result<(), StoreError> {
        super::control::control_loop(
            &self.executor.config,
            tokio::time::MissedTickBehavior::Skip,
            &shutdown,
            || self.dispatch_once(),
        )
        .await
    }
    async fn dispatch_once(&self) -> Result<(), StoreError> {
        self.executor.reconcile_definitions().await?;
        self.executor.recover().await?;
        let now = Utc::now();
        self.executor
            .store
            .recover_unclaimed_deliveries(
                now,
                now - chrono::Duration::seconds(
                    i64::try_from(self.executor.config.lease_secs)
                        .map_err(|_| StoreError::Invariant("lease out of range"))?,
                ),
            )
            .await?;
        Ok(())
    }
}

/// The failure that stopped the runtime wins; a later failure while draining is
/// logged instead of replacing it.
fn keep_first(
    first: Result<(), RuntimeError>,
    later: Result<(), RuntimeError>,
) -> Result<(), RuntimeError> {
    match (first, later) {
        (Err(first), Err(later)) => {
            tracing::warn!(error = %later, "durable runtime drain also failed");
            Err(first)
        }
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// Start the shared drain budget on cancellation even when both components are
/// awaiting external I/O. Dropping the worker future aborts its owned `JoinSet`.
/// The component failure that stopped the runtime is returned in preference to a
/// later drain failure or timeout.
async fn run_components(
    worker: impl std::future::Future<Output = Result<(), RuntimeError>>,
    dispatcher: impl std::future::Future<Output = Result<(), RuntimeError>>,
    stop: CancellationToken,
    budget: &super::shutdown::ShutdownBudget,
    outbox_stop: impl std::future::Future<Output = ()>,
) -> Result<(), RuntimeError> {
    tokio::pin!(worker, dispatcher);
    let (finished, result) = tokio::select! {
        biased;
        () = stop.cancelled() => (None, Ok(())),
        result = &mut worker => (Some(true), result),
        result = &mut dispatcher => (Some(false), result),
    };
    let deadline = budget.runtime_deadline();
    stop.cancel();
    let drained = tokio::time::timeout_at(deadline, async {
        let remaining = async {
            match finished {
                Some(true) => (&mut dispatcher).await,
                Some(false) => (&mut worker).await,
                None => {
                    let (worker, dispatcher) = tokio::join!(&mut worker, &mut dispatcher);
                    keep_first(worker, dispatcher)
                }
            }
        };
        let (joined, ()) = tokio::join!(remaining, outbox_stop);
        joined
    })
    .await
    .unwrap_or(Err(RuntimeError::ShutdownTimedOut));
    keep_first(result, drained)
}

#[cfg(all(test, feature = "integration"))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/integration/queue_runtime_tests.rs"]
mod integration_tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/runtime_tests.rs"]
mod tests;
