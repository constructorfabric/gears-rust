//! gRPC client transport configuration and connection utilities.
//!
//! This gear provides production-grade gRPC client configuration with:
//! - Configurable connect and RPC timeouts
//! - HTTP/2 keepalive settings for connection health
//! - Tracing spans around connection establishment
//!
//! **Note:** This gear handles both transport-level configuration and connection retries
//! ([`connect_with_retry`]). For RPC-level retry logic, see the [`crate::rpc_retry`] gear.

use std::time::Duration;

use tokio_retry::Retry;
use tokio_retry::strategy::{ExponentialBackoff, jitter};
use tonic::transport::{Channel, Endpoint};
use tracing::Instrument;

fn duration_to_i64_ms(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// Configuration for gRPC client transport stack.
///
/// This configuration controls transport-level settings such as timeouts and keepalive.
/// Retry-related fields (`max_retries`, `backoff_base_ms`, `backoff_factor`,
/// `max_backoff`) are stored here for convenience but are used by the
/// [`crate::rpc_retry`] gear, not by the transport layer.
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

    /// Base fed to `ExponentialBackoff::from_millis` — the growth ratio between
    /// attempts (delay sequence `backoff_base_ms^n * backoff_factor`).
    ///
    /// Used by both [`connect_with_retry`] and [`crate::rpc_retry::call_with_retry`].
    pub backoff_base_ms: u64,

    /// Multiplicative factor applied to every backoff delay. With the defaults
    /// (base `2`, factor `50`) the schedule is 100ms, 200ms, 400ms, ….
    ///
    /// Used by both [`connect_with_retry`] and [`crate::rpc_retry::call_with_retry`].
    pub backoff_factor: u64,

    /// Strict upper bound on backoff duration; each delay is capped here before
    /// full jitter (which only reduces it) is applied.
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
            backoff_base_ms: 2,
            backoff_factor: 50,
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

    /// Set the [`ExponentialBackoff`](tokio_retry::strategy::ExponentialBackoff)
    /// base (growth ratio between delays).
    pub fn with_backoff_base_ms(mut self, base_ms: u64) -> Self {
        self.backoff_base_ms = base_ms;
        self
    }

    /// Set the multiplicative factor applied to every backoff delay.
    pub fn with_backoff_factor(mut self, factor: u64) -> Self {
        self.backoff_factor = factor;
        self
    }

    /// Set the strict upper bound on any single backoff delay.
    pub fn with_max_backoff(mut self, duration: Duration) -> Self {
        self.max_backoff = duration;
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
/// use toolkit_transport_grpc::client::{connect_with_stack, GrpcClientConfig};
/// use toolkit_transport_grpc::rpc_retry::{call_with_retry, RpcRetryConfig};
/// use std::sync::Arc;
///
/// let config = GrpcClientConfig::new("my_service");
/// let client: MyServiceClient<Channel> = connect_with_stack(
///     "http://localhost:50051",
///     &config
/// ).await?;
///
/// // For retries, use the rpc_retry gear:
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
/// - `backoff_base_ms` / `backoff_factor`: `ExponentialBackoff` base and factor
///   (defaults `2`/`50` → 100ms, 200ms, 400ms, …)
/// - `max_backoff`: Strict upper bound on backoff duration (each delay is capped here)
///
/// Full jitter is then applied to each delay to spread out concurrent retries.
///
/// # Example
///
/// ```ignore
/// use toolkit_transport_grpc::client::{connect_with_retry, GrpcClientConfig};
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
    use anyhow::Context as _;

    let uri_string = uri.into();
    let uri_ref: &str = uri_string.as_str();
    let mut attempt: u32 = 0;
    let action = || {
        attempt += 1;
        let this_attempt = attempt;
        async move {
            let res = connect_with_stack::<TClient>(uri_ref, cfg).await;
            // A failure that still has retries left mirrors the previous
            // per-attempt WARN; the final failure is logged once after the loop.
            if let Err(ref e) = res
                && this_attempt <= cfg.max_retries
            {
                tracing::warn!(
                    service = cfg.service_name,
                    attempt = this_attempt,
                    max_retries = cfg.max_retries,
                    error = %e,
                    "gRPC connection failed, retrying..."
                );
            }
            res
        }
    };

    // Exponential backoff capped at `max_backoff`, with full jitter, over
    // `max_retries` retries — the config fields map straight onto the strategy.
    let strategy = ExponentialBackoff::from_millis(cfg.backoff_base_ms)
        .factor(cfg.backoff_factor)
        .max_delay(cfg.max_backoff)
        .map(jitter)
        .take(cfg.max_retries as usize);
    let result = Retry::start(strategy, action).await;

    match &result {
        Ok(_) if attempt > 1 => {
            tracing::info!(
                service = cfg.service_name,
                attempt,
                "gRPC connection established after retries"
            );
        }
        Err(e) => {
            tracing::error!(
                service = cfg.service_name,
                attempt,
                error = %e,
                "gRPC connection failed after all retries"
            );
        }
        Ok(_) => {}
    }

    result.with_context(|| {
        format!(
            "Failed to connect to {} after {} attempts",
            cfg.service_name, attempt
        )
    })
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
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = GrpcClientConfig::default();
        assert_eq!(cfg.connect_timeout, Duration::from_secs(10));
        assert_eq!(cfg.rpc_timeout, Duration::from_secs(30));
        assert_eq!(cfg.max_retries, 3);
        assert!(cfg.enable_metrics);
        assert!(cfg.enable_tracing);
    }

    #[test]
    fn test_config_builder() {
        let cfg = GrpcClientConfig::new("test_service")
            .with_connect_timeout(Duration::from_secs(5))
            .with_rpc_timeout(Duration::from_secs(15))
            .with_max_retries(5)
            .without_metrics()
            .without_tracing();

        assert_eq!(cfg.service_name, "test_service");
        assert_eq!(cfg.connect_timeout, Duration::from_secs(5));
        assert_eq!(cfg.rpc_timeout, Duration::from_secs(15));
        assert_eq!(cfg.max_retries, 5);
        assert!(!cfg.enable_metrics);
        assert!(!cfg.enable_tracing);
    }

    #[test]
    fn test_build_endpoint_succeeds() {
        let cfg = GrpcClientConfig::default();
        let result = build_endpoint("http://localhost:50051".to_owned(), &cfg);
        assert!(
            result.is_ok(),
            "build_endpoint should succeed with valid URI"
        );
    }

    #[test]
    fn test_build_endpoint_empty_uri() {
        let cfg = GrpcClientConfig::default();
        let result = build_endpoint(String::new(), &cfg);
        assert!(result.is_err(), "build_endpoint should fail with empty URI");
    }
}
