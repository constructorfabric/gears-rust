//! One exchange with the registry, shared by both REST clients: the retry policy, the
//! per-attempt deadline, the plane's credential and the success status and headers that
//! `runtime::client::send_unary` would discard.

use std::time::Duration;

use secrecy::SecretString;
use toolkit_contract::runtime::client::build_default_http_client;
use toolkit_contract::runtime::config::ClientConfig;
use toolkit_contract::runtime::http::{attach_internal_token, map_http_error, parse_retry_after};
use toolkit_contract::runtime::retry::retry_with_backoff;
use toolkit_contract::runtime::transport_error::TransportError;
use toolkit_http::{HttpClient, HttpError, RequestBuilder};

/// The credential of one plane (D17); a request carries exactly one, or none.
pub(super) enum Credential<'a> {
    /// The process's internal token, resolved afresh on every attempt.
    Internal,
    /// The caller's bearer, sent as `Authorization: Bearer`; a caller without one sends
    /// nothing, as a generated client does, and the listener refuses it.
    Bearer(Option<&'a SecretString>),
}

/// A success: its status, the receipt headers and the body.
pub(super) struct Answer {
    pub(super) status: u16,
    pub(super) location: Option<String>,
    pub(super) retry_after: Option<Duration>,
    pub(super) body: bytes::Bytes,
}

/// The HTTP client and the call-time configuration of one REST client.
pub(super) struct Transport {
    http: HttpClient,
    pub(super) config: ClientConfig,
    base_path: &'static str,
}

impl Transport {
    /// Over the default `toolkit-http` transport, as a generated client builds it.
    pub(super) fn new(
        service: &'static str,
        base_path: &'static str,
        config: ClientConfig,
    ) -> Result<Self, HttpError> {
        let http = build_default_http_client(service, &config)?;
        Ok(Self::with_http_client(http, base_path, config))
    }

    pub(super) fn with_http_client(
        http: HttpClient,
        base_path: &'static str,
        config: ClientConfig,
    ) -> Self {
        Self {
            http,
            config,
            base_path,
        }
    }

    pub(super) fn url(&self, path: &str) -> String {
        format!(
            "{}{}{path}",
            self.config.base_url.trim_end_matches('/'),
            self.base_path
        )
    }

    /// One exchange under the retry policy: every attempt is built afresh and carries
    /// `credential`. A non-success status is a `TransportError` carrying the registry's
    /// Problem; a success keeps its status and the headers the caller reads.
    pub(super) async fn exchange(
        &self,
        rpc: &'static str,
        credential: Credential<'_>,
        build: impl Fn(&HttpClient) -> Result<RequestBuilder, TransportError>,
    ) -> Result<Answer, TransportError> {
        retry_with_backoff(&self.config.retry, || async {
            let builder = build(&self.http)?;
            let builder = match credential {
                Credential::Internal => attach_internal_token(
                    builder,
                    self.config.internal_token_provider.as_ref(),
                    rpc,
                ),
                Credential::Bearer(Some(token)) => {
                    use secrecy::ExposeSecret as _;
                    builder.bearer_auth(token.expose_secret())
                }
                Credential::Bearer(None) => builder,
            };
            let attempt = async {
                let response = builder.send().await.map_err(map_send_error)?;
                let status = response.status().as_u16();
                let headers = response.headers();
                let retry_after = parse_retry_after(headers);
                let location = headers
                    .get("location")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let body = response.bytes().await.map_err(TransportError::network)?;
                if (200..300).contains(&status) {
                    Ok(Answer {
                        status,
                        location,
                        retry_after,
                        body,
                    })
                } else {
                    Err(map_http_error(
                        status,
                        String::from_utf8_lossy(&body).into_owned(),
                        retry_after,
                    ))
                }
            };
            tokio::time::timeout(self.config.timeout, attempt)
                .await
                .map_err(|_| TransportError::Timeout(self.config.timeout))?
        })
        .await
    }
}

fn map_send_error(err: HttpError) -> TransportError {
    match err {
        HttpError::Overloaded => TransportError::Overloaded,
        HttpError::Timeout(d) | HttpError::DeadlineExceeded(d) => TransportError::Timeout(d),
        other => TransportError::network(other),
    }
}

/// The span a generated client opens for one method of `$service` under `$base`.
macro_rules! client_span {
    ($service:literal, $base:expr, $method:literal, $http_method:literal, $route:literal) => {
        tracing::info_span!(
            concat!($service, ".", $method),
            otel.kind = "client",
            rpc.system = "rest",
            rpc.service = $service,
            rpc.method = $method,
            http.method = $http_method,
            http.route = concat!($base, $route),
            error = tracing::field::Empty,
        )
    };
}
pub(super) use client_span;

/// Run `call` inside `span`, recording a failure on it as a generated client does.
pub(super) async fn traced<T>(
    span: tracing::Span,
    call: impl Future<Output = Result<T, toolkit_canonical_errors::CanonicalError>>,
) -> Result<T, toolkit_canonical_errors::CanonicalError> {
    use tracing::Instrument as _;
    async {
        let result = call.await;
        if result.is_err() {
            tracing::Span::current().record("error", true);
        }
        result
    }
    .instrument(span)
    .await
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod transport_tests;
