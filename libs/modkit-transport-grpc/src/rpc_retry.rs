//! RPC-level retry helper for unary gRPC calls.
//!
//! This module provides a generic retry helper [`call_with_retry`] that implements
//! safe retries with exponential backoff and jitter for unary gRPC calls.
//!
//! ## Retry Policy
//!
//! The helper retries only on transient, network-related errors:
//! - [`tonic::Code::Unavailable`] - Server is temporarily unavailable
//! - [`tonic::Code::DeadlineExceeded`] - Request timed out
//!
//! All other error codes are considered non-retryable and will be returned immediately.
//!
//! ## Idempotency Warning
//!
//! **This helper assumes the operation is idempotent.** Non-idempotent operations
//! (e.g., creating resources without deduplication) should **not** use this helper,
//! as retries may cause duplicate side effects.
//!
//! ## Example
//!
//! ```ignore
//! use modkit_transport_grpc::client::{connect_with_stack, GrpcClientConfig};
//! use modkit_transport_grpc::rpc_retry::{call_with_retry, RpcRetryConfig};
//! use std::sync::Arc;
//!
//! let cfg = GrpcClientConfig::new("my_service");
//! let retry_cfg = Arc::new(RpcRetryConfig::from(&cfg));
//! let mut client: MyServiceClient<Channel> = connect_with_stack(
//!     "http://127.0.0.1:50051",
//!     &cfg
//! ).await?;
//!
//! let req = MyRequest { /* ... */ };
//! let resp = call_with_retry(
//!     &mut client,
//!     retry_cfg.clone(),
//!     req,
//!     |c, r| async move { c.my_call(r).await.map(|resp| resp.into_inner()) },
//!     "my_service.my_call",
//! ).await?;
//! ```

use std::sync::Arc;
use std::time::Duration;

use rand::Rng as _;
use tokio::time::sleep;
use tonic::{Code, Status};
use tracing::Instrument;

fn duration_to_i64_ms(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// Configuration for RPC-level retry policy.
///
/// This struct extracts retry-related settings from [`crate::client::GrpcClientConfig`]
/// for use with [`call_with_retry`].
#[derive(Debug, Clone)]
#[must_use]
pub struct RpcRetryConfig {
    /// Maximum number of retry attempts (not including the initial call).
    pub max_retries: u32,

    /// Base duration for exponential backoff.
    ///
    /// The actual backoff duration is `base_backoff * 2^(attempt - 1)`,
    /// capped at `max_backoff`, plus up to 25 % random jitter.
    pub base_backoff: Duration,

    /// Maximum duration for exponential backoff.
    pub max_backoff: Duration,
}

impl Default for RpcRetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
        }
    }
}

impl From<&crate::client::GrpcClientConfig> for RpcRetryConfig {
    fn from(cfg: &crate::client::GrpcClientConfig) -> Self {
        Self {
            max_retries: cfg.max_retries,
            base_backoff: cfg.base_backoff,
            max_backoff: cfg.max_backoff,
        }
    }
}

impl RpcRetryConfig {
    /// Create a new retry configuration with the given maximum retries.
    pub fn new(max_retries: u32) -> Self {
        Self {
            max_retries,
            ..Default::default()
        }
    }

    /// Set the base backoff duration.
    pub fn with_base_backoff(mut self, duration: Duration) -> Self {
        self.base_backoff = duration;
        self
    }

    /// Set the maximum backoff duration.
    pub fn with_max_backoff(mut self, duration: Duration) -> Self {
        self.max_backoff = duration;
        self
    }
}

/// Generic helper for unary gRPC calls with retries.
///
/// Executes a gRPC call and retries on transient errors (`UNAVAILABLE`, `DEADLINE_EXCEEDED`)
/// with exponential backoff and jitter.
///
/// # Arguments
///
/// * `client` - The tonic gRPC client instance (e.g., `MyServiceClient<Channel>`)
/// * `cfg` - Shared retry configuration
/// * `req` - Request payload (must implement `Clone` for retry attempts)
/// * `call` - Closure that performs the actual RPC call
/// * `op_name` - Static name of the operation for logging/tracing (e.g., `"my_service.my_method"`)
///
/// # Type Parameters
///
/// * `TClient` - The gRPC client type
/// * `F` - The closure type that performs the RPC call
/// * `Fut` - The future returned by the closure
/// * `Req` - The request type (must be `Clone`)
/// * `Res` - The response type
///
/// # Returns
///
/// Returns `Ok(Res)` on success, or `Err(Status)` if all retry attempts fail
/// or a non-retryable error is encountered.
///
/// # Example
///
/// ```ignore
/// let resp = call_with_retry(
///     &mut client,
///     retry_cfg.clone(),
///     my_request,
///     |c, r| async move { c.get_user(r).await.map(|r| r.into_inner()) },
///     "users.get_user",
/// ).await?;
/// ```
///
/// # Errors
/// Returns `Status` error if the RPC fails after all retry attempts.
pub async fn call_with_retry<TClient, F, Fut, Req, Res>(
    client: &mut TClient,
    cfg: Arc<RpcRetryConfig>,
    req: Req,
    call: F,
    op_name: &'static str,
) -> Result<Res, Status>
where
    F: Fn(&mut TClient, Req) -> Fut,
    Fut: std::future::Future<Output = Result<Res, Status>>,
    Req: Clone,
{
    let mut attempt: u32 = 0;

    loop {
        attempt += 1;

        let span = tracing::debug_span!("grpc_call", op = op_name, attempt,);

        let result = async {
            let res = call(client, req.clone()).await;
            if let Err(ref status) = res {
                tracing::warn!(
                    code = ?status.code(),
                    message = %status.message(),
                    attempt,
                    op = op_name,
                    "gRPC call failed",
                );
            }
            res
        }
        .instrument(span)
        .await;

        match result {
            Ok(res) => {
                if attempt > 1 {
                    tracing::info!(op = op_name, attempt, "gRPC call succeeded after retries");
                }
                return Ok(res);
            }
            Err(status) => {
                let code = status.code();

                // Retry only on network-like errors
                let retryable = matches!(code, Code::Unavailable | Code::DeadlineExceeded);

                if !retryable || attempt > cfg.max_retries {
                    tracing::error!(
                        op = op_name,
                        attempt,
                        code = ?code,
                        "gRPC call giving up"
                    );
                    return Err(status);
                }

                let jitter_factor = rand::rng().random_range(0.0..=0.25);
                let backoff = crate::backoff::compute_backoff(
                    cfg.base_backoff,
                    cfg.max_backoff,
                    attempt,
                    jitter_factor,
                );

                tracing::debug!(
                    op = op_name,
                    attempt,
                    backoff_ms = duration_to_i64_ms(backoff),
                    "Retrying gRPC call after backoff"
                );

                sleep(backoff).await;
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "rpc_retry_tests.rs"]
mod tests;
