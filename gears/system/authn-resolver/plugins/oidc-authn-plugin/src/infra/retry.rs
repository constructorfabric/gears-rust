//! Shared retry helpers for outbound `IdP` HTTP calls.

use std::error::Error as _;
use std::future::Future;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use http::StatusCode;
use tokio_retry::RetryIf;
use tokio_retry::strategy::{ExponentialBackoff, jitter};
use toolkit_http::{HttpError, ProxyErrorKind};

use tracing::warn;

use crate::config::RetryPolicyConfig;
use crate::domain::error::AuthNError;
use crate::infra::idp_http::IdpResponse;
use crate::infra::url_policy::loggable_url;

/// Terminal failure returned after retry policy handling is exhausted or skipped.
#[derive(Debug)]
pub enum RetriedRequestError {
    /// A non-retryable transport error, or a retryable one after all retries.
    Transport(HttpError),
    /// A non-success HTTP status, retryable or not, after policy handling.
    Status(StatusCode),
}

/// Returns `true` when an HTTP status is retryable under policy.
#[must_use]
pub fn is_retryable_status(status: StatusCode) -> bool {
    status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS
}

/// Returns `true` when a transport error is retryable under policy.
///
/// Only failures to establish a connection are retried (refused or
/// unreachable, DNS, TLS handshake), plus a refused proxy `CONNECT`: the
/// request never left, so a retry cannot send the token POST twice. This is
/// the set reqwest's `is_connect()` covered. A connection lost after the
/// request went out, timeouts (the connect timeout included) and
/// configuration errors are not retried.
#[must_use]
pub fn is_retryable_transport(error: &HttpError) -> bool {
    match error {
        HttpError::Transport(_) => is_connect_error(error) && !has_timed_out_source(error),
        HttpError::Proxy { kind, .. } => matches!(kind, ProxyErrorKind::TunnelRejected),
        _ => false,
    }
}

/// Returns `true` for failures that never reached the `IdP`: the local client
/// refused the request (load shedding, a dead buffer worker) or could not
/// build or route it (bad URL or header, form encoding, proxy credentials
/// rejected). These must not count as an unreachable `IdP`. A bad proxy URL
/// already fails when the client is built; the proxy arm only guards it.
#[must_use]
pub fn is_local_client_failure(error: &HttpError) -> bool {
    match error {
        HttpError::Overloaded
        | HttpError::ServiceClosed
        | HttpError::RequestBuild(_)
        | HttpError::InvalidHeaderName(_)
        | HttpError::InvalidHeaderValue(_)
        | HttpError::FormEncode(_)
        | HttpError::InvalidUri { .. }
        | HttpError::InvalidScheme { .. } => true,
        HttpError::Proxy { kind, .. } => !matches!(kind, ProxyErrorKind::TunnelRejected),
        _ => false,
    }
}

/// Log a failed `IdP` GET (discovery, JWKS) and map it to the domain error.
///
/// A request that never reached the `IdP` is `HttpClientUnavailable`, which
/// the circuit breaker does not count; everything else is `IdpUnreachable`.
#[expect(
    clippy::cognitive_complexity,
    reason = "the score comes from the expansion of three tracing macros"
)]
pub fn idp_get_error(
    error: &RetriedRequestError,
    endpoint: &'static str,
    url: &url::Url,
) -> AuthNError {
    match error {
        RetriedRequestError::Transport(e) if is_local_client_failure(e) => {
            warn!(endpoint, url = %loggable_url(url.as_str()), error = %e, "IdP request not sent by the local HTTP client");
            return AuthNError::HttpClientUnavailable(e.to_string());
        }
        RetriedRequestError::Transport(e) => {
            warn!(endpoint, url = %loggable_url(url.as_str()), error = %e, "IdP request failed");
        }
        RetriedRequestError::Status(status) => {
            warn!(endpoint, url = %loggable_url(url.as_str()), %status, "IdP returned non-success status");
        }
    }
    AuthNError::IdpUnreachable
}

/// Whether hyper-util failed before the request was sent, while connecting.
fn is_connect_error(error: &HttpError) -> bool {
    let mut source = error.source();
    while let Some(err) = source {
        if let Some(client_error) = err.downcast_ref::<hyper_util::client::legacy::Error>() {
            return client_error.is_connect();
        }
        source = err.source();
    }
    false
}

fn has_timed_out_source(error: &HttpError) -> bool {
    let mut source = error.source();
    while let Some(err) = source {
        if let Some(io_error) = err.downcast_ref::<io::Error>()
            && io_error.kind() == io::ErrorKind::TimedOut
        {
            return true;
        }
        source = err.source();
    }
    false
}

/// Parse `Retry-After` delta-seconds or HTTP-date value and cap to `max_backoff`.
#[must_use]
pub fn retry_after_delay(response: &IdpResponse, max_backoff: Duration) -> Option<Duration> {
    let raw = response.headers().get(http::header::RETRY_AFTER)?;
    let raw = raw.to_str().ok()?;
    parse_retry_after_delay(raw, SystemTime::now(), max_backoff)
}

/// Send a request and apply the shared outbound `IdP` retry policy.
///
/// The closure is called once per attempt so callers can rebuild non-cloneable
/// request builders while this helper owns retry classification and sleeping.
pub async fn send_with_retry<F, Fut>(
    policy: &RetryPolicyConfig,
    mut send: F,
) -> Result<IdpResponse, RetriedRequestError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<IdpResponse, HttpError>>,
{
    // Server `Retry-After` (HTTP 429) overrides our computed backoff for the
    // next delay. The action writes the parsed value (ms; `0` = none) here and
    // the strategy consumes it, so the override rides inside tokio-retry instead
    // of a separate bespoke sleep.
    let retry_after_ms = Arc::new(AtomicU64::new(0));

    let jitter_on = policy.jitter;
    let strat_cell = Arc::clone(&retry_after_ms);
    let strategy = ExponentialBackoff::from_millis(policy.backoff_base_ms)
        .factor(policy.backoff_factor)
        .max_delay(policy.max_backoff)
        .map(move |computed| {
            let override_ms = strat_cell.swap(0, Ordering::Relaxed);
            if override_ms > 0 {
                // `Retry-After` is server-authoritative — used verbatim (already
                // capped to `max_backoff`), never jittered.
                Duration::from_millis(override_ms)
            } else if jitter_on {
                jitter(computed)
            } else {
                computed
            }
        })
        .take(policy.max_retries as usize);

    let max_backoff = policy.max_backoff;
    let action_cell = Arc::clone(&retry_after_ms);
    let action = || {
        let cell = Arc::clone(&action_cell);
        let fut = send();
        async move {
            match fut.await {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        return Ok(response);
                    }
                    if status == StatusCode::TOO_MANY_REQUESTS
                        && let Some(delay) = retry_after_delay(&response, max_backoff)
                    {
                        let ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
                        // Clamp to >=1 so `0` stays the "no override" sentinel.
                        cell.store(ms.max(1), Ordering::Relaxed);
                    }
                    Err(RetriedRequestError::Status(status))
                }
                Err(error) => Err(RetriedRequestError::Transport(error)),
            }
        }
    };

    // Classify which failures are worth retrying; everything else short-circuits.
    let retryable = |e: &RetriedRequestError| match e {
        RetriedRequestError::Transport(error) => is_retryable_transport(error),
        RetriedRequestError::Status(status) => is_retryable_status(*status),
    };

    RetryIf::start(strategy, action, retryable).await
}

fn parse_retry_after_delay(raw: &str, now: SystemTime, max_backoff: Duration) -> Option<Duration> {
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(Duration::from_secs(secs).min(max_backoff));
    }

    let retry_at = httpdate::parse_http_date(raw).ok()?;
    let delay = retry_at.duration_since(now).unwrap_or(Duration::ZERO);
    Some(delay.min(max_backoff))
}

/// A loopback TCP endpoint that answers every request with fixed bytes.
#[cfg(test)]
pub mod raw_endpoint {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// The running endpoint.
    pub struct RawEndpoint {
        /// Where it listens.
        pub addr: SocketAddr,
        connections: Arc<AtomicUsize>,
    }

    impl RawEndpoint {
        /// Connections accepted so far.
        pub fn connections(&self) -> usize {
            self.connections.load(Ordering::SeqCst)
        }
    }

    /// Accept connections; on each, read the request head, write `reply`,
    /// then keep the connection open until the client closes it.
    pub async fn spawn(reply: &'static [u8]) -> RawEndpoint {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let connections = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&connections);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0_u8; 1024];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    if stream.write_all(reply).await.is_err() {
                        return;
                    }
                    while matches!(stream.read(&mut buf).await, Ok(n) if n > 0) {}
                });
            }
        });
        RawEndpoint { addr, connections }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transport(kind: io::ErrorKind) -> HttpError {
        HttpError::Transport(Box::new(io::Error::new(kind, "test")))
    }

    #[test]
    fn does_not_retry_timeouts() {
        assert!(!is_retryable_transport(&transport(io::ErrorKind::TimedOut)));
        assert!(!is_retryable_transport(&HttpError::Timeout(
            Duration::from_secs(1)
        )));
        assert!(!is_retryable_transport(&HttpError::DeadlineExceeded(
            Duration::from_secs(1)
        )));
    }

    #[test]
    fn local_client_failures_are_not_retried_and_not_idp_failures() {
        let request_build = http::Request::builder()
            .method("not a method")
            .body(())
            .expect_err("invalid method");
        for error in [
            HttpError::Overloaded,
            HttpError::ServiceClosed,
            HttpError::RequestBuild(request_build),
            HttpError::InvalidScheme {
                scheme: "http".to_owned(),
                reason: "test".to_owned(),
            },
            HttpError::InvalidUri {
                url: "x".to_owned(),
                kind: toolkit_http::InvalidUriKind::ParseError,
                reason: "test".to_owned(),
            },
        ] {
            assert!(is_local_client_failure(&error), "{error:?}");
            assert!(!is_retryable_transport(&error), "{error:?}");
        }
        assert!(!is_local_client_failure(&transport(
            io::ErrorKind::ConnectionRefused
        )));
    }

    #[tokio::test]
    async fn proxy_rejecting_credentials_is_a_local_failure() {
        let proxy =
            raw_endpoint::spawn(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").await;
        let http = crate::infra::idp_http::IdpHttpClient::builder(Duration::from_secs(5))
            .tls_roots(toolkit_http::TlsRootConfig::WebPki)
            .outbound_proxy(toolkit_http::OutboundProxy::Explicit(
                toolkit_http::ExplicitProxy::all(format!("http://{}", proxy.addr)),
            ))
            .build()
            .expect("client should build");
        let client = crate::infra::idp_http::IdpHttpClient::new(http, Duration::from_secs(5));

        let error = client
            .get(&url::Url::parse("https://idp.example.com/").expect("URL should parse"))
            .await
            .expect_err("the proxy answers 407");

        assert!(is_local_client_failure(&error), "{error:?}");
        assert!(!is_retryable_transport(&error), "{error:?}");
    }

    #[tokio::test]
    async fn failure_after_the_request_was_sent_is_not_retried() {
        let endpoint = raw_endpoint::spawn(b"HTTP/1.1 not-a-status\r\n\r\n").await;

        let error = crate::infra::idp_http::IdpHttpClient::insecure_for_tests()
            .get(&url::Url::parse(&format!("http://{}/", endpoint.addr)).expect("URL should parse"))
            .await
            .expect_err("the status line is invalid");

        assert!(matches!(error, HttpError::Transport(_)), "{error:?}");
        assert!(!is_retryable_transport(&error), "{error:?}");
    }

    #[tokio::test]
    async fn real_dns_failure_is_retryable() {
        let error = crate::infra::idp_http::IdpHttpClient::insecure_for_tests()
            .get(&url::Url::parse("http://idp.invalid/").expect("URL should parse"))
            .await
            .expect_err("the .invalid TLD never resolves");

        assert!(is_retryable_transport(&error), "{error:?}");
    }

    #[tokio::test]
    async fn real_connection_refused_is_retryable() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("addr").port()
        };
        let error = crate::infra::idp_http::IdpHttpClient::insecure_for_tests()
            .get(&url::Url::parse(&format!("http://127.0.0.1:{port}/")).expect("URL should parse"))
            .await
            .expect_err("nothing listens on the port");

        assert!(is_retryable_transport(&error), "{error:?}");
    }

    #[test]
    fn does_not_retry_tls_or_body_errors() {
        assert!(!is_retryable_transport(&HttpError::Tls(Box::new(
            io::Error::new(io::ErrorKind::ConnectionReset, "tls")
        ))));
        assert!(!is_retryable_transport(&HttpError::BodyTooLarge {
            limit: 1,
            actual: 2
        }));
    }

    #[test]
    fn parses_retry_after_delta_seconds() {
        let delay = parse_retry_after_delay("42", SystemTime::UNIX_EPOCH, Duration::from_mins(2));

        assert_eq!(delay, Some(Duration::from_secs(42)));
    }

    #[test]
    fn caps_retry_after_delta_seconds() {
        let delay = parse_retry_after_delay("120", SystemTime::UNIX_EPOCH, Duration::from_secs(3));

        assert_eq!(delay, Some(Duration::from_secs(3)));
    }

    #[test]
    fn parses_retry_after_http_date() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let retry_at = now + Duration::from_secs(30);
        let raw = httpdate::fmt_http_date(retry_at);

        let delay = parse_retry_after_delay(&raw, now, Duration::from_mins(2));

        assert_eq!(delay, Some(Duration::from_secs(30)));
    }

    #[test]
    fn caps_retry_after_http_date() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let retry_at = now + Duration::from_secs(30);
        let raw = httpdate::fmt_http_date(retry_at);

        let delay = parse_retry_after_delay(&raw, now, Duration::from_secs(3));

        assert_eq!(delay, Some(Duration::from_secs(3)));
    }

    #[test]
    fn treats_past_retry_after_http_date_as_zero_delay() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let retry_at = now - Duration::from_secs(30);
        let raw = httpdate::fmt_http_date(retry_at);

        let delay = parse_retry_after_delay(&raw, now, Duration::from_mins(2));

        assert_eq!(delay, Some(Duration::ZERO));
    }

    #[test]
    fn rejects_invalid_retry_after_value() {
        let delay =
            parse_retry_after_delay("not-a-date", SystemTime::UNIX_EPOCH, Duration::from_mins(2));

        assert_eq!(delay, None);
    }
}
