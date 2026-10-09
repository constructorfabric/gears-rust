//! Synchronous engine execution budgets. Budgets are rebuilt for each evaluation,
//! independently of immutable compiled policy artifacts.

use std::num::NonZeroU64;
use std::time::{Duration, Instant};

/// Closed failures reported by a resolution engine.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    /// The wall-time limit expired.
    #[error("engine evaluation timed out")]
    Timeout,
    /// The operation budget was exhausted.
    #[error("engine evaluation cost exceeded")]
    CostExceeded,
    /// An expression or result has an incompatible type.
    #[error("engine type error: {0}")]
    TypeError(String),
    /// A configuration cannot be evaluated by this engine.
    #[error("invalid engine configuration: {0}")]
    InvalidConfig(String),
    /// An engine failed internally.
    #[error("internal engine failure: {0}")]
    Internal(String),
}

/// Validated limits for one engine invocation, excluding preparation and SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvaluationBudget {
    timeout: Duration,
    cost_limit: NonZeroU64,
}

impl EvaluationBudget {
    /// Documented timeout when the policy does not request one.
    pub const DEFAULT_TIMEOUT_MS: u64 = 5;

    /// Resolve a persisted request against the currently loaded operator clamp.
    /// Neither the persisted request nor a compiled artifact is modified.
    ///
    /// # Errors
    /// Returns `InvalidConfig` for an explicitly requested zero timeout.
    pub fn new(
        requested_timeout_ms: Option<u64>,
        upper_timeout_ms: NonZeroU64,
        cost_limit: NonZeroU64,
    ) -> Result<Self, EngineError> {
        let requested = requested_timeout_ms.unwrap_or(Self::DEFAULT_TIMEOUT_MS);
        if requested == 0 {
            return Err(EngineError::InvalidConfig(
                "timeout must be positive".into(),
            ));
        }
        Ok(Self {
            timeout: Duration::from_millis(requested.min(upper_timeout_ms.get())),
            cost_limit,
        })
    }

    /// The budget of one batch item: whatever remains of the batch timer,
    /// which supersedes the policy's own timeout. `None` once nothing
    /// remains.
    #[must_use]
    pub fn within(remaining: Duration, cost_limit: NonZeroU64) -> Option<Self> {
        (!remaining.is_zero()).then_some(Self {
            timeout: remaining,
            cost_limit,
        })
    }

    /// Maximum engine wall time for this invocation.
    #[must_use]
    pub const fn timeout(self) -> Duration {
        self.timeout
    }

    /// Maximum charged operations for this invocation.
    #[must_use]
    pub const fn cost_limit(self) -> NonZeroU64 {
        self.cost_limit
    }

    /// Start internal accounting immediately before evaluation.
    #[must_use]
    pub fn start(self) -> EvaluationMeter {
        EvaluationMeter {
            budget: self,
            started: Instant::now(),
            remaining: self.cost_limit.get(),
        }
    }
}

/// Bounds of one engine invocation, applied when the budget is built and never
/// frozen into a persisted version or a compiled artifact. The clamp, not a
/// resolved budget, is what crosses the storage boundary: only the transaction
/// knows which policy version it selected, so only it can resolve that
/// version's requested timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvaluationLimits {
    /// Ceiling a policy's requested `timeout_ms` is clamped to.
    pub upper_timeout_ms: NonZeroU64,
    /// Operations an engine may charge before `CostExceeded`.
    pub cost_limit: NonZeroU64,
}

impl EvaluationLimits {
    /// The budget for one evaluation of a policy that requested
    /// `requested_timeout_ms`, recomputed per evaluation so a tighter clamp
    /// loaded at process start applies without republishing any version.
    ///
    /// # Errors
    /// `InvalidConfig` for an explicitly requested zero timeout.
    pub fn budget(
        &self,
        requested_timeout_ms: Option<u64>,
    ) -> Result<EvaluationBudget, EngineError> {
        EvaluationBudget::new(requested_timeout_ms, self.upper_timeout_ms, self.cost_limit)
    }
}

/// Invocation-local accounting. Engines must charge before bounded operations;
/// this cannot preempt a blocking call or an uninstrumented interpreter.
#[derive(Debug)]
pub struct EvaluationMeter {
    budget: EvaluationBudget,
    started: Instant,
    remaining: u64,
}

impl EvaluationMeter {
    /// Check elapsed wall time and reserve execution cost before doing work.
    ///
    /// # Errors
    /// Returns `Timeout` on deadline expiry or `CostExceeded` on exhaustion.
    pub fn charge(&mut self, cost: u64) -> Result<(), EngineError> {
        if self.started.elapsed() >= self.budget.timeout {
            return Err(EngineError::Timeout);
        }
        self.remaining = self
            .remaining
            .checked_sub(cost)
            .ok_or(EngineError::CostExceeded)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_request_is_reclamped_for_each_evaluation() -> Result<(), EngineError> {
        let high = NonZeroU64::new(10).ok_or_else(|| EngineError::Internal("fixture".into()))?;
        let low = NonZeroU64::new(2).ok_or_else(|| EngineError::Internal("fixture".into()))?;
        assert_eq!(
            EvaluationBudget::new(None, high, high)?.timeout(),
            Duration::from_millis(5)
        );
        assert_eq!(
            EvaluationBudget::new(Some(8), high, high)?.timeout(),
            Duration::from_millis(8)
        );
        assert_eq!(
            EvaluationBudget::new(Some(8), low, high)?.timeout(),
            Duration::from_millis(2)
        );
        assert_eq!(
            EvaluationBudget::new(None, low, high)?.timeout(),
            Duration::from_millis(2)
        );
        assert!(matches!(
            EvaluationBudget::new(Some(0), high, high),
            Err(EngineError::InvalidConfig(_))
        ));
        Ok(())
    }

    #[test]
    fn cost_cannot_underflow() -> Result<(), EngineError> {
        let limit =
            NonZeroU64::new(u64::MAX).ok_or_else(|| EngineError::Internal("fixture".into()))?;
        let mut meter = EvaluationBudget::new(None, limit, limit)?.start();
        meter.charge(u64::MAX)?;
        assert_eq!(meter.charge(1), Err(EngineError::CostExceeded));
        Ok(())
    }
}

/// Persistable, engine-dependent schema closure. MRW uses an empty closure.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySchemaSnapshot {
    /// Fully resolved documents keyed by canonical schema identifier.
    pub schemas: std::collections::BTreeMap<String, serde_json::Value>,
    /// Environment schema for each metric admitted by this policy.
    pub environments: Vec<MetricEnvironmentSchema>,
    /// Which environment inputs the policy reads. Only their contracts are
    /// retained above, and a later activation is checked only against them,
    /// so a catalogue change to an input the policy never touches cannot
    /// invalidate it.
    #[serde(default)]
    pub inputs: EnvironmentInputs,
}

/// Server-resolved input for save-time validation; never accepted from a client.
pub struct EngineValidationInput<'a> {
    /// Operator-authored engine configuration.
    pub raw: &'a serde_json::Value,
    /// Trusted schema closure used for validation and offline rebuilds.
    pub schemas: &'a PolicySchemaSnapshot,
}

/// Structured save-time error. Source positions are one-based when available.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct EngineConfigError {
    /// Actionable description without reproducing sensitive configuration.
    pub message: String,
    /// Source line, when the parser provides it.
    pub line: Option<usize>,
    /// Source column, when the parser provides it.
    pub column: Option<usize>,
}

/// Which typed environment inputs a compiled policy reads. Decides which
/// schemas its version persists and which catalogue changes can invalidate a
/// later activation: a policy that never reads `resource` is not touched by a
/// resource projection being added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "three independent, individually named flags; a bitset would hide them on the wire"
)]
pub struct EnvironmentInputs {
    /// The request projection metadata.
    pub request: bool,
    /// The resource projection.
    pub resource: bool,
    /// Each applicable Quota's arbitration metadata.
    pub arbitration: bool,
}

impl EnvironmentInputs {
    /// Everything: the conservative answer for an engine that does not report.
    pub const ALL: Self = Self {
        request: true,
        resource: true,
        arbitration: true,
    };
    /// Nothing: an engine that reads no metadata at all.
    pub const NONE: Self = Self {
        request: false,
        resource: false,
        arbitration: false,
    };
}

impl Default for EnvironmentInputs {
    fn default() -> Self {
        Self::ALL
    }
}

/// Immutable compiled engine artifact, safe to share across invocations.
pub trait ValidatedConfig: std::any::Any + Send + Sync {
    /// Downcast at the owning engine boundary.
    fn as_any(&self) -> &dyn std::any::Any;

    /// The environment inputs this artifact reads. Defaults to all of them;
    /// an engine that can tell narrows it so unrelated schema changes do not
    /// invalidate its policies.
    fn inputs(&self) -> EnvironmentInputs {
        EnvironmentInputs::ALL
    }
}

/// Resolved subject specificity; custom owner projection IDs do not affect order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QuotaScopeTier {
    /// Tenant-wide fallback tier.
    Tenant,
    /// User-specific tier, preferred over tenant quotas.
    User,
}

/// One applicable quota with the tier resolved before engine invocation.
#[derive(Debug)]
pub struct EvaluationQuota<'a> {
    /// Current transaction-visible accounting snapshot.
    pub snapshot: &'a crate::QuotaSnapshot,
    /// Resolved owner scope tier.
    pub tier: QuotaScopeTier,
    /// Owner-defined, validated arbitration value; inaccessible to MRW.
    pub arbitration: &'a serde_json::Value,
}

/// Invocation-scoped, server-materialized input. No identity or I/O capability.
pub struct EvaluationContext<'a> {
    /// Authoritatively selected policy version.
    pub policy: &'a crate::PolicyVersion,
    /// Requested metric.
    pub metric: &'a crate::MetricId,
    /// Requested debit amount.
    pub amount: u64,
    /// Server evaluation time.
    pub time: time::OffsetDateTime,
    /// Every applicable quota, including those excluded by engine metadata rules.
    pub quotas: &'a [EvaluationQuota<'a>],
    /// Validated request projection value.
    pub request: &'a serde_json::Value,
    /// Validated resource projection value.
    pub resource: &'a serde_json::Value,
    /// Newly resolved limits for this invocation.
    pub budget: EvaluationBudget,
}

/// Statically linked, synchronous and internally bounded policy evaluator.
pub trait QuotaResolutionEngineV1: Send + Sync {
    /// Stable identifier from the deployment's bounded registration set.
    fn id(&self) -> &'static str;

    /// Validate and compile using only supplied schemas, without evaluation I/O.
    ///
    /// # Errors
    /// Returns actionable configuration or schema errors before persistence.
    fn validate_config(
        &self,
        input: EngineValidationInput<'_>,
    ) -> Result<std::sync::Arc<dyn ValidatedConfig>, EngineConfigError>;

    /// Evaluate deterministically, charging cost and checking wall time internally.
    ///
    /// # Errors
    /// Returns a closed engine failure; no mutation may follow an error.
    fn evaluate(
        &self,
        context: &EvaluationContext<'_>,
        config: &dyn ValidatedConfig,
    ) -> Result<crate::Decision, EngineError>;
}

/// One metric's persisted CEL environment schema.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricEnvironmentSchema {
    /// Admitted metric identifier.
    pub metric: crate::MetricId,
    /// Resolved environment schema.
    pub schema: serde_json::Value,
}

/// Closed debit-plan invariant labels used by telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DebitPlanInvariant {
    /// A debit references a quota absent from the resolved set.
    #[error("quota_id_outside_applicable_set")]
    QuotaIdOutsideApplicableSet,
    /// A signed engine output was negative before conversion to the SDK plan.
    #[error("negative_amount")]
    NegativeAmount,
    /// One debit exceeds the requested amount.
    #[error("amount_exceeds_request_amount")]
    AmountExceedsRequestAmount,
    /// Verdict and plan disagree, including an invalid MRW shape.
    #[error("result_plan_inconsistency")]
    ResultPlanInconsistency,
}

/// Decision that passed the engine boundary. Constructible only by validation.
#[derive(Debug)]
pub struct EvaluationOutcome {
    decision: crate::Decision,
}

impl EvaluationOutcome {
    /// Validate all shared invariants and the built-in MRW shape.
    ///
    /// # Errors
    /// Returns the first violated closed invariant, before storage may mutate.
    pub fn validate(
        mut decision: crate::Decision,
        context: &EvaluationContext<'_>,
    ) -> Result<Self, DebitPlanInvariant> {
        let applicable: std::collections::BTreeSet<_> =
            context.quotas.iter().map(|q| q.snapshot.quota_id).collect();
        for (id, debit) in &decision.debit_plan {
            if !applicable.contains(id) {
                return Err(DebitPlanInvariant::QuotaIdOutsideApplicableSet);
            }
            if debit.amount > context.amount {
                return Err(DebitPlanInvariant::AmountExceedsRequestAmount);
            }
        }
        match decision.result {
            crate::DecisionResult::Denied { .. } if !decision.debit_plan.is_empty() => {
                return Err(DebitPlanInvariant::ResultPlanInconsistency);
            }
            crate::DecisionResult::Allowed => {
                if decision.debit_plan.is_empty() && context.amount != 0 {
                    return Err(DebitPlanInvariant::ResultPlanInconsistency);
                }
                if context.policy.engine_id == "most-restrictive-wins"
                    && (decision.debit_plan.len() != 1
                        || decision
                            .debit_plan
                            .values()
                            .any(|debit| debit.amount != context.amount))
                {
                    return Err(DebitPlanInvariant::ResultPlanInconsistency);
                }
            }
            crate::DecisionResult::Denied { .. } => {}
        }
        decision.diagnostics.insert(
            "engine_id".into(),
            serde_json::json!(context.policy.engine_id),
        );
        decision.diagnostics.insert(
            "policy_id".into(),
            serde_json::json!(context.policy.policy_id),
        );
        decision.diagnostics.insert(
            "policy_version".into(),
            serde_json::json!(context.policy.version),
        );
        Ok(Self { decision })
    }

    /// Inspect the validated decision without cloning it.
    #[must_use]
    pub const fn decision(&self) -> &crate::Decision {
        &self.decision
    }

    /// Transfer the validated result to transaction-owned persistence.
    #[must_use]
    pub fn into_decision(self) -> crate::Decision {
        self.decision
    }
}

/// Transaction callback failure. Preparation requires rollback before compilation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvaluationFailure {
    /// The selected immutable artifact must be prepared outside the transaction.
    #[error("policy artifact preparation required for {policy_id} version {version}")]
    PreparationRequired {
        /// Authoritatively selected policy identifier.
        policy_id: crate::PolicyId,
        /// Authoritatively selected version.
        version: u32,
    },
    /// Engine failure; abort the transaction without publishing a decision.
    #[error(transparent)]
    Engine(#[from] EngineError),
    /// Invalid engine output; abort the transaction.
    #[error(transparent)]
    Invariant(#[from] DebitPlanInvariant),
}

/// Shared synchronous convention for debit, lease acquisition and atomic batch.
/// The plugin supplies transaction-selected state, owns all database handles,
/// and rolls back on error. The callback performs no I/O or externally visible
/// side effects and may be retried after preparation outside the transaction.
///
/// It is passed as an [`Arc`](std::sync::Arc) rather than a borrow because a
/// plugin has to move it into the future that runs inside its transaction, and
/// such a future may not name any lifetime of its caller.
pub type TransactionEvaluator = dyn for<'ctx> Fn(&EvaluationContext<'ctx>) -> Result<EvaluationOutcome, EvaluationFailure>
    + Send
    + Sync;
