//! Call context for hand-written `#[client_manual]` REST methods (ADR-0009).
//!
//! The generated client opens the method's span, captures the credential for
//! the method's security plane and hands the handler a [`ManualCallContext`].
//! The handler owns encoding and response interpretation; the context owns the
//! destination, credential attachment and transport, so a manual method sends
//! with the same endpoint, credentials, tracing and tuning as a generated one.

use serde::de::DeserializeOwned;
use toolkit_http::{HttpClient, RequestBuilder};

use crate::ir::binding::{HttpBindingIr, HttpMethod, HttpMethodBindingIr};
use crate::runtime::config::{ClientConfig, InternalTokenProvider};
use crate::runtime::transport_error::TransportError;

/// Per-client transport state for manual methods that the generated client
/// keeps next to its `HttpClient`. Opaque; built by generated code only.
#[doc(hidden)]
pub struct ManualTransport {
    /// Metrics scope, as for the client's `toolkit-http` transport.
    #[cfg_attr(not(feature = "raw-http"), allow(dead_code))]
    pub(crate) client_type: &'static str,
    /// The `max_concurrent_requests` budget raw calls draw on.
    #[cfg_attr(not(feature = "raw-http"), allow(dead_code))]
    pub(crate) limiter: Option<toolkit_http::ConcurrencyLimiter>,
    #[cfg(feature = "raw-http")]
    pub(crate) raw: std::sync::OnceLock<crate::runtime::raw::RawTransport>,
}

impl ManualTransport {
    /// The default `toolkit-http` client and the manual transport, sharing
    /// one `max_concurrent_requests` budget. `client_type` names the metrics
    /// scope (the projection trait).
    ///
    /// # Errors
    /// As [`build_default_http_client`](crate::runtime::client::build_default_http_client).
    #[doc(hidden)]
    pub fn build(
        client_type: &'static str,
        config: &ClientConfig,
    ) -> Result<(HttpClient, Self), toolkit_http::HttpError> {
        let limiter = crate::runtime::client::concurrency_limiter(config);
        let http = crate::runtime::client::build_http_client_with_limiter(
            client_type,
            config,
            limiter.clone(),
        )?;
        Ok((http, Self::with_limiter(client_type, limiter)))
    }

    /// The manual transport next to a caller-supplied `toolkit-http` client,
    /// whose limit it cannot see: raw calls get a budget of their own from
    /// `config.max_concurrent_requests`.
    #[doc(hidden)]
    #[must_use]
    pub fn new(client_type: &'static str, config: &ClientConfig) -> Self {
        Self::with_limiter(
            client_type,
            crate::runtime::client::concurrency_limiter(config),
        )
    }

    fn with_limiter(
        client_type: &'static str,
        limiter: Option<toolkit_http::ConcurrencyLimiter>,
    ) -> Self {
        Self {
            client_type,
            limiter,
            #[cfg(feature = "raw-http")]
            raw: std::sync::OnceLock::new(),
        }
    }
}

/// What a manual handler receives on every call.
///
/// Borrowed from the client for the duration of the call. A handler that
/// returns a stream must not keep the borrow: it clones what later polling
/// needs (e.g. [`http`](Self::http) is cheap to clone).
pub struct ManualCallContext<'a> {
    http: &'a HttpClient,
    config: &'a ClientConfig,
    binding: &'a HttpBindingIr,
    method: &'a HttpMethodBindingIr,
    bearer: Option<String>,
    internal_token_provider: Option<InternalTokenProvider>,
    #[cfg_attr(not(feature = "raw-http"), allow(dead_code))]
    transport: &'a ManualTransport,
}

impl<'a> ManualCallContext<'a> {
    /// Built by generated code only. `bearer` is the tenant-plane token and
    /// `internal_token_provider` the platform-plane source; at most one is set.
    #[doc(hidden)]
    #[must_use]
    pub fn new(
        http: &'a HttpClient,
        config: &'a ClientConfig,
        binding: &'a HttpBindingIr,
        method: &'a HttpMethodBindingIr,
        bearer: Option<String>,
        internal_token_provider: Option<InternalTokenProvider>,
        transport: &'a ManualTransport,
    ) -> Self {
        Self {
            http,
            config,
            binding,
            method,
            bearer,
            internal_token_provider,
            transport,
        }
    }

    /// Base URL of the provider this call goes to: the configured or
    /// currently resolved endpoint.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.config.base_url
    }

    /// The client's configuration (timeouts, retry, streaming tuning).
    #[must_use]
    pub fn config(&self) -> &ClientConfig {
        self.config
    }

    /// The client's HTTP transport.
    #[must_use]
    pub fn http(&self) -> &HttpClient {
        self.http
    }

    /// Contract method name, as in the binding IR.
    #[must_use]
    pub fn method_name(&self) -> &str {
        &self.method.method_name
    }

    /// The projection's HTTP binding.
    #[must_use]
    pub fn binding(&self) -> &HttpBindingIr {
        self.binding
    }

    /// This method's HTTP binding.
    #[must_use]
    pub fn method_binding(&self) -> &HttpMethodBindingIr {
        self.method
    }

    /// The method's URL with `fields` substituted into its path template and
    /// an already-encoded `query` appended.
    ///
    /// # Errors
    /// [`TransportError::UrlBuild`] as for [`crate::runtime::http::build_request_url`].
    pub fn url(
        &self,
        fields: &serde_json::Value,
        query: Option<&str>,
    ) -> Result<String, TransportError> {
        crate::runtime::http::build_request_url(
            &self.config.base_url,
            &self.binding.base_path,
            self.method,
            fields,
            query,
        )
    }

    /// A request to `url` with the method's verb and the method's
    /// security-plane credential attached.
    ///
    /// # Errors
    /// [`TransportError::InvalidRequest`] for a passthrough method, which sends
    /// with [`send_raw`](Self::send_raw) instead.
    pub fn request(&self, url: &str) -> Result<RequestBuilder, TransportError> {
        let builder = match self.method.http_method {
            HttpMethod::Get => self.http.get(url),
            HttpMethod::Post => self.http.post(url),
            HttpMethod::Put => self.http.put(url),
            HttpMethod::Patch => self.http.patch(url),
            HttpMethod::Delete => self.http.delete(url),
            HttpMethod::Head => self.http.head(url),
            HttpMethod::Options => self.http.options(url),
            HttpMethod::Passthrough => {
                return Err(TransportError::InvalidRequest(format!(
                    "method '{}' is a passthrough binding; send it with send_raw",
                    self.method.method_name
                )));
            }
        };
        let builder = match &self.bearer {
            Some(token) => builder.bearer_auth(token),
            None => builder,
        };
        Ok(crate::runtime::http::attach_internal_token(
            builder,
            self.internal_token_provider.as_ref(),
            &self.method.method_name,
        ))
    }

    /// Send `builder` under the client's per-call timeout and decode a 2xx
    /// JSON body; other statuses become the same errors a generated method
    /// returns.
    ///
    /// # Errors
    /// The [`TransportError`] of the send, status or decode step.
    pub async fn send_json<R: DeserializeOwned>(
        &self,
        builder: RequestBuilder,
    ) -> Result<R, TransportError> {
        crate::runtime::client::send_unary(|| Ok(builder), Some(self.config.timeout)).await
    }

    #[cfg(feature = "raw-http")]
    /// The client's manual transport state.
    pub(crate) fn transport(&self) -> &ManualTransport {
        self.transport
    }

    #[cfg(feature = "raw-http")]
    /// The tenant bearer and the platform token source; at most one is set.
    pub(crate) fn credentials(&self) -> (Option<&str>, Option<&InternalTokenProvider>) {
        (
            self.bearer.as_deref(),
            self.internal_token_provider.as_ref(),
        )
    }
}
