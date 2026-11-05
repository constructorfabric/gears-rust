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
                (
                    quota.snapshot.quota_id,
                    QuotaDebitPlan {
                        amount: context.amount,
                    },
                )
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
