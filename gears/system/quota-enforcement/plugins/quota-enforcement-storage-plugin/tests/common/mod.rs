//! Evaluation helpers the `PostgreSQL` suites share.
#![allow(dead_code, reason = "each suite uses its own subset")]

use std::sync::Arc;

use quota_enforcement_sdk::engine::{
    EvaluationContext, EvaluationLimits, EvaluationOutcome, TransactionEvaluator,
};
use quota_enforcement_sdk::{Decision, QuotaDebitPlan};
use testcontainers::ContainerAsync;
use testcontainers::core::ExecCommand;
use testcontainers_modules::postgres::Postgres;

pub fn limits() -> EvaluationLimits {
    EvaluationLimits {
        upper_timeout_ms: std::num::NonZeroU64::new(500).expect("nonzero"),
        cost_limit: std::num::NonZeroU64::new(100_000).expect("nonzero"),
    }
}

/// Allow the requested amount against every applicable Quota.
pub fn allow_all() -> Arc<TransactionEvaluator> {
    Arc::new(move |context: &EvaluationContext<'_>| {
        let plan = context
            .quotas
            .iter()
            .map(|quota| {
                vec![QuotaDebitPlan {
                    quota_id: quota.snapshot.quota_id,
                    amount: context.amount,
                }]
            })
            .collect();
        Ok(EvaluationOutcome::validate(
            Decision::allowed_with_plan(plan),
            context,
        )?)
    })
}

/// Sessions `PostgreSQL` shows waiting on a lock.
pub async fn lock_waiters(container: &ContainerAsync<Postgres>) -> u32 {
    let sql = "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock'";
    let args = ["psql", "-tA", "-U", "user", "-d", "app", "-c", sql];
    let mut output = container
        .exec(ExecCommand::new(args.map(str::to_owned)))
        .await
        .expect("psql runs in the container");
    let stdout = output.stdout_to_vec().await.expect("psql output");
    String::from_utf8_lossy(&stdout)
        .trim()
        .parse()
        .expect("a count")
}

/// Counts the statements `sqlx` reports while it dispatches for one future.
#[derive(Clone, Default)]
pub struct Statements(Arc<std::sync::atomic::AtomicUsize>);

impl Statements {
    /// `op`'s output and the statements it issued.
    pub async fn during<T>(op: impl std::future::Future<Output = T>) -> (T, usize) {
        use tracing::instrument::WithSubscriber as _;
        let counter = Self::default();
        let out = op
            .with_subscriber(tracing::Dispatch::new(counter.clone()))
            .await;
        (out, counter.0.load(std::sync::atomic::Ordering::SeqCst))
    }
}

impl tracing::Subscriber for Statements {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() == "sqlx::query"
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, _: &tracing::Event<'_>) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}
