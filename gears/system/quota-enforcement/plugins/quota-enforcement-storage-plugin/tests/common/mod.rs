//! Evaluation helpers the `PostgreSQL` suites share.

use std::sync::Arc;

use quota_enforcement_sdk::engine::{
    EvaluationContext, EvaluationLimits, EvaluationOutcome, TransactionEvaluator,
};
use quota_enforcement_sdk::{Decision, QuotaDebitPlan};

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
