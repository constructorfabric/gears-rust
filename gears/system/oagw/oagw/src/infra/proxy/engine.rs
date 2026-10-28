//! OAGW adapter over the `toolkit-http-proxy` upstream engine (ADR-0019).
//!
//! Turns a selected [`Endpoint`] into an engine [`Target`] (resolving and
//! SSRF-filtering the address when the load balancer did not), maps engine
//! failures onto [`DomainError`] and converts bodies between the engine and
//! the OAGW SDK.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::TryStreamExt as _;
use http_body_util::BodyExt as _;
use oagw_sdk::body::{BodyStream, BoxError};
use oagw_sdk::field;
use toolkit_http_proxy::balance::{AddressFilter, DnsResolver, Resolver as _};
use toolkit_http_proxy::{
    ConnectionInfo, ErrorKind, Observer, ProtocolPolicy, ProxyClient, ProxyClientConfig,
    ProxyError, ResponseBody, Target, Transport,
};
use tracing::{debug, warn};

use crate::domain::error::DomainError;
use crate::domain::model::{Endpoint, Scheme};
use crate::domain::services::SelectedEndpoint;
use crate::domain::ssrf::SsrfGuard;

/// Idle pooled connections are closed after this long.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// Idle HTTP/1.1 connections kept per upstream host.
const MAX_IDLE_PER_HOST: usize = 128;

/// SSRF policy as an engine address filter.
pub(crate) struct SsrfFilter(
    /// Decides which resolved addresses are blocked.
    pub(crate) Arc<SsrfGuard>,
);

impl AddressFilter for SsrfFilter {
    fn check<'a>(&'a self, host: &str, addr: &SocketAddr) -> Result<(), &'a str> {
        if self.0.is_resolved_ip_blocked(host, addr.ip()) {
            Err(self.0.resolved_block_reason(host, addr.ip()))
        } else {
            Ok(())
        }
    }
}

/// Why the fallback resolution found no address to connect to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NoPermittedAddr {
    /// DNS answered with no addresses at all.
    NoneResolved,
    /// Every resolved address was refused by the SSRF policy; the reason given
    /// for the last one.
    Refused(String),
}

/// The first of `addrs` the SSRF guard permits for `host`, logging each one it
/// refuses. The fallback path of [`ProxyEngine::target`], which resolves
/// outside the load balancer.
pub(crate) fn first_permitted_addr(
    guard: &SsrfGuard,
    host: &str,
    addrs: impl IntoIterator<Item = SocketAddr>,
) -> Result<SocketAddr, NoPermittedAddr> {
    let mut refused = None;
    for addr in addrs {
        if !guard.is_resolved_ip_blocked(host, addr.ip()) {
            return Ok(addr);
        }
        let reason = guard.resolved_block_reason(host, addr.ip());
        warn!(
            host = %host,
            resolved_ip = %addr.ip(),
            reason,
            "SSRF protection: blocked DNS-resolved private IP"
        );
        refused = Some(reason.to_owned());
    }
    Err(refused.map_or(NoPermittedAddr::NoneResolved, NoPermittedAddr::Refused))
}

/// Engine settings derived from the gear configuration.
#[derive(Debug, Clone)]
pub(crate) struct EngineSettings {
    /// TCP connect and TLS handshake budget.
    pub connect_timeout: Duration,
    /// Response head and per-body-frame budget. SSE bodies use the
    /// streaming idle timeout instead (see `forward` in `service.rs`).
    pub read_timeout: Duration,
    /// ALPN protocol cache TTL (zero disables).
    pub protocol_cache_ttl: Duration,
    /// Accept any upstream certificate. **Tests only.**
    #[cfg(feature = "test-utils")]
    pub skip_upstream_tls_verify: bool,
}

/// The data plane's upstream engine.
pub struct ProxyEngine {
    client: ProxyClient,
    resolver: DnsResolver,
    ssrf_guard: Arc<SsrfGuard>,
}

impl ProxyEngine {
    /// Build the engine.
    ///
    /// # Errors
    ///
    /// Fails when the TLS client configuration cannot be built (no trust
    /// anchors, or no FIPS provider installed under `fips`).
    pub(crate) fn new(
        settings: &EngineSettings,
        ssrf_guard: Arc<SsrfGuard>,
    ) -> Result<Self, ProxyError> {
        let mut cfg = ProxyClientConfig::default();
        cfg.connect_timeout = settings.connect_timeout;
        cfg.read_timeout = Some(settings.read_timeout);
        cfg.protocol_cache_ttl = settings.protocol_cache_ttl;
        cfg.pool.idle_timeout = IDLE_TIMEOUT;
        cfg.pool.max_idle_per_host = MAX_IDLE_PER_HOST;
        #[cfg(feature = "test-utils")]
        if settings.skip_upstream_tls_verify {
            cfg.tls.verification = toolkit_http_proxy::CertVerification::DangerAcceptAny;
        }
        let client = ProxyClient::with_observer(cfg, Arc::new(LogObserver))?;
        Ok(Self {
            client,
            resolver: DnsResolver::default(),
            ssrf_guard,
        })
    }

    /// The underlying engine client.
    pub(crate) fn client(&self) -> &ProxyClient {
        &self.client
    }

    /// Engine target for a selected endpoint.
    ///
    /// Uses the load balancer's pre-resolved (already SSRF-filtered)
    /// address when present; otherwise resolves the host with retries and
    /// connects to the first address the SSRF policy allows.
    pub(crate) async fn target(
        &self,
        selected: &SelectedEndpoint,
        instance: &str,
    ) -> Result<Target, DomainError> {
        let ep = &selected.endpoint;
        let addr = match selected.resolved_addr {
            Some(addr) => addr,
            None => self.resolve(ep, instance).await?,
        };
        Ok(target_for(ep, addr))
    }

    async fn resolve(&self, ep: &Endpoint, instance: &str) -> Result<SocketAddr, DomainError> {
        let addrs = self.resolver.resolve(&ep.host, ep.port).await.map_err(|e| {
            warn!(host = %ep.host, port = ep.port, error = %e, "DNS resolution failed after retries");
            DomainError::LinkUnavailable {
                detail: "upstream connection error".into(),
                instance: instance.to_string(),
            }
        })?;
        first_permitted_addr(&self.ssrf_guard, &ep.host, addrs).map_err(|why| {
            match why {
                NoPermittedAddr::NoneResolved => {
                    warn!(host = %ep.host, port = ep.port, "DNS returned no usable addresses");
                }
                NoPermittedAddr::Refused(reason) => {
                    warn!(host = %ep.host, port = ep.port, reason, "SSRF policy refused every resolved address");
                }
            }
            DomainError::LinkUnavailable {
                detail: "upstream connection error".into(),
                instance: instance.to_string(),
            }
        })
    }
}

/// Map an endpoint and its address onto an engine target.
///
/// TLS for `https`, `wss` and `wt`; WebSocket upgrades always use
/// HTTP/1.1. Cleartext endpoints speak HTTP/1.1.
pub(crate) fn target_for(ep: &Endpoint, addr: SocketAddr) -> Target {
    let transport = match ep.scheme {
        Scheme::Https | Scheme::Wss | Scheme::Wt => Transport::Tls,
        Scheme::Http | Scheme::Grpc => Transport::Plain,
    };
    let policy = match ep.scheme {
        Scheme::Wss => ProtocolPolicy::Http1Only,
        _ => ProtocolPolicy::Auto,
    };
    Target::new(addr, ep.host.as_str(), transport)
        .with_authority_port(ep.port)
        .with_policy(policy)
}

/// Map an engine failure onto the OAGW error model.
pub(crate) fn map_proxy_error(err: &ProxyError, instance: &str) -> DomainError {
    let instance = instance.to_string();
    debug!(kind = %err.kind(), error = %err, "upstream exchange failed");
    match err.kind() {
        ErrorKind::ConnectTimeout => DomainError::ConnectionTimeout {
            detail: "upstream connection timed out".into(),
            instance,
        },
        ErrorKind::ReadTimeout => DomainError::RequestTimeout {
            detail: "upstream read timed out".into(),
            instance,
        },
        ErrorKind::WriteTimeout => DomainError::RequestTimeout {
            detail: "upstream write timed out".into(),
            instance,
        },
        ErrorKind::Http2 => DomainError::ProtocolError {
            detail: "upstream HTTP/2 error".into(),
            instance,
        },
        ErrorKind::Read => DomainError::StreamAborted {
            detail: "upstream stream read error".into(),
            instance,
        },
        ErrorKind::Write => DomainError::StreamAborted {
            detail: "upstream stream write error".into(),
            instance,
        },
        ErrorKind::ConnectNoRoute => DomainError::LinkUnavailable {
            detail: "no route to upstream host".into(),
            instance,
        },
        ErrorKind::Connect => DomainError::LinkUnavailable {
            detail: "upstream connection error".into(),
            instance,
        },
        ErrorKind::ConnectionClosed => DomainError::DownstreamError {
            detail: "upstream connection closed (peer disconnect)".into(),
            instance,
        },
        ErrorKind::ConnectRefused => DomainError::DownstreamError {
            detail: "upstream connection refused".into(),
            instance,
        },
        ErrorKind::TlsHandshake | ErrorKind::TlsHandshakeTimeout => DomainError::DownstreamError {
            detail: "upstream TLS handshake failed".into(),
            instance,
        },
        ErrorKind::InvalidCertificate => DomainError::DownstreamError {
            detail: "upstream certificate invalid".into(),
            instance,
        },
        ErrorKind::InvalidRequest => DomainError::Validation {
            field: "request",
            reason: field::INVALID_VALUE,
            detail: err.message().to_string(),
            instance,
        },
        ErrorKind::Configuration => DomainError::Internal {
            message: format!("upstream engine misconfigured: {err}"),
        },
        // `Protocol` and any future kind.
        _ => DomainError::DownstreamError {
            detail: "upstream sent an invalid response".into(),
            instance,
        },
    }
}

/// Adapt an engine response body to the SDK body stream.
pub(crate) fn into_body_stream(body: ResponseBody) -> BodyStream {
    Box::pin(body.into_data_stream().map_err(|e| Box::new(e) as BoxError))
}

/// Logs connection reuse at debug level.
struct LogObserver;

impl Observer for LogObserver {
    fn on_connection(&self, info: &ConnectionInfo<'_>) {
        debug!(
            reused = info.reused,
            peer = %info.target,
            version = ?info.version,
            connect_time = ?info.connect_time,
            "Connected to upstream"
        );
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod engine_tests;
