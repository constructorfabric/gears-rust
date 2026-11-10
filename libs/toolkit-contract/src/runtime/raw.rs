//! Raw HTTP passthrough for manual REST methods (ADR-0009 §4).
//!
//! [`ManualCallContext::send_raw`] forwards an opaque request to the configured
//! provider and returns the response head and an opaque streaming body. It
//! runs on `toolkit-http-proxy`'s engine, which streams both directions with
//! bounded buffers, never decompresses, never follows redirects and cancels
//! the exchange when the body is dropped. On top of it this module pins the
//! destination, replaces credentials with the method's security plane,
//! propagates trace context, applies the client's TLS settings and
//! `max_concurrent_requests` budget, bounds the open and records the client
//! metric. It never retries: the engine is configured to resend only requests
//! that provably never reached the provider, and moves to the endpoint's next
//! address only when a connect failed.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use http::header::{AUTHORIZATION, CONNECTION, HOST, HeaderName, HeaderValue, UPGRADE};
use http::{Method, Request, Response, Uri};
use http_body_util::BodyExt;
use toolkit_http_proxy::balance::{DnsResolver, Resolver};
use toolkit_http_proxy::{
    ErrorKind, ProxyClient, ProxyClientConfig, ProxyError, RootStore, Target, Transport,
};

use crate::ir::binding::{HttpFieldBinding, HttpMethod};
use crate::runtime::config::ClientConfig;
use crate::runtime::manual::ManualCallContext;
use crate::runtime::transport_error::TransportError;

pub use toolkit_http_proxy::{BoxError, empty_body, full_body, stream_body};

/// Request and response body of a raw call: `Send` but not `Sync`, streamed
/// with backpressure. Build one with [`empty_body`], [`full_body`] or
/// [`stream_body`].
pub type RawBody = toolkit_http_proxy::ProxyBody;

const INTERNAL_TOKEN: HeaderName =
    HeaderName::from_static(toolkit_security::constants::INTERNAL_TOKEN_HEADER);

/// How long resolved addresses are reused when `pool_idle_timeout` is unset.
const DEFAULT_ADDRESS_TTL: Duration = Duration::from_secs(90);

/// The engine behind raw calls, built on first use per generated client.
pub(crate) struct RawTransport {
    client: ProxyClient,
    resolver: DnsResolver,
    /// Last resolution of the endpoint host. Like a pooled connection, it is
    /// reused until it ages out or none of its addresses accepts a connect.
    addresses: parking_lot::Mutex<Option<Resolved>>,
    address_ttl: Duration,
    #[cfg(feature = "otel")]
    metrics: toolkit_http::RequestDurationRecorder,
}

struct Resolved {
    host: String,
    port: u16,
    addrs: Vec<SocketAddr>,
    at: Instant,
}

/// The engine configuration a client config asks for.
fn engine_config(config: &ClientConfig) -> Result<ProxyClientConfig, TransportError> {
    let mut cfg = ProxyClientConfig::default();
    // ADR-0009 §4: no replay of a request the provider may have processed.
    cfg.resend_idempotent = false;
    // `timeout` bounds opening the exchange: name resolution in `send_raw`,
    // and here each TCP connect and TLS handshake. The response head and each
    // body frame get `stream_idle_timeout`; a stalled upload trips the engine's
    // `write_timeout`. A request body may therefore take longer than `timeout`
    // to upload (ADR-0009 §4: open and body deadlines are separate).
    cfg.connect_timeout = config.timeout;
    cfg.read_timeout = Some(config.stream_idle_timeout);
    cfg.pool.max_idle_per_host = config.pool_max_idle_per_host;
    if let Some(idle) = config.pool_idle_timeout {
        cfg.pool.idle_timeout = idle;
    }
    // The client's TLS settings, as `build_default_http_client` applies them.
    cfg.tls.roots = match config.tls_roots {
        toolkit_http::TlsRootConfig::WebPki => RootStore::WebPki,
        toolkit_http::TlsRootConfig::Native => RootStore::Native,
        other => {
            return Err(TransportError::network(format!(
                "TLS roots {other:?} are not supported by raw calls"
            )));
        }
    };
    cfg.tls.settings = config.tls.clone();
    Ok(cfg)
}

impl RawTransport {
    fn new(config: &ClientConfig, client_type: &str) -> Result<Self, TransportError> {
        let client = ProxyClient::new(engine_config(config)?).map_err(TransportError::network)?;
        #[cfg(not(feature = "otel"))]
        let _ = client_type;
        Ok(Self {
            client,
            resolver: DnsResolver::default(),
            addresses: parking_lot::Mutex::new(None),
            address_ttl: config.pool_idle_timeout.unwrap_or(DEFAULT_ADDRESS_TTL),
            #[cfg(feature = "otel")]
            metrics: toolkit_http::RequestDurationRecorder::new(client_type),
        })
    }

    /// One target per address of `host:port`, resolving only when the cached
    /// resolution is missing, stale or for another endpoint.
    async fn targets(
        &self,
        host: &str,
        port: u16,
        transport: Transport,
    ) -> Result<Vec<Target>, TransportError> {
        let cached = self
            .addresses
            .lock()
            .as_ref()
            .filter(|r| r.host == host && r.port == port && r.at.elapsed() < self.address_ttl)
            .map(|r| r.addrs.clone());
        let addrs = if let Some(addrs) = cached {
            addrs
        } else {
            let addrs = self
                .resolver
                .resolve(host, port)
                .await
                .map_err(TransportError::network)?;
            if addrs.is_empty() {
                return Err(TransportError::network(format!("no address for {host}")));
            }
            *self.addresses.lock() = Some(Resolved {
                host: host.to_owned(),
                port,
                addrs: addrs.clone(),
                at: Instant::now(),
            });
            addrs
        };
        let server_name = host.trim_start_matches('[').trim_end_matches(']');
        Ok(addrs
            .into_iter()
            .map(|addr| Target::new(addr, server_name, transport).with_authority_port(port))
            .collect())
    }

    /// Resolve again on the next call: no address accepted a connect.
    fn forget_addresses(&self) {
        *self.addresses.lock() = None;
    }
}

impl ManualCallContext<'_> {
    /// Forward `req` to this passthrough method's endpoint.
    ///
    /// The request's path and query fill the binding's trailing `{*param}`
    /// wildcard; its scheme, authority and `Host` are ignored, so a caller
    /// cannot redirect the call or its credentials. Caller `Authorization` and
    /// `X-ToolKit-Internal-Token` headers are replaced by the method's
    /// security-plane credential. Non-2xx responses are returned as is; a
    /// failure after the response head surfaces as an error from the body.
    /// Response trailers are kept; request trailers reach an HTTP/2 provider
    /// only, because the engine never forwards a request `Trailer` declaration
    /// and HTTP/1.1 sends no undeclared trailers.
    ///
    /// # Errors
    /// - [`TransportError::InvalidRequest`] for a non-passthrough method, an
    ///   upgrade or `CONNECT` request, a method outside the binding's set, a
    ///   dot segment in the path, or a credential that is not a header value;
    /// - [`TransportError::UrlBuild`] for an unusable endpoint;
    /// - [`TransportError::Timeout`] when name resolution exceeds the client's
    ///   `timeout`, or no response head arrives within `stream_idle_timeout`
    ///   (which also covers the upload of the request body);
    /// - [`TransportError::Network`] for connect, TLS and protocol failures.
    pub async fn send_raw(
        &self,
        req: Request<RawBody>,
    ) -> Result<Response<RawBody>, TransportError> {
        let (methods, param) = self.passthrough_spec()?;
        let (mut parts, body) = req.into_parts();
        check_request(&parts, methods)?;

        let uri = self.destination(&parts.uri, param)?;
        let (host, port, transport) = self.endpoint_parts(&uri)?;
        let raw = self.raw_transport()?;
        parts.uri = origin_form(&uri)?;

        unnominate_owned(&mut parts.headers);
        // The target's authority is the configured endpoint; nothing the
        // caller sent may override it or the credentials.
        parts.headers.remove(HOST);
        parts.headers.remove(AUTHORIZATION);
        parts.headers.remove(INTERNAL_TOKEN);
        self.attach_credentials(&mut parts.headers)?;
        toolkit_http::otel::inject_current_span(&mut parts.headers);
        toolkit_trace_context::inject_request_id(&mut parts.headers);

        let method = parts.method.clone();
        // The generated span declares `http.method` before the verb is known.
        tracing::Span::current().record("http.method", method.as_str());
        let started = Instant::now();
        // The client's `max_concurrent_requests` budget, held until the
        // response head arrives, as `toolkit-http` holds it.
        let result = match self.acquire_slot() {
            Err(overloaded) => Err(overloaded),
            Ok(slot) => {
                let req = Request::from_parts(parts, body);
                let opened = async {
                    let targets = tokio::time::timeout(
                        self.config().timeout,
                        raw.targets(&host, port, transport),
                    )
                    .await
                    .unwrap_or(Err(TransportError::Timeout(self.config().timeout)))?;
                    raw.client.send_any(&targets, req).await.map_err(|e| {
                        if is_connect_failure(&e) {
                            raw.forget_addresses();
                        }
                        map_proxy_error(e, self.config())
                    })
                }
                .await;
                drop(slot);
                opened
            }
        };
        // Recorded for shed calls too, as `toolkit-http` records them.
        self.record(raw, &method, &uri, started, &result);
        if result.is_err() {
            tracing::Span::current().record("error", true);
        }
        Ok(result?.map(|body| body.map_err(BoxError::from).boxed_unsync()))
    }

    fn passthrough_spec(&self) -> Result<(&[HttpMethod], &str), TransportError> {
        let binding = self.method_binding();
        binding
            .field_bindings
            .iter()
            .find_map(|fb| match fb {
                HttpFieldBinding::Passthrough { param, methods, .. } => {
                    Some((methods.as_slice(), param.as_str()))
                }
                _ => None,
            })
            .ok_or_else(|| {
                TransportError::InvalidRequest(format!(
                    "method '{}' is not a passthrough binding",
                    binding.method_name
                ))
            })
    }

    /// `base_url + base_path + template prefix + request path and query`.
    fn destination(&self, requested: &Uri, param: &str) -> Result<Uri, TransportError> {
        let template = &self.method_binding().path_template;
        let prefix = template
            .strip_suffix(&format!("{{*{param}}}"))
            .ok_or_else(|| {
                TransportError::UrlBuild(format!(
                    "path template '{template}' does not end with '{{*{param}}}'"
                ))
            })?;
        let path = requested.path();
        if path
            .split('/')
            .map(|seg| percent_encoding::percent_decode_str(seg).decode_utf8_lossy())
            .any(|seg| seg == "." || seg == "..")
        {
            return Err(TransportError::InvalidRequest(format!(
                "request path '{path}' has a dot segment"
            )));
        }
        let mut url = format!(
            "{}{}{}{}",
            self.endpoint().trim_end_matches('/'),
            self.binding().base_path.trim_end_matches('/'),
            prefix.trim_end_matches('/'),
            if path.starts_with('/') { "" } else { "/" },
        );
        url.push_str(path);
        if let Some(query) = requested.query() {
            url.push('?');
            url.push_str(query);
        }
        url.parse::<Uri>()
            .map_err(|e| TransportError::UrlBuild(format!("invalid passthrough URL: {e}")))
    }

    /// Host, port and transport of the destination; plaintext only when the
    /// client's config allows it, as for its `toolkit-http` transport.
    fn endpoint_parts(&self, uri: &Uri) -> Result<(String, u16, Transport), TransportError> {
        let transport = match uri.scheme_str() {
            Some("https") => Transport::Tls,
            Some("http") if !self.config().require_tls => Transport::Plain,
            Some("http") => {
                return Err(TransportError::UrlBuild(
                    "plaintext http:// endpoint while TLS is required".to_owned(),
                ));
            }
            other => {
                return Err(TransportError::UrlBuild(format!(
                    "unsupported endpoint scheme {other:?}"
                )));
            }
        };
        let host = uri
            .host()
            .ok_or_else(|| TransportError::UrlBuild("endpoint has no host".to_owned()))?;
        let port = uri
            .port_u16()
            .unwrap_or(if transport == Transport::Tls { 443 } else { 80 });
        Ok((host.to_owned(), port, transport))
    }

    fn acquire_slot(&self) -> Result<Option<toolkit_http::ConcurrencySlot>, TransportError> {
        self.transport()
            .limiter
            .as_ref()
            .map(|limiter| {
                limiter
                    .try_acquire()
                    .map_err(|_| TransportError::Overloaded)
            })
            .transpose()
    }

    fn raw_transport(&self) -> Result<&RawTransport, TransportError> {
        let cell = &self.transport().raw;
        if let Some(raw) = cell.get() {
            return Ok(raw);
        }
        let raw = RawTransport::new(self.config(), self.transport().client_type)?;
        Ok(cell.get_or_init(|| raw))
    }

    fn attach_credentials(&self, headers: &mut http::HeaderMap) -> Result<(), TransportError> {
        let (bearer, provider) = self.credentials();
        if let Some(token) = bearer {
            let mut value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
                TransportError::InvalidRequest(
                    "bearer token is not a valid header value".to_owned(),
                )
            })?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        if let Some(token) = crate::runtime::config::InternalTokenProvider::resolve_for_attach(
            provider,
            self.method_name(),
        ) {
            let mut value = {
                use secrecy::ExposeSecret as _;
                HeaderValue::from_str(token.expose_secret())
            }
            .map_err(|_| {
                TransportError::InvalidRequest(
                    "internal token is not a valid header value".to_owned(),
                )
            })?;
            value.set_sensitive(true);
            headers.insert(INTERNAL_TOKEN, value);
        }
        Ok(())
    }

    #[cfg(feature = "otel")]
    fn record(
        &self,
        raw: &RawTransport,
        method: &Method,
        uri: &Uri,
        started: Instant,
        result: &Result<Response<toolkit_http_proxy::ResponseBody>, TransportError>,
    ) {
        let outcome = match result {
            Ok(resp) => toolkit_http::RequestOutcome::Status(resp.status().as_u16()),
            Err(TransportError::Timeout(_)) => toolkit_http::RequestOutcome::Error("timeout"),
            Err(TransportError::Overloaded) => toolkit_http::RequestOutcome::Error("overloaded"),
            Err(_) => toolkit_http::RequestOutcome::Error("transport"),
        };
        raw.metrics.record(
            started.elapsed(),
            toolkit_http::RequestLabels {
                method,
                route: &self.method_binding().path_template,
                uri,
                request_type: None,
            },
            outcome,
        );
    }

    #[cfg(not(feature = "otel"))]
    #[allow(clippy::unused_self, reason = "same signature as the `otel` build")]
    fn record(
        &self,
        _raw: &RawTransport,
        _method: &Method,
        _uri: &Uri,
        _started: Instant,
        _result: &Result<Response<toolkit_http_proxy::ResponseBody>, TransportError>,
    ) {
    }
}

/// Refuse what cannot be forwarded faithfully as a single exchange.
fn check_request(
    parts: &http::request::Parts,
    methods: &[HttpMethod],
) -> Result<(), TransportError> {
    if parts.method == Method::CONNECT {
        return Err(TransportError::InvalidRequest(
            "CONNECT cannot be forwarded".to_owned(),
        ));
    }
    let upgrade_token = parts
        .headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case("upgrade"));
    if upgrade_token || parts.headers.contains_key(UPGRADE) {
        return Err(TransportError::InvalidRequest(
            "protocol upgrades are not supported by raw calls".to_owned(),
        ));
    }
    let allowed = methods
        .iter()
        .filter_map(|m| crate::runtime::http::to_http_method(*m))
        .any(|m| m == parts.method);
    if allowed {
        Ok(())
    } else {
        Err(TransportError::InvalidRequest(format!(
            "method {} is not allowed by this passthrough binding",
            parts.method
        )))
    }
}

fn origin_form(uri: &Uri) -> Result<Uri, TransportError> {
    let path_and_query = uri
        .path_and_query()
        .map_or("/", http::uri::PathAndQuery::as_str);
    path_and_query
        .parse()
        .map_err(|e| TransportError::UrlBuild(format!("invalid passthrough path: {e}")))
}

/// Headers this transport sets after the caller's headers are in.
const OWNED_HEADERS: [&str; 6] = [
    "host",
    "authorization",
    toolkit_security::constants::INTERNAL_TOKEN_HEADER,
    "traceparent",
    "tracestate",
    "x-request-id",
];

/// Remove the transport's own headers from the caller's `Connection` list:
/// the engine strips every header `Connection` nominates, which would drop
/// the credential and trace context attached after it. Other nominations
/// stay, so the engine still filters them from headers and trailers.
fn unnominate_owned(headers: &mut http::HeaderMap) {
    let kept: Vec<String> = headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|token| {
            !token.is_empty() && !OWNED_HEADERS.iter().any(|h| token.eq_ignore_ascii_case(h))
        })
        .map(str::to_owned)
        .collect();
    headers.remove(CONNECTION);
    if let Ok(value) = HeaderValue::from_str(&kept.join(", "))
        && !kept.is_empty()
    {
        headers.insert(CONNECTION, value);
    }
}

/// A TCP connect failure: nothing reached the provider.
fn is_connect_failure(err: &ProxyError) -> bool {
    matches!(
        err.kind(),
        ErrorKind::ConnectTimeout
            | ErrorKind::ConnectRefused
            | ErrorKind::ConnectNoRoute
            | ErrorKind::Connect
    )
}

fn map_proxy_error(err: ProxyError, config: &ClientConfig) -> TransportError {
    match err.kind() {
        // No response head within the per-frame read budget.
        ErrorKind::ReadTimeout => TransportError::Timeout(config.stream_idle_timeout),
        ErrorKind::InvalidRequest => TransportError::InvalidRequest(err.message().to_owned()),
        _ => TransportError::network(err),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn parts(req: Request<()>) -> http::request::Parts {
        req.into_parts().0
    }

    const ALL: &[HttpMethod] = &[HttpMethod::Get, HttpMethod::Post];

    #[test]
    fn rejects_connect_and_upgrades() {
        let connect = parts(Request::connect("example.com:443").body(()).unwrap());
        assert!(check_request(&connect, ALL).is_err());

        let upgrade = parts(
            Request::get("/ws")
                .header("connection", "keep-alive, Upgrade")
                .body(())
                .unwrap(),
        );
        assert!(check_request(&upgrade, ALL).is_err());

        let bare_upgrade = parts(
            Request::get("/ws")
                .header("upgrade", "h2c")
                .body(())
                .unwrap(),
        );
        assert!(check_request(&bare_upgrade, ALL).is_err());
    }

    #[test]
    fn enforces_the_method_set() {
        let get = parts(Request::get("/a").body(()).unwrap());
        check_request(&get, ALL).unwrap();
        let delete = parts(Request::delete("/a").body(()).unwrap());
        let err = check_request(&delete, ALL).unwrap_err();
        assert!(err.to_string().contains("DELETE"), "{err}");
    }

    #[test]
    fn engine_uses_the_configured_tls_settings() {
        let mut tls = toolkit_http::TlsConfig::default();
        tls.min_version = toolkit_http::TlsVersion::Tls13;
        let config = ClientConfig::new("https://provider")
            .with_tls_roots(toolkit_http::TlsRootConfig::Native)
            .with_tls(tls.clone());
        let engine = engine_config(&config).unwrap();
        assert_eq!(engine.tls.settings, tls);
        assert!(matches!(engine.tls.roots, RootStore::Native));
        assert!(!engine.resend_idempotent);
    }

    #[test]
    fn connection_keeps_nominations_except_the_transport_headers() {
        let mut headers = http::HeaderMap::new();
        headers.append(CONNECTION, "Authorization, x-private".parse().unwrap());
        headers.append(
            CONNECTION,
            "X-ToolKit-Internal-Token, traceparent".parse().unwrap(),
        );
        unnominate_owned(&mut headers);
        assert_eq!(headers[CONNECTION], "x-private");

        let mut only_owned = http::HeaderMap::new();
        only_owned.insert(CONNECTION, "authorization".parse().unwrap());
        unnominate_owned(&mut only_owned);
        assert!(!only_owned.contains_key(CONNECTION));
    }

    #[test]
    fn origin_form_drops_scheme_and_authority() {
        let uri: Uri = "http://evil.example/a/b?x=1".parse().unwrap();
        assert_eq!(origin_form(&uri).unwrap().to_string(), "/a/b?x=1");
    }
}
