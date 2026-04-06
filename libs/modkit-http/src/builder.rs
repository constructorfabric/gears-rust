use crate::config::{
    HttpClientConfig, RedirectConfig, RetryConfig, TlsRootConfig, TransportSecurity,
};
use crate::error::HttpError;
use crate::layers::{OtelLayer, RetryLayer, SecureRedirectPolicy, UserAgentLayer};
use crate::response::ResponseBody;
use crate::tls;
use bytes::Bytes;
use http::Response;
use http_body_util::{BodyExt, Full};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioTimer};
use std::time::Duration;
use tower::buffer::Buffer;
use tower::limit::ConcurrencyLimitLayer;
use tower::load_shed::LoadShedLayer;
use tower::timeout::TimeoutLayer;
use tower::util::BoxCloneService;
use tower::{ServiceBuilder, ServiceExt};
use tower_http::decompression::DecompressionLayer;
use tower_http::follow_redirect::FollowRedirectLayer;

/// Type-erased inner service between layer composition steps in [`HttpClientBuilder::build`].
type InnerService =
    BoxCloneService<http::Request<Full<Bytes>>, http::Response<ResponseBody>, HttpError>;

/// Builder for constructing an [`HttpClient`] with a layered tower middleware stack.
pub struct HttpClientBuilder {
    config: HttpClientConfig,
    auth_layer: Option<Box<dyn FnOnce(InnerService) -> InnerService + Send>>,
}

impl HttpClientBuilder {
    /// Create a new builder with default configuration
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: HttpClientConfig::default(),
            auth_layer: None,
        }
    }

    /// Create a builder with a specific configuration
    #[must_use]
    pub fn with_config(config: HttpClientConfig) -> Self {
        Self {
            config,
            auth_layer: None,
        }
    }

    /// Set the per-request timeout
    ///
    /// This timeout applies to each individual HTTP request/attempt.
    /// If retries are enabled, each retry attempt gets its own timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.config.request_timeout = timeout;
        self
    }

    /// Set the total timeout spanning all retry attempts
    ///
    /// When set, the entire operation (including all retries and backoff delays)
    /// must complete within this duration. If the deadline is exceeded,
    /// the request fails with `HttpError::DeadlineExceeded(total_timeout)`.
    #[must_use]
    pub fn total_timeout(mut self, timeout: Duration) -> Self {
        self.config.total_timeout = Some(timeout);
        self
    }

    /// Set the user agent string
    #[must_use]
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.config.user_agent = user_agent.into();
        self
    }

    /// Set the retry configuration
    #[must_use]
    pub fn retry(mut self, retry: Option<RetryConfig>) -> Self {
        self.config.retry = retry;
        self
    }

    /// Set the maximum response body size
    #[must_use]
    pub fn max_body_size(mut self, size: usize) -> Self {
        self.config.max_body_size = size;
        self
    }

    /// Set transport security mode
    ///
    /// Use `TransportSecurity::TlsOnly` to enforce HTTPS for all connections.
    #[must_use]
    pub fn transport(mut self, transport: TransportSecurity) -> Self {
        self.config.transport = transport;
        self
    }

    /// Deny insecure HTTP connections, enforcing TLS for all traffic
    ///
    /// Equivalent to `.transport(TransportSecurity::TlsOnly)`.
    ///
    /// Use this when TLS enforcement is required (e.g., production environments).
    #[must_use]
    pub fn deny_insecure_http(mut self) -> Self {
        tracing::debug!(
            target: "modkit_http::security",
            "deny_insecure_http() called - enforcing TLS for all connections"
        );
        self.config.transport = TransportSecurity::TlsOnly;
        self
    }

    /// Enable OpenTelemetry tracing layer
    ///
    /// When enabled, creates spans for outbound requests with HTTP metadata
    /// and injects W3C trace context headers (when `otel` feature is enabled).
    #[must_use]
    pub fn with_otel(mut self) -> Self {
        self.config.otel = true;
        self
    }

    /// Insert an optional auth layer between retry and timeout in the stack.
    ///
    /// Stack position: `… → Retry → **this layer** → Timeout → …`
    ///
    /// The layer sits inside the retry loop so each attempt re-executes it
    /// (e.g. re-reads a refreshed bearer token). Only one auth layer can be
    /// set; a second call replaces the first.
    #[must_use]
    pub fn with_auth_layer(
        mut self,
        wrap: impl FnOnce(InnerService) -> InnerService + Send + 'static,
    ) -> Self {
        self.auth_layer = Some(Box::new(wrap));
        self
    }

    /// Set the buffer capacity for concurrent request handling
    ///
    /// The HTTP client uses an internal buffer to allow concurrent requests
    /// without external locking. This sets the maximum number of requests
    /// that can be queued.
    ///
    /// **Note**: A capacity of 0 is invalid and will be clamped to 1.
    /// Tower's Buffer panics with capacity=0, so we enforce minimum of 1.
    #[must_use]
    pub fn buffer_capacity(mut self, capacity: usize) -> Self {
        // Clamp to at least 1 - tower::Buffer panics with capacity=0
        self.config.buffer_capacity = capacity.max(1);
        self
    }

    /// Set the maximum number of redirects to follow
    ///
    /// Set to `0` to disable redirect following (3xx responses pass through as-is).
    /// Default: 10
    #[must_use]
    pub fn max_redirects(mut self, max_redirects: usize) -> Self {
        self.config.redirect.max_redirects = max_redirects;
        self
    }

    /// Disable redirect following
    ///
    /// Equivalent to `.max_redirects(0)`. When disabled, 3xx responses are
    /// returned to the caller without following the `Location` header.
    #[must_use]
    pub fn no_redirects(mut self) -> Self {
        self.config.redirect = RedirectConfig::disabled();
        self
    }

    /// Set the redirect policy configuration
    ///
    /// Use this to configure redirect security settings:
    /// - `same_origin_only`: Only follow redirects to the same host
    /// - `strip_sensitive_headers`: Remove `Authorization`/`Cookie` on cross-origin
    /// - `allow_https_downgrade`: Allow HTTPS → HTTP redirects (not recommended)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let client = HttpClient::builder()
    ///     .redirect(RedirectConfig::permissive()) // Allow all redirects with header stripping
    ///     .build()?;
    /// ```
    #[must_use]
    pub fn redirect(mut self, config: RedirectConfig) -> Self {
        self.config.redirect = config;
        self
    }

    /// Set the idle connection timeout for the connection pool
    ///
    /// Connections that remain idle for longer than this duration will be
    /// closed and removed from the pool. Default: 90 seconds.
    ///
    /// Set to `None` to disable idle timeout (connections kept indefinitely).
    #[must_use]
    pub fn pool_idle_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.config.pool_idle_timeout = timeout;
        self
    }

    /// Set the maximum number of idle connections per host
    ///
    /// Limits how many idle connections are kept in the pool for each host.
    /// Default: 32.
    ///
    /// - Setting to `0` disables connection reuse entirely
    /// - Setting too high may waste resources on rarely-used connections
    #[must_use]
    pub fn pool_max_idle_per_host(mut self, max: usize) -> Self {
        self.config.pool_max_idle_per_host = max;
        self
    }

    /// Build the HTTP client with all configured layers
    ///
    /// # Errors
    /// Returns an error if TLS initialization fails or configuration is invalid
    pub fn build(self) -> Result<crate::HttpClient, HttpError> {
        let timeout = self.config.request_timeout;
        let total_timeout = self.config.total_timeout;

        // Build the HTTPS connector (may fail for Native roots if no valid certs)
        let https = build_https_connector(self.config.tls_roots, self.config.transport)?;

        // Create the base hyper client with HTTP/2 support and connection pool settings
        let mut client_builder = Client::builder(TokioExecutor::new());

        // Configure connection pool
        // CRITICAL: pool_timer is required for pool_idle_timeout to work!
        client_builder
            .pool_timer(TokioTimer::new())
            .pool_max_idle_per_host(self.config.pool_max_idle_per_host)
            .http2_only(false); // Allow both HTTP/1 and HTTP/2 via ALPN

        // Set idle timeout (None = no timeout, connections kept indefinitely)
        if let Some(idle_timeout) = self.config.pool_idle_timeout {
            client_builder.pool_idle_timeout(idle_timeout);
        }

        let hyper_client = client_builder.build::<_, Full<Bytes>>(https);

        // Parse user agent header (may fail)
        let ua_layer = UserAgentLayer::try_new(&self.config.user_agent)?;

        // =======================================================================
        // Tower Layer Stack (outer to inner)
        // =======================================================================
        //
        // Request flow (outer → inner):
        //   Buffer → OtelLayer → LoadShed/Concurrency → RetryLayer →
        //   [AuthLayer?] → ErrorMapping → Timeout → UserAgent →
        //   Decompression → FollowRedirect → hyper_client
        //
        // AuthLayer (if set via with_auth_layer) sits inside the retry
        // loop so each retry re-acquires credentials (e.g. refreshed
        // bearer token).
        //
        // Response flow (inner → outer):
        //   hyper_client → FollowRedirect → Decompression → UserAgent →
        //   Timeout → ErrorMapping → [AuthLayer?] → RetryLayer →
        //   LoadShed/Concurrency → OtelLayer → Buffer
        //
        // Key semantics (reqwest-like):
        //  - send() returns Ok(Response) for ALL HTTP statuses (including 4xx/5xx)
        //  - send() returns Err only for transport/timeout/TLS errors
        //  - Non-2xx converted to error ONLY via error_for_status()
        //  - RetryLayer handles both Err (transport) and Ok(Response) (status)
        //     retries internally, draining body before retry for connection reuse
        //  - FollowRedirect handles 3xx responses internally with security protections:
        //     * Same-origin enforcement (default) - blocks SSRF attacks
        //     * Sensitive header stripping on cross-origin redirects
        //     * HTTPS downgrade protection
        //
        // =======================================================================
        //
        let redirect_policy = SecureRedirectPolicy::new(self.config.redirect.clone());

        // Build the service stack with secure redirect following
        let service = ServiceBuilder::new()
            .layer(TimeoutLayer::new(timeout))
            .layer(ua_layer)
            .layer(DecompressionLayer::new())
            .layer(FollowRedirectLayer::with_policy(redirect_policy))
            .service(hyper_client);

        // Map the decompression body to our boxed ResponseBody type.
        // This converts Response<DecompressionBody<Incoming>> to Response<ResponseBody>.
        //
        // The decompression body's error type is tower_http::BoxError, which we convert
        // to our boxed error type for consistency with the ResponseBody definition.
        let service = service.map_response(map_decompression_response);

        // Map errors to HttpError with proper timeout duration
        let service = service.map_err(move |e: tower::BoxError| map_tower_error(e, timeout));

        // Box the service for type erasure
        let mut boxed_service = service.boxed_clone();

        // Apply auth layer (between timeout and retry).
        // Inside retry so each retry attempt re-acquires the token.
        if let Some(wrap) = self.auth_layer {
            boxed_service = wrap(boxed_service);
        }

        // Conditionally wrap with RetryLayer
        //
        // RetryLayer handles retries for both:
        // - Err(HttpError::Transport/Timeout) - transport-level failures
        // - Ok(Response) with retryable status codes (429, 5xx for GET, etc.)
        //
        // When retrying on status codes, RetryLayer drains the response body
        // (up to configured limit) to allow connection reuse.
        //
        // If total_timeout is set, the entire operation (including all retries)
        // must complete within that duration.
        if let Some(ref retry_config) = self.config.retry {
            let retry_layer = RetryLayer::with_total_timeout(retry_config.clone(), total_timeout);
            let retry_service = ServiceBuilder::new()
                .layer(retry_layer)
                .service(boxed_service);
            boxed_service = retry_service.boxed_clone();
        }

        // Conditionally wrap with concurrency limit + load shedding
        // LoadShedLayer returns error immediately when ConcurrencyLimitLayer is saturated
        // instead of waiting indefinitely (Poll::Pending)
        if let Some(rate_limit) = self.config.rate_limit
            && rate_limit.max_concurrent_requests < usize::MAX
        {
            let limited_service = ServiceBuilder::new()
                .layer(LoadShedLayer::new())
                .layer(ConcurrencyLimitLayer::new(
                    rate_limit.max_concurrent_requests,
                ))
                .service(boxed_service);
            // Map load shed errors to HttpError::Overloaded
            let limited_service = limited_service.map_err(map_load_shed_error);
            boxed_service = limited_service.boxed_clone();
        }

        // Conditionally wrap with OTEL tracing layer (outermost layer before buffer)
        // Applied last so it sees the final request after UserAgent and other modifications.
        // Creates spans, records status, and injects trace context headers.
        if self.config.otel {
            let otel_service = ServiceBuilder::new()
                .layer(OtelLayer::new())
                .service(boxed_service);
            boxed_service = otel_service.boxed_clone();
        }

        // Wrap in Buffer as the final step for true concurrent access
        // Buffer spawns a background task that processes requests from a channel,
        // providing Clone + Send + Sync without any mutex serialization.
        let buffer_capacity = self.config.buffer_capacity.max(1);
        let buffered_service: crate::client::BufferedService =
            Buffer::new(boxed_service, buffer_capacity);

        Ok(crate::HttpClient {
            service: buffered_service,
            max_body_size: self.config.max_body_size,
            transport_security: self.config.transport,
        })
    }
}

impl Default for HttpClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Map tower errors to `HttpError` with actual timeout duration
///
/// Attempts to extract existing `HttpError` from the boxed error before
/// wrapping as `Transport`. This preserves typed errors like `Overloaded`
/// and `ServiceClosed` that may have been boxed by tower middleware.
fn map_tower_error(err: tower::BoxError, timeout: Duration) -> HttpError {
    if err.is::<tower::timeout::error::Elapsed>() {
        return HttpError::Timeout(timeout);
    }

    match err.downcast::<HttpError>() {
        Ok(http_err) => *http_err,
        Err(other) => HttpError::Transport(other),
    }
}

/// Map load shed errors to `HttpError::Overloaded`
fn map_load_shed_error(err: tower::BoxError) -> HttpError {
    if err.is::<tower::load_shed::error::Overloaded>() {
        HttpError::Overloaded
    } else {
        match err.downcast::<HttpError>() {
            Ok(http_err) => *http_err,
            Err(err) => HttpError::Transport(err),
        }
    }
}

/// Map the decompression response to our boxed response body type.
///
/// This converts `Response<DecompressionBody<Incoming>>` to `Response<ResponseBody>`
/// by boxing the body with appropriate error type mapping.
fn map_decompression_response<B>(response: Response<B>) -> Response<ResponseBody>
where
    B: hyper::body::Body<Data = Bytes> + Send + Sync + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let (parts, body) = response.into_parts();
    let boxed_body: ResponseBody = body.map_err(Into::into).boxed();
    Response::from_parts(parts, boxed_body)
}

/// Build the HTTPS connector with the specified TLS root configuration.
///
/// For `TlsRootConfig::Native`, uses cached native root certificates to avoid
/// repeated OS certificate store lookups on each `build()` call.
///
/// HTTP/2 is enabled via `enable_all_versions()` which configures ALPN to
/// advertise both h2 and http/1.1. Protocol selection happens during TLS
/// handshake based on server support.
///
/// # Errors
///
/// Returns `HttpError::Tls` if `TlsRootConfig::Native` is requested but no
/// valid root certificates are available from the OS certificate store.
fn build_https_connector(
    tls_roots: TlsRootConfig,
    transport: TransportSecurity,
) -> Result<HttpsConnector<HttpConnector>, HttpError> {
    let allow_http = transport == TransportSecurity::AllowInsecureHttp;

    match tls_roots {
        TlsRootConfig::WebPki => {
            let provider = tls::get_crypto_provider();
            let builder = hyper_rustls::HttpsConnectorBuilder::new()
                .with_provider_and_webpki_roots(provider)
                .map_err(|e| HttpError::Tls(Box::new(e)))?;
            let connector = if allow_http {
                builder.https_or_http().enable_all_versions().build()
            } else {
                builder.https_only().enable_all_versions().build()
            };
            Ok(connector)
        }
        TlsRootConfig::Native => {
            let client_config =
                tls::native_roots_client_config().map_err(|e| HttpError::Tls(e.into()))?;
            let builder = hyper_rustls::HttpsConnectorBuilder::new().with_tls_config(client_config);
            let connector = if allow_http {
                builder.https_or_http().enable_all_versions().build()
            } else {
                builder.https_only().enable_all_versions().build()
            };
            Ok(connector)
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "builder_tests.rs"]
mod tests;
