//! HTTP client for outbound `IdP` calls: OIDC discovery, JWKS, token endpoint.

use std::time::Duration;

use serde::de::DeserializeOwned;
use tokio::time::Instant;
#[cfg(any(test, feature = "test-util"))]
use toolkit_http::TransportSecurity;
use toolkit_http::{
    HttpClient, HttpClientBuilder, HttpError, HttpResponse, OutboundProxy, RequestBuilder,
    TlsRootConfig,
};

/// Maximum JSON response body accepted from `OIDC`/`OAuth2` endpoints.
///
/// Discovery documents, JWKS sets, and token endpoint responses are small. The
/// cap keeps an `IdP` or intermediary from making the plugin buffer an
/// unbounded body; the client cuts the body off while it streams.
pub const HTTP_RESPONSE_BODY_SIZE_LIMIT_BYTES: usize = 1024 * 1024;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Failure while reading or decoding a bounded HTTP response body.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ResponseBodyError {
    /// Response body exceeded [`HTTP_RESPONSE_BODY_SIZE_LIMIT_BYTES`].
    #[error("HTTP response body exceeded limit of {limit} bytes (read {actual} bytes)")]
    TooLarge {
        /// The configured limit in bytes.
        limit: usize,
        /// Bytes read before the read was cut off.
        actual: usize,
    },
    /// The body did not arrive within the request timeout.
    #[error("HTTP response body not received within {0:?}")]
    Timeout(Duration),
    /// Response body could not be read from the network stream.
    #[error("failed to read HTTP response body: {0}")]
    Read(#[source] HttpError),
    /// Response body was not valid JSON for the requested type.
    #[error("failed to parse HTTP JSON response: {0}")]
    Json(#[from] serde_json::Error),
}

/// Failure to build the `IdP` HTTP client from the plugin settings.
#[derive(Debug, thiserror::Error)]
pub enum IdpHttpClientError {
    /// A configured CA bundle could not be read.
    #[error("failed to read custom CA certificate bundle {path:?}: {source}")]
    ReadBundle {
        /// The configured bundle path.
        path: String,
        /// Why the file could not be read.
        #[source]
        source: std::io::Error,
    },
    /// A configured CA bundle holds no usable PEM certificate.
    #[error("failed to parse custom CA certificate bundle {path:?}: {source}")]
    ParseBundle {
        /// The configured bundle path.
        path: String,
        /// Why the bundle was rejected.
        #[source]
        source: HttpError,
    },
    /// The client could not be built; certificates with invalid DER only
    /// fail here, so the bundles are named.
    #[error("failed to build HTTP client (custom CA bundles: {bundles:?}): {source}")]
    Build {
        /// All configured bundle paths.
        bundles: Vec<String>,
        /// Why the client could not be built.
        #[source]
        source: HttpError,
    },
}

/// A response whose body must arrive by `deadline`: the request timeout,
/// counted from when the request was sent.
pub(crate) struct IdpResponse {
    response: HttpResponse,
    deadline: Instant,
}

impl std::fmt::Debug for IdpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdpResponse")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl IdpResponse {
    /// The response status.
    pub(crate) fn status(&self) -> http::StatusCode {
        self.response.status()
    }

    /// The response headers.
    pub(crate) fn headers(&self) -> &http::HeaderMap {
        self.response.headers()
    }
}

/// [`HttpClient`] plus the request timeout. toolkit-http applies that timeout
/// only until the response headers arrive; here one deadline, set when the
/// request is sent, covers the headers and [`read_json`](Self::read_json).
#[derive(Clone)]
pub struct IdpHttpClient {
    http: HttpClient,
    request_timeout: Duration,
}

impl std::fmt::Debug for IdpHttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdpHttpClient")
            .field("request_timeout", &self.request_timeout)
            .finish_non_exhaustive()
    }
}

impl IdpHttpClient {
    /// Builder with the production settings for `IdP` calls.
    ///
    /// - No client-level retries: `send_with_retry` owns retries, and a second
    ///   layer would multiply attempts.
    /// - The OS trust store and `HTTPS_PROXY` / `NO_PROXY`, as reqwest used.
    /// - No concurrency cap: a locally shed request would be reported as an
    ///   unreachable `IdP` and count against that host's circuit breaker.
    /// - Redirects stay on the same origin (toolkit-http default), so the
    ///   token request's client secret is never re-posted to another host.
    /// - A client span per request and a `traceparent` header (when the host
    ///   enables toolkit-http's `otel` feature).
    #[must_use]
    pub fn builder(request_timeout: Duration) -> HttpClientBuilder {
        HttpClient::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(request_timeout)
            .retry(None)
            .concurrency_limit(None)
            .max_body_size(HTTP_RESPONSE_BODY_SIZE_LIMIT_BYTES)
            .tls_roots(TlsRootConfig::Native)
            .outbound_proxy(OutboundProxy::FromEnv)
            .with_otel()
    }

    /// The production client: [`builder`](Self::builder) plus the configured
    /// custom CA bundles (PEM files, added to the OS trust store).
    ///
    /// # Errors
    ///
    /// Returns [`IdpHttpClientError`] when a bundle cannot be read or holds no
    /// usable certificate, or the client cannot be built.
    pub fn from_settings(
        request_timeout: Duration,
        custom_ca_certificate_paths: &[String],
    ) -> Result<Self, IdpHttpClientError> {
        let mut builder = Self::builder(request_timeout);
        for path in custom_ca_certificate_paths {
            let pem_bundle =
                std::fs::read(path).map_err(|source| IdpHttpClientError::ReadBundle {
                    path: path.clone(),
                    source,
                })?;
            builder = builder
                .add_root_certificates_pem(&pem_bundle)
                .map_err(|source| IdpHttpClientError::ParseBundle {
                    path: path.clone(),
                    source,
                })?;
        }
        let http = builder
            .build()
            .map_err(|source| IdpHttpClientError::Build {
                bundles: custom_ca_certificate_paths.to_vec(),
                source,
            })?;
        Ok(Self::new(http, request_timeout))
    }

    /// Wrap a client built from [`builder`](Self::builder) with the same timeout.
    #[must_use]
    pub fn new(http: HttpClient, request_timeout: Duration) -> Self {
        Self {
            http,
            request_timeout,
        }
    }

    /// Production settings with plain HTTP allowed and the bundled web PKI
    /// roots (tests must not depend on the OS store), for `httpmock` servers.
    ///
    /// # Panics
    ///
    /// Panics if the client cannot be built.
    #[cfg(any(test, feature = "test-util"))]
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "test helper; the settings are fixed and build only fails on a broken TLS backend"
    )]
    pub fn insecure_for_tests_with_timeout(request_timeout: Duration) -> Self {
        let http = Self::builder(request_timeout)
            .transport(TransportSecurity::AllowInsecureHttp)
            .tls_roots(TlsRootConfig::WebPki)
            .outbound_proxy(OutboundProxy::None)
            .build()
            .expect("test HTTP client should build");
        Self::new(http, request_timeout)
    }

    /// [`insecure_for_tests_with_timeout`](Self::insecure_for_tests_with_timeout)
    /// with a 5 second timeout.
    #[cfg(any(test, feature = "test-util"))]
    #[must_use]
    pub fn insecure_for_tests() -> Self {
        Self::insecure_for_tests_with_timeout(Duration::from_secs(5))
    }

    /// Send a GET request to `url`.
    pub(crate) async fn get(&self, url: &url::Url) -> Result<IdpResponse, HttpError> {
        self.send(self.http.get(url.as_str())).await
    }

    /// Send a POST request to `url` with a URL-encoded form body.
    pub(crate) async fn post_form(
        &self,
        url: &url::Url,
        form: &[(&str, &str)],
    ) -> Result<IdpResponse, HttpError> {
        let request = self.http.post(url.as_str()).form(form)?;
        self.send(request).await
    }

    async fn send(&self, request: RequestBuilder) -> Result<IdpResponse, HttpError> {
        let deadline = Instant::now() + self.request_timeout;
        let response = request.send().await?;
        Ok(IdpResponse { response, deadline })
    }

    /// Read a successful response as JSON, by the deadline set when it was
    /// sent and within the body size limit.
    pub(crate) async fn read_json<T>(&self, response: IdpResponse) -> Result<T, ResponseBodyError>
    where
        T: DeserializeOwned,
    {
        let body = tokio::time::timeout_at(response.deadline, response.response.bytes())
            .await
            .map_err(|_| ResponseBodyError::Timeout(self.request_timeout))?
            .map_err(|error| match error {
                HttpError::BodyTooLarge { limit, actual } => {
                    ResponseBodyError::TooLarge { limit, actual }
                }
                other => ResponseBodyError::Read(other),
            })?;
        Ok(serde_json::from_slice(&body)?)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use httpmock::prelude::{GET, MockServer};
    use serde::Deserialize;
    use tokio::io::AsyncWriteExt;

    use super::*;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct TestPayload {
        value: String,
    }

    #[tokio::test]
    async fn read_json_parses_json_under_limit() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/json");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"value":"ok"}"#);
        });
        let client = IdpHttpClient::insecure_for_tests();
        let url = url::Url::parse(&server.url("/json")).expect("mock URL should parse");

        let response = client.get(&url).await.expect("mock request should succeed");
        let parsed: TestPayload = client
            .read_json(response)
            .await
            .expect("bounded JSON body should parse");

        assert_eq!(
            parsed,
            TestPayload {
                value: "ok".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn read_json_rejects_body_over_production_limit() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/too-large");
            then.status(200)
                .header("content-type", "application/json")
                .body("x".repeat(HTTP_RESPONSE_BODY_SIZE_LIMIT_BYTES + 1));
        });
        let client = IdpHttpClient::insecure_for_tests();
        let url = url::Url::parse(&server.url("/too-large")).expect("mock URL should parse");

        let response = client.get(&url).await.expect("mock request should succeed");
        let error = client
            .read_json::<TestPayload>(response)
            .await
            .expect_err("oversized body should be rejected");

        assert!(
            matches!(
                error,
                ResponseBodyError::TooLarge {
                    limit: HTTP_RESPONSE_BODY_SIZE_LIMIT_BYTES,
                    actual
                } if actual > HTTP_RESPONSE_BODY_SIZE_LIMIT_BYTES
            ),
            "expected TooLarge, got: {error:?}"
        );
    }

    /// Retries belong to `send_with_retry`; the client itself must send once.
    #[tokio::test]
    async fn client_does_not_retry_on_its_own() {
        let server = MockServer::start();
        let unavailable = server.mock(|when, then| {
            when.method(GET).path("/discovery");
            then.status(503);
        });
        let client = IdpHttpClient::insecure_for_tests();
        let url = url::Url::parse(&server.url("/discovery")).expect("mock URL should parse");

        let response = client.get(&url).await.expect("503 is a response");

        assert_eq!(response.status(), http::StatusCode::SERVICE_UNAVAILABLE);
        unavailable.assert_calls(1);
    }

    /// Headers arrive, the body never does: the request timeout must still
    /// end the call (toolkit-http's own timeout stops at the headers).
    #[tokio::test]
    async fn read_json_times_out_on_stalled_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = [0_u8; 1024];
            drop(tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await);
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\n\r\n{",
                )
                .await
                .expect("write headers");
            std::future::pending::<()>().await;
        });
        let timeout = Duration::from_millis(200);
        let client = IdpHttpClient::insecure_for_tests_with_timeout(timeout);
        let url = url::Url::parse(&format!("http://{addr}/jwks")).expect("URL should parse");

        let started = Instant::now();
        let response = client.get(&url).await.expect("headers should arrive");
        let error = client
            .read_json::<TestPayload>(response)
            .await
            .expect_err("stalled body should time out");
        server.abort();

        assert!(
            matches!(error, ResponseBodyError::Timeout(t) if t == timeout),
            "{error:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// Slow headers and a stalled body share one request timeout: the call
    /// ends at the deadline set when the request was sent, not a full timeout
    /// after the headers.
    #[tokio::test]
    async fn one_deadline_covers_headers_and_body() {
        const TIMEOUT: Duration = Duration::from_millis(400);
        const HEADERS_AFTER: Duration = Duration::from_millis(300);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = [0_u8; 1024];
            drop(tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await);
            tokio::time::sleep(HEADERS_AFTER).await;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\n\r\n{",
                )
                .await
                .expect("write headers");
            std::future::pending::<()>().await;
        });
        let client = IdpHttpClient::insecure_for_tests_with_timeout(TIMEOUT);
        let url = url::Url::parse(&format!("http://{addr}/jwks")).expect("URL should parse");

        let started = Instant::now();
        let response = client.get(&url).await.expect("headers arrive in time");
        let error = client
            .read_json::<TestPayload>(response)
            .await
            .expect_err("stalled body should time out");
        let elapsed = started.elapsed();
        server.abort();

        assert!(matches!(error, ResponseBodyError::Timeout(_)), "{error:?}");
        assert!(
            elapsed < TIMEOUT + Duration::from_millis(200),
            "the body got its own full timeout: {elapsed:?}"
        );
    }

    fn write_temp_bundle(name: &str, contents: &[u8]) -> String {
        let path = std::env::temp_dir().join(format!(
            "oidc-authn-plugin-{name}-{}.pem",
            std::process::id()
        ));
        std::fs::write(&path, contents).expect("temp CA bundle should be written");
        path.to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn from_settings_reports_missing_custom_ca_bundle() {
        let missing = std::env::temp_dir()
            .join(format!(
                "oidc-authn-plugin-missing-ca-dir-{}",
                std::process::id()
            ))
            .join("custom-root-ca.pem")
            .to_string_lossy()
            .into_owned();

        let error =
            IdpHttpClient::from_settings(Duration::from_secs(5), std::slice::from_ref(&missing))
                .expect_err("missing custom CA bundle should fail client construction");

        assert!(
            matches!(&error, IdpHttpClientError::ReadBundle { path, .. } if *path == missing),
            "{error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("failed to read custom CA certificate bundle"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn from_settings_rejects_bundle_without_certificates() {
        let path = write_temp_bundle("empty-ca", b"not a certificate\n");
        let error =
            IdpHttpClient::from_settings(Duration::from_secs(5), std::slice::from_ref(&path))
                .expect_err("a bundle without certificates should fail client construction");
        std::fs::remove_file(&path).ok();

        assert!(
            matches!(&error, IdpHttpClientError::ParseBundle { path: p, .. } if *p == path),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn from_settings_accepts_valid_custom_ca_bundle() {
        let ca = rcgen::generate_simple_self_signed(vec!["idp.internal".to_owned()])
            .expect("test certificate should be generated");
        let path = write_temp_bundle("valid-ca", ca.cert.pem().as_bytes());
        let result =
            IdpHttpClient::from_settings(Duration::from_secs(5), std::slice::from_ref(&path));
        std::fs::remove_file(&path).ok();

        assert!(
            result.is_ok(),
            "valid bundle should build a client: {result:?}"
        );
    }
}
