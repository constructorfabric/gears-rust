//! Object-safe boundary between typed authoring and the durable-execution runtime.
//!
//! Workflow authors never need these items: [`crate::workflow::Step`] and
//! [`crate::Compensation`] produce them. They are public for the implementation crate.
use crate::{ActivityContext, ActivityError, authoring::CompensationPolicy};
use async_trait::async_trait;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};

/// Type-erased forward handler. Result payloads are persisted, so handlers return
/// identifiers or receipts rather than credentials or private domain objects.
#[async_trait]
pub trait ErasedActivity: Send + Sync {
    /// Execute an at-least-once invocation; honor cancellation and provider idempotency.
    ///
    /// # Errors
    /// Returns a safe retryable or permanent code, or confirmed cancellation.
    async fn execute(
        &self,
        context: ActivityContext,
        input: ActivityInput,
    ) -> Result<Value, ActivityError>;
}

/// Serialized input and checkpoint values supplied to one forward invocation.
#[derive(Clone)]
pub struct ActivityInput {
    /// Exact input selected for a runtime instance; `None` selects it from the flow.
    pub selected_input: Option<Value>,
    /// Original persisted run input.
    pub run_input: Value,
    /// Confirmed forward checkpoints keyed by template ID.
    pub previous_results: BTreeMap<String, Value>,
}
impl std::fmt::Debug for ActivityInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivityInput")
            .field("checkpoint_count", &self.previous_results.len())
            .finish_non_exhaustive()
    }
}

/// Type-erased reverse handler for one confirmed forward effect.
#[async_trait]
pub trait ErasedCompensation: Send + Sync {
    /// Reverse the effect described by the original input and confirmed output.
    ///
    /// # Errors
    /// Reports retryable or permanent reversal failures with safe codes.
    async fn execute(
        &self,
        context: ActivityContext,
        input: Value,
        output: Value,
    ) -> Result<(), ActivityError>;
}

/// Process-local reverse handler paired with its serializable policy.
#[derive(Clone)]
pub struct ErasedCompensationBinding {
    /// Persisted policy, included in the workflow fingerprint.
    pub policy: CompensationPolicy,
    /// Process-local reverse handler.
    pub handler: Arc<dyn ErasedCompensation>,
}
impl std::fmt::Debug for ErasedCompensationBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ErasedCompensationBinding")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}
