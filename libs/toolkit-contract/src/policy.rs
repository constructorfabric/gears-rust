//! Per-call policy stack for cross-cutting concerns.
//!
//! Policies run hooks before and after the transport call. The
//! [`PolicyStack`] composes an ordered list of [`Policy`] implementations
//! and drives execution through them.

use async_trait::async_trait;
use std::future::Future;
use std::sync::Arc;

use crate::error::ContractError;
use crate::ir::contract::{Idempotency, MethodKind};

/// Context passed to policy hooks for each contract call.
pub struct PolicyContext {
    /// Contract name being invoked.
    pub service: &'static str,
    /// Method name being invoked.
    pub method: &'static str,
    /// Idempotency classification (used for retry decisions).
    pub idempotency: Idempotency,
    /// Whether the method is unary or streaming.
    pub kind: MethodKind,
}

/// A policy that can intercept contract calls before and after transport.
///
/// Implement this trait to add cross-cutting concerns such as tracing,
/// metrics, or authorization checks.
#[async_trait]
pub trait Policy: Send + Sync {
    /// Called before the transport call is made.
    ///
    /// # Errors
    ///
    /// Return an error to short-circuit the call (subsequent policies
    /// and the transport call will be skipped).
    async fn on_request(&self, ctx: &PolicyContext) -> Result<(), ContractError>;

    /// Called after the transport call completes.
    ///
    /// # Errors
    ///
    /// Returning an error replaces the original transport result.
    async fn on_response(&self, ctx: &PolicyContext, success: bool) -> Result<(), ContractError>;
}

/// Ordered list of policies applied to every contract call.
///
/// Policies run `on_request` in insertion order and `on_response` in
/// reverse order (like middleware stacks).
pub struct PolicyStack {
    policies: Vec<Arc<dyn Policy>>,
}

impl PolicyStack {
    /// Create an empty policy stack.
    #[must_use]
    pub fn new() -> Self {
        Self {
            policies: Vec::new(),
        }
    }

    /// Append a policy to the end of the stack.
    pub fn push(&mut self, policy: Arc<dyn Policy>) {
        self.policies.push(policy);
    }

    /// Execute a contract call through the policy stack.
    ///
    /// 1. Runs `on_request` for each policy in order.
    /// 2. Invokes the transport closure `f`.
    /// 3. Runs `on_response` for each policy in reverse order.
    ///
    /// # Errors
    ///
    /// Returns the first error from any policy hook, or the transport
    /// error if the call itself fails.
    pub async fn execute<F, Fut, T, E>(
        &self,
        ctx: &PolicyContext,
        f: F,
        map_policy_err: fn(ContractError) -> E,
    ) -> Result<T, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        // Track the highest index for which `on_request` succeeded so that
        // we can symmetrically unwind those policies on the error path.
        let mut last_ok: Option<usize> = None;
        let mut request_err: Option<ContractError> = None;
        for (idx, policy) in self.policies.iter().enumerate() {
            match policy.on_request(ctx).await {
                Ok(()) => last_ok = Some(idx),
                Err(e) => {
                    request_err = Some(e);
                    break;
                }
            }
        }

        if let Some(e) = request_err {
            // Unwind already-succeeded policies in reverse with success=false.
            if let Some(top) = last_ok {
                for policy in self.policies[..=top].iter().rev() {
                    // Best-effort cleanup unwind: original request error takes precedence,
                    // so any error from on_response here is intentionally dropped.
                    drop(policy.on_response(ctx, false).await);
                }
            }
            return Err(map_policy_err(e));
        }

        let result = f().await;
        let success = result.is_ok();

        for policy in self.policies.iter().rev() {
            if let Err(e) = policy.on_response(ctx, success).await {
                return Err(map_policy_err(e));
            }
        }

        result
    }
}

impl Default for PolicyStack {
    fn default() -> Self {
        Self::new()
    }
}

/// Policy that emits `tracing` spans and log events for each contract call.
pub struct TracingPolicy;

#[async_trait]
impl Policy for TracingPolicy {
    async fn on_request(&self, ctx: &PolicyContext) -> Result<(), ContractError> {
        tracing::info!(
            service = ctx.service,
            method = ctx.method,
            idempotency = ?ctx.idempotency,
            kind = ?ctx.kind,
            "contract call started"
        );
        Ok(())
    }

    async fn on_response(&self, ctx: &PolicyContext, success: bool) -> Result<(), ContractError> {
        if success {
            tracing::info!(
                service = ctx.service,
                method = ctx.method,
                "contract call succeeded"
            );
        } else {
            tracing::warn!(
                service = ctx.service,
                method = ctx.method,
                "contract call failed"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct OrderRecorder {
        id: usize,
        log: Arc<parking_lot::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Policy for OrderRecorder {
        async fn on_request(&self, _ctx: &PolicyContext) -> Result<(), ContractError> {
            self.log.lock().push(format!("on_request:{}", self.id));
            Ok(())
        }

        async fn on_response(
            &self,
            _ctx: &PolicyContext,
            success: bool,
        ) -> Result<(), ContractError> {
            self.log
                .lock()
                .push(format!("on_response:{}:{success}", self.id));
            Ok(())
        }
    }

    fn test_ctx() -> PolicyContext {
        PolicyContext {
            service: "TestService",
            method: "test_method",
            idempotency: Idempotency::SafeRead,
            kind: MethodKind::Unary,
        }
    }

    #[tokio::test]
    async fn policy_stack_calls_in_order() {
        let log: Arc<parking_lot::Mutex<Vec<String>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));

        let mut stack = PolicyStack::new();
        stack.push(Arc::new(OrderRecorder {
            id: 1,
            log: Arc::clone(&log),
        }));
        stack.push(Arc::new(OrderRecorder {
            id: 2,
            log: Arc::clone(&log),
        }));

        let ctx = test_ctx();
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_inner = Arc::clone(&call_count);

        let result: Result<&str, ContractError> = stack
            .execute(
                &ctx,
                || async move {
                    call_count_inner.fetch_add(1, Ordering::Relaxed);
                    Ok("done")
                },
                std::convert::identity,
            )
            .await;

        assert_eq!(result.unwrap(), "done");
        assert_eq!(call_count.load(Ordering::Relaxed), 1);

        let entries = log.lock().clone();
        assert_eq!(
            entries,
            vec![
                "on_request:1",
                "on_request:2",
                "on_response:2:true",
                "on_response:1:true",
            ]
        );
    }

    struct FailPolicy;

    #[async_trait]
    impl Policy for FailPolicy {
        async fn on_request(&self, _ctx: &PolicyContext) -> Result<(), ContractError> {
            Err(ContractError::Validation("blocked by policy".to_owned()))
        }

        async fn on_response(
            &self,
            _ctx: &PolicyContext,
            _success: bool,
        ) -> Result<(), ContractError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn policy_stack_short_circuits_on_request_error() {
        let log: Arc<parking_lot::Mutex<Vec<String>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));

        let mut stack = PolicyStack::new();
        stack.push(Arc::new(FailPolicy));
        stack.push(Arc::new(OrderRecorder {
            id: 2,
            log: Arc::clone(&log),
        }));

        let ctx = test_ctx();
        let result: Result<&str, ContractError> = stack
            .execute(
                &ctx,
                || async { Ok("should not run") },
                std::convert::identity,
            )
            .await;

        assert!(result.is_err());
        let entries = log.lock().clone();
        assert!(entries.is_empty());
    }

    struct RecordCleanupPolicy {
        cleaned: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait]
    impl Policy for RecordCleanupPolicy {
        async fn on_request(&self, _ctx: &PolicyContext) -> Result<(), ContractError> {
            Ok(())
        }

        async fn on_response(
            &self,
            _ctx: &PolicyContext,
            _success: bool,
        ) -> Result<(), ContractError> {
            self.cleaned
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn on_request_error_invokes_on_response_for_succeeded_policies() {
        let cleaned = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut stack = PolicyStack::new();
        stack.push(Arc::new(RecordCleanupPolicy {
            cleaned: Arc::clone(&cleaned),
        }));
        stack.push(Arc::new(FailPolicy));

        let ctx = test_ctx();
        let result: Result<&str, ContractError> = stack
            .execute(
                &ctx,
                || async { Ok("should not run") },
                std::convert::identity,
            )
            .await;

        assert!(result.is_err());
        assert!(
            cleaned.load(std::sync::atomic::Ordering::SeqCst),
            "expected first policy's on_response to fire after second policy's on_request failed"
        );
    }

    #[tokio::test]
    async fn tracing_policy_does_not_error() {
        let policy = TracingPolicy;
        let ctx = test_ctx();

        policy.on_request(&ctx).await.unwrap();
        policy.on_response(&ctx, true).await.unwrap();
        policy.on_response(&ctx, false).await.unwrap();
    }
}
