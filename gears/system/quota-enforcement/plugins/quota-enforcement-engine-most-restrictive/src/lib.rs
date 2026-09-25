//! Deterministic single-quota arbitration, per PRD section 5.9.
use std::collections::BTreeMap;
use std::sync::Arc;

use quota_enforcement_sdk::{
    Decision, DecisionResult, EngineConfigError, EngineError, EngineValidationInput,
    EnvironmentInputs, EvaluationContext, QuotaDebitPlan, QuotaResolutionEngineV1, ValidatedConfig,
};
use serde_json::json;

/// The statically linked metadata-independent engine.
#[derive(Debug, Default)]
pub struct MostRestrictiveWins;

#[derive(Debug)]
struct EmptyConfig;

impl ValidatedConfig for EmptyConfig {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    /// Reads no request, resource or arbitration metadata, so no catalogue
    /// change to any of them can strand a `most-restrictive-wins` policy.
    fn inputs(&self) -> EnvironmentInputs {
        EnvironmentInputs::NONE
    }
}

// @cpt-algo:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-mrw-engine:p1
impl QuotaResolutionEngineV1 for MostRestrictiveWins {
    fn id(&self) -> &'static str {
        "most-restrictive-wins"
    }

    fn validate_config(
        &self,
        input: EngineValidationInput<'_>,
    ) -> Result<Arc<dyn ValidatedConfig>, EngineConfigError> {
        if !input.raw.is_null() && !input.raw.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(EngineConfigError {
                message: "most-restrictive-wins requires empty configuration".into(),
                line: None,
                column: None,
            });
        }
        Ok(Arc::new(EmptyConfig))
    }

    fn evaluate(
        &self,
        context: &EvaluationContext<'_>,
        config: &dyn ValidatedConfig,
    ) -> Result<Decision, EngineError> {
        if !config.as_any().is::<EmptyConfig>() {
            return Err(EngineError::InvalidConfig(
                "artifact belongs to another engine".into(),
            ));
        }
        let mut meter = context.budget.start();
        let mut binding = None;
        let mut applicable = 0_u64;
        let mut violated = Vec::new();
        let mut details = BTreeMap::new();
        for quota in context.quotas {
            meter.charge(1)?;
            let snapshot = quota.snapshot;
            // @cpt-begin:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-window
            if snapshot
                .validity_window
                .is_some_and(|window| !window.contains(context.time))
            {
                continue;
            }
            // @cpt-end:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-window
            // @cpt-begin:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-metadata
            // `request`, `resource` and every `arbitration` value are ignored:
            // metadata-driven selection is what a `cel` policy is for.
            // @cpt-end:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-metadata
            applicable += 1;
            // @cpt-begin:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-satisfiable
            let remaining = snapshot
                .cap
                .map(|cap| cap.saturating_sub(snapshot.consumed));
            // @cpt-end:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-satisfiable
            // @cpt-begin:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-binding
            if remaining.is_none_or(|value| value >= context.amount) {
                let rank = (
                    std::cmp::Reverse(quota.tier),
                    remaining.is_none(),
                    remaining.unwrap_or(0),
                    snapshot.quota_id,
                );
                if binding.is_none_or(|previous| rank < previous) {
                    binding = Some(rank);
                }
            // @cpt-end:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-binding
            // @cpt-begin:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-deny-if
            } else {
                violated.push(snapshot.quota_id);
                // @cpt-end:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-deny-if
            }
            details.insert(snapshot.quota_id.to_string(), json!({
                "quota_id": snapshot.quota_id, "quota_type": snapshot.quota_type,
                "enforcement_mode": snapshot.enforcement_mode, "consumed": snapshot.consumed,
                "cap": snapshot.cap, "remaining": remaining, "requested": context.amount,
                "violation_amount": remaining.map_or(0, |value| context.amount.saturating_sub(value)),
                "contribution": 0,
            }));
        }
        let mut plan = BTreeMap::new();
        // @cpt-begin:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-return
        let result = if let Some((_, _, _, id)) = binding {
            plan.insert(
                id,
                QuotaDebitPlan {
                    amount: context.amount,
                },
            );
            if let Some(detail) = details.get_mut(&id.to_string()) {
                detail["contribution"] = json!(context.amount);
            }
            DecisionResult::Allowed
        // @cpt-end:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-return
        } else {
            // @cpt-begin:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-deny
            violated.sort_unstable();
            DecisionResult::Denied {
                violated_quota_ids: violated,
                // @cpt-begin:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-empty-if
                reason: if applicable == 0 {
                    // @cpt-begin:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-empty
                    "NO_APPLICABLE_QUOTA"
                    // @cpt-end:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-empty
                } else {
                    "QUOTA_EXCEEDED"
                }
                .into(),
                // @cpt-end:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-empty-if
            }
            // @cpt-end:cpt-cf-quota-enforcement-algo-most-restrictive-wins:p1:inst-mrw-deny
        };
        meter.charge(0)?;
        Ok(Decision {
            result,
            debit_plan: plan,
            diagnostics: BTreeMap::from([
                ("engine_id".into(), json!(self.id())),
                ("policy_id".into(), json!(context.policy.policy_id)),
                ("policy_version".into(), json!(context.policy.version)),
                ("quotas".into(), json!(details)),
            ]),
        })
    }
}
