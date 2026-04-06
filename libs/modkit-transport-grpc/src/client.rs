//! gRPC client transport configuration and connection utilities.
//!
//! This module provides production-grade gRPC client configuration with:
//! - Configurable connect and RPC timeouts
//! - HTTP/2 keepalive settings for connection health
//! - Tracing spans around connection establishment
//!
//! **Note:** This module handles both transport-level configuration and connection retries
//! ([`connect_with_retry`]). For RPC-level retry logic, see the [`crate::rpc_retry`] module.

use std::time::Duration;

use rand::Rng as _;
use tonic::transport::{Channel, Endpoint};
use tracing::Instrument;

fn duration_to_i64_ms(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn duration_to_u64_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Configuration for gRPC client transport stack.
///
/// This configuration controls transport-level settings such as timeouts and keepalive.
/// Retry-related fields (`max_retries`, `base_backoff`, `max_backoff`) are stored here
/// for convenience but are used by the [`crate::rpc_retry`] module, not by the transport layer.
#[derive(Debug, Clone)]
#[must_use]
pub struct GrpcClientConfig {
    /// Timeout for establishing the initial connection.
    pub connect_timeout: Duration,

    /// Timeout for individual RPC calls (applied at transport level).
    pub rpc_timeout: Duration,

    /// Maximum number of retry attempts.
    ///
    /// Used by both [`connect_with_retry`] (connection retries) and
    /// [`crate::rpc_retry::call_with_retry`] (RPC-call retries).
    pub max_retries: u32,

    /// Initial backoff duration; doubled each attempt (`base * 2^(attempt-1)`).
    ///
    /// Used by both [`connect_with_retry`] and [`crate::rpc_retry::call_with_retry`].
    pub base_backoff: Duration,

    /// Strict upper bound on backoff duration, enforced both before and after jitter.
    ///
    /// Used by both [`connect_with_retry`] and [`crate::rpc_retry::call_with_retry`].
    pub max_backoff: Duration,

    /// Service name for metrics and tracing.
    pub service_name: &'static str,

    /// Enable Prometheus metrics collection.
    pub enable_metrics: bool,

    /// Enable OpenTelemetry tracing.
    pub enable_tracing: bool,
}

impl Default for GrpcClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            rpc_timeout: Duration::from_secs(30),
            max_retries: 3,
            base_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
            service_name: "grpc_client",
            enable_metrics: true,
            enable_tracing: true,
        }
    }
}

impl GrpcClientConfig {
    /// Create a new configuration with the given service name.
    pub fn new(service_name: &'static str) -> Self {
        Self {
            service_name,
            ..Default::default()
        }
    }

    /// Set the connect timeout.
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Set the RPC timeout.
    pub fn with_rpc_timeout(mut self, timeout: Duration) -> Self {
        self.rpc_timeout = timeout;
        self
    }

    /// Set the maximum number of retries.
    ///
    /// This value is used by [`crate::rpc_retry::call_with_retry`].
    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// Disable metrics collection.
    pub fn without_metrics(mut self) -> Self {
        self.enable_metrics = false;
        self
    }

    /// Disable tracing.
    pub fn without_tracing(mut self) -> Self {
        self.enable_tracing = false;
        self
    }
}

/// Build a tonic `Endpoint` with timeouts and keepalive settings.
///
/// Configures:
/// - Connect timeout
/// - Per-RPC timeout
/// - TCP keepalive (30 seconds)
/// - HTTP/2 keepalive interval (30 seconds)
/// - Keepalive timeout (10 seconds)
/// - Keep alive while idle
fn build_endpoint(
    uri: String,
    cfg: &GrpcClientConfig,
) -> Result<Endpoint, tonic::transport::Error> {
    let endpoint = Endpoint::from_shared(uri)?
        .connect_timeout(cfg.connect_timeout)
        .timeout(cfg.rpc_timeout)
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .http2_keep_alive_interval(Duration::from_secs(30))
        .keep_alive_timeout(Duration::from_secs(10))
        .keep_alive_while_idle(true);

    Ok(endpoint)
}

/// Connect to a gRPC service with the configured transport stack.
///
/// This function establishes a connection with:
/// - Configurable connect and RPC timeouts
/// - HTTP/2 keepalive for connection health
/// - A tracing span around the connection attempt
///
/// **Note:** This function does **not** perform retries or backoff at the transport level.
/// For RPC-level retry logic, use [`crate::rpc_retry::call_with_retry`] after obtaining
/// a client from this function.
///
/// # Example
///
/// ```ignore
/// use modkit_transport_grpc::client::{connect_with_stack, GrpcClientConfig};
/// use modkit_transport_grpc::rpc_retry::{call_with_retry, RpcRetryConfig};
/// use std::sync::Arc;
///
/// let config = GrpcClientConfig::new("my_service");
/// let client: MyServiceClient<Channel> = connect_with_stack(
///     "http://localhost:50051",
///     &config
/// ).await?;
///
/// // For retries, use the rpc_retry module:
/// let retry_cfg = Arc::new(RpcRetryConfig::from(&config));
/// let response = call_with_retry(
///     &mut client,
///     retry_cfg,
///     request,
///     |c, r| async move { c.my_method(r).await.map(|r| r.into_inner()) },
///     "my_service.my_method",
/// ).await?;
/// ```
///
/// # Errors
/// Returns an error if the connection cannot be established.
pub async fn connect_with_stack<TClient>(
    uri: impl Into<String>,
    cfg: &GrpcClientConfig,
) -> anyhow::Result<TClient>
where
    TClient: From<Channel>,
{
    let uri_string = uri.into();
    let span = tracing::debug_span!(
        "grpc_connect",
        service = cfg.service_name,
        uri = %uri_string
    );

    async move {
        let endpoint = build_endpoint(uri_string, cfg)?;
        let channel = endpoint.connect().await?;

        if cfg.enable_tracing {
            let connect_timeout_ms = duration_to_i64_ms(cfg.connect_timeout);
            let rpc_timeout_ms = duration_to_i64_ms(cfg.rpc_timeout);
            tracing::info!(
                service_name = cfg.service_name,
                connect_timeout_ms,
                rpc_timeout_ms,
                "gRPC client connected"
            );
        }

        Ok(TClient::from(channel))
    }
    .instrument(span)
    .await
}

/// Connect to a gRPC service with retry logic using exponential backoff and jitter.
///
/// This function attempts to establish a connection and retries on failure
/// using the retry parameters from [`GrpcClientConfig`]:
/// - `max_retries`: Maximum number of retry attempts
/// - `base_backoff`: Initial backoff duration; doubled each attempt (`base * 2^(attempt-1)`)
/// - `max_backoff`: Strict upper bound on backoff duration (enforced both before and after jitter)
///
/// A random jitter of 0–25 % is added after capping to spread out concurrent retries.
///
/// # Example
///
/// ```ignore
/// use modkit_transport_grpc::client::{connect_with_retry, GrpcClientConfig};
///
/// let config = GrpcClientConfig::new("my_service")
///     .with_max_retries(5);
///
/// let client: MyServiceClient<Channel> = connect_with_retry(
///     "http://localhost:50051",
///     &config
/// ).await?;
/// ```
///
/// # Errors
/// Returns an error if the connection fails after all retry attempts.
pub async fn connect_with_retry<TClient>(
    uri: impl Into<String>,
    cfg: &GrpcClientConfig,
) -> anyhow::Result<TClient>
where
    TClient: From<Channel>,
{
    use anyhow::Context;

    let uri_string = uri.into();
    let mut attempt: u32 = 0;

    loop {
        attempt += 1;

        match connect_with_stack::<TClient>(&uri_string, cfg).await {
            Ok(client) => {
                if attempt > 1 {
                    tracing::info!(
                        service = cfg.service_name,
                        attempt,
                        "gRPC connection established after retries"
                    );
                }
                return Ok(client);
            }
            Err(e) if attempt <= cfg.max_retries => {
                let jitter_factor = rand::rng().random_range(0.0..=0.25);
                let backoff = crate::backoff::compute_backoff(
                    cfg.base_backoff,
                    cfg.max_backoff,
                    attempt,
                    jitter_factor,
                );
                tracing::warn!(
                    service = cfg.service_name,
                    attempt,
                    max_retries = cfg.max_retries,
                    error = %e,
                    backoff_ms = duration_to_u64_ms(backoff),
                    "gRPC connection failed, retrying..."
                );
                tokio::time::sleep(backoff).await;
            }
            Err(e) => {
                tracing::error!(
                    service = cfg.service_name,
                    attempt,
                    error = %e,
                    "gRPC connection failed after all retries"
                );
                return Err(e).context(format!(
                    "Failed to connect to {} after {} attempts",
                    cfg.service_name, attempt
                ));
            }
        }
    }
}

/// Simple connection helper without custom configuration.
///
/// Uses default configuration with the provided service name.
/// This only sets up the transport connection; retries and backoff
/// for RPC calls should be handled using [`crate::rpc_retry::call_with_retry`].
///
/// # Errors
/// Returns an error if the connection cannot be established.
pub async fn connect<TClient>(
    uri: impl Into<String>,
    service_name: &'static str,
) -> anyhow::Result<TClient>
where
    TClient: From<Channel>,
{
    let cfg = GrpcClientConfig::new(service_name);
    connect_with_stack(uri, &cfg).await
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod tests;
