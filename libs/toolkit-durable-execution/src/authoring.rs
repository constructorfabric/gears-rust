//! Fan-out limits and reverse handlers for code-defined activities.
use crate::runtime::{ErasedCompensation, ErasedCompensationBinding};
use crate::workflow::Payload;
use crate::{ActivityContext, ActivityError, DefinitionError, DefinitionErrorKind, RetryPolicy};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

/// Bounded expansion of one durable stage inside a run.
///
/// ```
/// use toolkit_durable_execution::FanOutOptions;
/// let options = FanOutOptions::default().max_items(100).concurrency(4);
/// assert!(options.validate().is_ok());
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FanOutOptions {
    /// Maximum number of items admitted before any branch starts.
    pub max_items: usize,
    /// Maximum simultaneous claims across all hosts for this stage.
    pub concurrency: usize,
}
impl Default for FanOutOptions {
    /// Up to 1024 items with 16 concurrent claims.
    fn default() -> Self {
        Self {
            max_items: Self::MAX_ITEMS,
            concurrency: 16,
        }
    }
}
impl FanOutOptions {
    /// Largest accepted `max_items`.
    pub const MAX_ITEMS: usize = 1024;
    /// Largest accepted `concurrency`.
    pub const MAX_CONCURRENCY: usize = 128;

    /// Reject runs whose list is longer than `max_items`.
    #[must_use]
    pub fn max_items(mut self, max_items: usize) -> Self {
        self.max_items = max_items;
        self
    }
    /// Limit simultaneous claims across all hosts.
    #[must_use]
    pub fn concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency;
        self
    }
    /// # Errors
    /// [`DefinitionErrorKind::InvalidFanOut`] for zero or excessive limits.
    pub fn validate(&self) -> Result<(), DefinitionError> {
        if !(1..=Self::MAX_ITEMS).contains(&self.max_items)
            || !(1..=Self::MAX_CONCURRENCY).contains(&self.concurrency)
        {
            return Err(DefinitionError::new(DefinitionErrorKind::InvalidFanOut));
        }
        Ok(())
    }
}

/// Independent timeout and retry budget for one compensator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompensationPolicy {
    /// Cleanup timeout.
    pub timeout: Duration,
    /// Automatic retry budget, independent of the forward budget.
    pub retry: RetryPolicy,
}

type ReverseHandler<I, O> = dyn Fn(ActivityContext, I, O) -> Pin<Box<dyn Future<Output = Result<(), ActivityError>> + Send>>
    + Send
    + Sync;

/// Typed compensator for a step's original input and confirmed output.
///
/// The handler receives the reverse-phase context: its
/// [`ActivityContext::idempotency_key`] differs from the forward key.
#[must_use = "attach the compensator to a step with `Step::compensate`"]
pub struct Compensation<I, O> {
    policy: CompensationPolicy,
    handler: Arc<ReverseHandler<I, O>>,
}
impl<I: Payload, O: Payload> Compensation<I, O> {
    /// The handler may run more than once; deduplicate external cleanup by the context key.
    pub fn new<F, Fut>(timeout: Duration, handler: F) -> Self
    where
        F: Fn(ActivityContext, I, O) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), ActivityError>> + Send + 'static,
    {
        Self {
            policy: CompensationPolicy {
                timeout,
                retry: RetryPolicy::default(),
            },
            handler: Arc::new(move |ctx, input, output| Box::pin(handler(ctx, input, output))),
        }
    }
    /// Override the independent reverse retry budget.
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.policy.retry = retry;
        self
    }
    /// Type-erase the handler for the runtime boundary.
    pub(crate) fn erase(self) -> ErasedCompensationBinding {
        ErasedCompensationBinding {
            policy: self.policy,
            handler: Arc::new(ReverseAdapter {
                handler: self.handler,
            }),
        }
    }
}
impl<I, O> std::fmt::Debug for Compensation<I, O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compensation")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}
struct ReverseAdapter<I, O> {
    handler: Arc<ReverseHandler<I, O>>,
}
#[async_trait]
impl<I: Payload, O: Payload> ErasedCompensation for ReverseAdapter<I, O> {
    async fn execute(
        &self,
        ctx: ActivityContext,
        input: Value,
        output: Value,
    ) -> Result<(), ActivityError> {
        let input = serde_json::from_value(input)
            .map_err(|_| ActivityError::permanent("compensation_input_decode_failed"))?;
        let output = serde_json::from_value(output)
            .map_err(|_| ActivityError::permanent("compensation_output_decode_failed"))?;
        (self.handler)(ctx, input, output).await
    }
}
