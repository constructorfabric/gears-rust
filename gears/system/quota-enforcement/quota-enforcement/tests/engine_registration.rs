#![allow(clippy::expect_used)]
//! An engine crate linked next to the gear joins the registry by submitting
//! itself, and evaluates like a built-in; nothing in the gear names it.

use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;

use quota_enforcement::domain::engines::{PolicyArtifactCache, evaluate_prepared, linked_registry};
use quota_enforcement::domain::ports::metrics::NoopMetrics;
use quota_enforcement_sdk::{
    Decision, DecisionResult, EnforcementMode, EngineConfigError, EngineError,
    EngineValidationInput, EvaluationBudget, EvaluationContext, EvaluationQuota, MetricId,
    PolicyId, PolicySchemaSnapshot, PolicyScope, PolicyVersion, PolicyVersionState, QuotaDebitPlan,
    QuotaId, QuotaResolutionEngineV1, QuotaScopeTier, QuotaSnapshot, QuotaType, ValidatedConfig,
};
use serde_json::{Value, json};
use time::OffsetDateTime;

const ENGINE_ID: &str = "linked-test-engine";

struct LinkedEngine;

struct NoConfig;

impl ValidatedConfig for NoConfig {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl QuotaResolutionEngineV1 for LinkedEngine {
    fn id(&self) -> &'static str {
        ENGINE_ID
    }

    fn validate_config(
        &self,
        _input: EngineValidationInput<'_>,
    ) -> Result<Arc<dyn ValidatedConfig>, EngineConfigError> {
        Ok(Arc::new(NoConfig))
    }

    fn evaluate(
        &self,
        context: &EvaluationContext<'_>,
        _config: &dyn ValidatedConfig,
    ) -> Result<Decision, EngineError> {
        let quota = context.quotas.first().ok_or(EngineError::CostExceeded)?;
        Ok(Decision {
            result: DecisionResult::Allowed,
            debit_plan: std::iter::once((
                quota.snapshot.quota_id,
                QuotaDebitPlan {
                    amount: context.amount,
                },
            ))
            .collect(),
            diagnostics: std::collections::BTreeMap::new(),
        })
    }
}

quota_enforcement_sdk::register_engine!(|| Arc::new(LinkedEngine));

fn policy() -> PolicyVersion {
    PolicyVersion {
        schema_snapshot: PolicySchemaSnapshot::default(),
        policy_id: PolicyId::global(),
        version: 1,
        scope: PolicyScope::Global,
        engine_id: ENGINE_ID.to_owned(),
        engine_config: json!({}),
        timeout_ms: None,
        description: None,
        state: PolicyVersionState::Active,
        created_at: OffsetDateTime::UNIX_EPOCH,
        created_by: "operator".to_owned(),
        comment: None,
    }
}

fn metric() -> MetricId {
    MetricId::parse("gts.cf.core.qe.metric_type.v1~cf.genai.llm_gateway.ai_tokens_input.v1")
        .expect("metric")
}

fn snapshot() -> QuotaSnapshot {
    QuotaSnapshot {
        quota_id: QuotaId::generate(),
        subject: serde_json::from_value(json!({
            "projection_type": "gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~",
            "subject_id": "alice"
        }))
        .expect("subject"),
        metric: metric(),
        quota_type: QuotaType::Consumption,
        enforcement_mode: EnforcementMode::Hard,
        cap: Some(100),
        consumed: 0,
        remaining: Some(100),
        period: None,
        metadata: serde_json::Map::new(),
        validity_window: None,
        currently_within_window: true,
    }
}

#[test]
fn a_linked_engine_joins_the_registry_beside_the_built_ins() {
    let registry = linked_registry().expect("no duplicate ids");
    assert_eq!(
        registry.ids().collect::<Vec<_>>(),
        vec!["cel", ENGINE_ID, "most-restrictive-wins"],
        "every linked engine, ordered by id"
    );
}

#[test]
fn a_linked_engine_evaluates_like_a_built_in() {
    let registry = linked_registry().expect("no duplicate ids");
    let cache = PolicyArtifactCache::new(
        NonZeroUsize::new(4).expect("capacity"),
        NonZeroUsize::new(1).expect("permits"),
    );
    cache.publish(PolicyId::global(), 1, Arc::new(NoConfig));
    let policy = policy();
    let quota = snapshot();
    let quotas = [EvaluationQuota {
        snapshot: &quota,
        tier: QuotaScopeTier::User,
        arbitration: &Value::Null,
    }];
    let thousand = NonZeroU64::new(1000).expect("nonzero");
    let context = EvaluationContext {
        policy: &policy,
        metric: &metric(),
        amount: 7,
        time: OffsetDateTime::UNIX_EPOCH,
        quotas: &quotas,
        request: &Value::Null,
        resource: &Value::Null,
        budget: EvaluationBudget::new(Some(1000), thousand, thousand).expect("budget"),
    };

    let outcome = evaluate_prepared(&registry, &cache, &context, &NoopMetrics)
        .expect("a linked engine must evaluate");
    assert_eq!(
        outcome.decision().debit_plan.get(&quota.quota_id),
        Some(&QuotaDebitPlan { amount: 7 })
    );
}
