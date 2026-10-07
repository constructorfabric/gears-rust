//! Worker-side delivery loop. The composition root must prepare journal
//! migrations and register definitions before starting this runtime.
use std::{collections::BTreeMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::Utc;
use durable_execution_sdk::RunId;
use tokio_util::sync::CancellationToken;

use crate::{
    infra::apalis::{Delivery, DeliveryHandler, Queue},
    infra::executor::Executor,
};

/// Internal adapter: neither transport credentials nor payloads escape the gear.
struct Handler(Arc<Executor>, Arc<super::shutdown::ShutdownBudget>);

#[async_trait]
impl DeliveryHandler for Handler {
    async fn execute(&self, delivery: Delivery, shutdown: CancellationToken) -> anyhow::Result<()> {
        // Malformed transport data cannot identify a legitimate run. Do not
        // retry a poison message or log its content.
        let Ok(id) = uuid::Uuid::parse_str(&delivery.run_id) else {
            return Ok(());
        };
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

pub struct Runtime {
    executor: Arc<Executor>,
    queues: BTreeMap<String, Queue>,
    outbox: Option<toolkit_db::outbox::OutboxHandle>,
}

impl Runtime {
    /// Applies the official queue adapter's migrations before consuming work.
    /// Journal migrations are owned by the gear's framework lifecycle.
    pub async fn prepare(executor: Arc<Executor>, url: &str) -> anyhow::Result<Self> {
        executor.config.validate().map_err(anyhow::Error::msg)?;
        executor.registry.validate_bindings()?;
        let transport = Queue::connect(
            url,
            &executor.config.default_queue,
            executor.config.queues[&executor.config.default_queue],
        )
        .await?;
        let mut queues = BTreeMap::new();
        for (name, concurrency) in &executor.config.queues {
            queues.insert(name.clone(), transport.named(name, *concurrency));
        }
        let outbox = executor
            .store
            .start_outbox(crate::infra::outbox::DeliverToApalis {
                store: executor.store.clone(),
                queues: queues.clone(),
                config: executor.config.clone(),
                #[cfg(all(test, feature = "integration"))]
                probe: None,
            })
            .await?;
        Ok(Self {
            executor,
            queues,
            outbox: Some(outbox),
        })
    }

    pub async fn run(mut self, shutdown: CancellationToken) -> anyhow::Result<()> {
        let outbox = self
            .outbox
            .take()
            .ok_or_else(|| anyhow::anyhow!("durable runtime outbox not prepared"))?;
        let local_stop = shutdown.child_token();
        let budget = Arc::new(super::shutdown::ShutdownBudget::new(Duration::from_secs(
            self.executor.config.shutdown_timeout_secs,
        )));
        let worker = async {
            let mut workers = tokio::task::JoinSet::new();
            if self.executor.config.execute_activities {
                for (name, queue) in &self.queues {
                    let queue = queue.clone();
                    let concurrency = self.executor.config.queues[name];
                    let handler = Arc::new(Handler(self.executor.clone(), budget.clone()));
                    let stop = local_stop.clone();
                    workers.spawn(async move { queue.run(handler, stop, concurrency).await });
                }
                let first = workers.join_next().await;
                local_stop.cancel();
                let result = match first {
                    Some(result) => result.map_err(anyhow::Error::from)?,
                    None => Ok(()),
                };
                while let Some(joined) = workers.join_next().await {
                    joined.map_err(anyhow::Error::from)??;
                }
                result
            } else {
                local_stop.cancelled().await;
                Ok(())
            }
        };
        let dispatcher = self.dispatch(local_stop.clone());
        run_components(
            worker,
            dispatcher,
            local_stop.clone(),
            &budget,
            outbox.stop(),
        )
        .await
    }

    async fn dispatch(&self, shutdown: CancellationToken) -> anyhow::Result<()> {
        let mut tick = tokio::time::interval(Duration::from_secs(
            self.executor.config.dispatch_interval_secs,
        ));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut retry = super::retry::RetryPolicy::new(&self.executor.config);
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => return Ok(()),
                _ = tick.tick() => {}
            }
            let result = tokio::select! {
                biased;
                () = shutdown.cancelled() => return Ok(()),
                result = self.dispatch_once() => result,
            };
            if let Err(error) = result {
                if matches!(
                    error,
                    crate::infra::storage::StoreError::AuthorizationUnavailable
                        | crate::infra::storage::StoreError::Authorization(_)
                        | crate::infra::storage::StoreError::Forbidden
                ) {
                    tracing::warn!("durable control plane paused: authorization unavailable");
                    continue;
                }
                if skippable(&error) {
                    continue;
                }
                if let Some(delay) = retry.retry_delay(&error) {
                    if !super::retry::wait_retry(delay, &shutdown).await {
                        return Ok(());
                    }
                    continue;
                }
                return Err(error.into());
            }
            retry.reset();
        }
    }
    async fn dispatch_once(&self) -> Result<(), crate::infra::storage::StoreError> {
        self.executor.reconcile_definitions().await?;
        self.executor.recover().await?;
        let now = Utc::now();
        self.executor
            .store
            .recover_unclaimed_deliveries(
                now,
                now - chrono::Duration::seconds(
                    i64::try_from(self.executor.config.lease_secs).map_err(|_| {
                        crate::infra::storage::StoreError::Invariant("lease out of range")
                    })?,
                ),
            )
            .await?;
        self.executor.store.forward_legacy_deliveries().await?;
        Ok(())
    }
}

/// Start the shared drain budget on cancellation even when both components are
/// awaiting external I/O. Dropping the worker future aborts its owned `JoinSet`.
async fn run_components(
    worker: impl std::future::Future<Output = anyhow::Result<()>>,
    dispatcher: impl std::future::Future<Output = anyhow::Result<()>>,
    stop: CancellationToken,
    budget: &super::shutdown::ShutdownBudget,
    outbox_stop: impl std::future::Future<Output = ()>,
) -> anyhow::Result<()> {
    tokio::pin!(worker, dispatcher);
    let (finished, result) = tokio::select! {
        biased;
        () = stop.cancelled() => (None, Ok(())),
        result = &mut worker => (Some(true), result),
        result = &mut dispatcher => (Some(false), result),
    };
    let deadline = budget.runtime_deadline();
    stop.cancel();
    tokio::time::timeout_at(deadline, async {
        let remaining = async {
            match finished {
                Some(true) => (&mut dispatcher).await,
                Some(false) => (&mut worker).await,
                None => {
                    let (worker, dispatcher) = tokio::join!(&mut worker, &mut dispatcher);
                    worker.and(dispatcher)
                }
            }
        };
        let (joined, ()) = tokio::join!(remaining, outbox_stop);
        joined
    })
    .await
    .map_err(|_| anyhow::anyhow!("durable runtime shutdown timed out"))??;
    result
}

/// Races are retried on the next tick; one broken run must not stop the
/// control plane for all others, so it is logged and skipped.
fn skippable(error: &crate::infra::storage::StoreError) -> bool {
    use crate::infra::storage::StoreError;
    match error {
        StoreError::Conflict => true,
        StoreError::Invariant(_) | StoreError::Domain(_) => {
            tracing::error!(error = %error, "durable control plane skipped a broken run");
            true
        }
        _ => false,
    }
}

#[cfg(all(test, feature = "integration"))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/integration/queue_runtime_tests.rs"]
mod integration_tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/runtime_tests.rs"]
mod tests;
