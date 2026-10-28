//! HTTP/1 forwarding over the shared upstream engine (design.md D7):
//! a pooled `toolkit_http_proxy::ProxyClient` called directly from an
//! ordinary axum handler - the dispatcher has no auth/policy logic of its own
//! to run first, unlike `oagw`. The request body is buffered, bounded via
//! [`crate::config::MAX_REQUEST_BODY_BYTES`] (`http_body_util::Limited`, not
//! `BatchConfig::max_payload_bytes` - see that constant's doc comment); the
//! response body streams back with an idle-timeout on each read (design.md
//! D9), so long-poll/SSE connections stay open as long as bytes - heartbeat
//! or data - keep arriving.

use axum::body::Body;
use axum::extract::Request;
use axum::http::HeaderName;
use axum::response::Response;
use http_body_util::{BodyExt, Limited};
use toolkit_http_proxy::{ProtocolPolicy, ProxyClient, Target, Transport, full_body};

/// Forwarding to `addr` (`host:port`, no scheme) failed - the caller (design
/// wise `forward.rs`) logs the real cause and maps every variant but
/// [`ProxyError::BodyTooLarge`] to the "instance unreachable" `503`
/// (design.md D10, distinct from "no instance registered");
/// `BodyTooLarge` maps to `413` instead.
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    /// Peer-address resolution (literal parse or `tokio::net::lookup_host`)
    /// failed for `addr`.
    #[error("failed to resolve forwarding address {addr}: {source}")]
    Resolve {
        addr: String,
        #[source]
        source: std::io::Error,
    },

    /// Connecting to the instance, writing the request or reading the
    /// response head failed (including the idle-read timeout).
    #[error("upstream exchange failed: {0}")]
    Upstream(#[source] toolkit_http_proxy::ProxyError),

    /// The upstream response, or our own outbound request, could not be
    /// represented (header build/append/insert, missing status/response
    /// header, status-code parse, response-builder failure, or a
    /// non-size-related request-body read failure) - not independently
    /// actionable beyond "malformed", per design.md's Decisions.
    #[error("malformed upstream response or request construction failure")]
    MalformedResponse,

    /// The proxied request body exceeded the caller-supplied limit
    /// (defaulting to [`crate::config::MAX_REQUEST_BODY_BYTES`]).
    #[error("proxied request body exceeded {limit} bytes")]
    BodyTooLarge { limit: usize },

    /// The resolved instance advertised an unsupported scheme (`https://`)
    /// - rejected rather than silently downgraded to plaintext.
    #[error("endpoint advertises unsupported scheme {scheme:?}")]
    UnsupportedScheme { scheme: String },
}

/// RFC 7230 §6.1 hop-by-hop headers, stripped in both directions - the same
/// set `oagw`'s bridge uses.
fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// Resolves `host_port` (no scheme) to a connectable [`std::net::SocketAddr`].
/// Tries the fast literal `SocketAddr::parse` first (no DNS lookup, no
/// behavior change for IP-literal-advertised instances); on failure, falls
/// back to asynchronous resolution via `tokio::net::lookup_host`, which
/// cannot block the runtime thread or panic on resolution failure.
/// The first resolved address is used. Hostname-advertised instances are not
/// a hypothetical: the real platform Directory service intentionally
/// accepts them (design.md's "Hostname resolution is not a latent edge
/// case").
async fn resolve_peer_addr(host_port: &str) -> Result<std::net::SocketAddr, ProxyError> {
    if let Ok(addr) = host_port.parse() {
        return Ok(addr);
    }

    let mut addrs =
        tokio::net::lookup_host(host_port)
            .await
            .map_err(|source| ProxyError::Resolve {
                addr: host_port.to_owned(),
                source,
            })?;

    addrs.next().ok_or_else(|| ProxyError::Resolve {
        addr: host_port.to_owned(),
        source: std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "hostname resolved to zero addresses",
        ),
    })
}

/// Forwards `req` to `addr` (a resolved `ServiceInstance::address`, e.g.
/// `"http://host:port"` - design.md D5) using the shared, pooled `client`,
/// and returns the relayed response, streaming the response body back with
/// the client's idle-read timeout on each read: it resets on every byte
/// received (heartbeat or data), not tied to total connection duration
/// (design.md D9). The connect and idle-read budgets live in the client's
/// configuration (`DispatcherState` builds it).
///
/// `addr` is rejected up front with [`ProxyError::UnsupportedScheme`] if it
/// advertises `https://` - real upstream TLS support is out of scope for
/// this change (design.md's Decisions); an `http://`-advertised or
/// bare `host:port` address proceeds exactly as before.
pub async fn proxy(
    addr: &str,
    client: &ProxyClient,
    req: Request,
    max_body_bytes: usize,
) -> Result<Response, ProxyError> {
    if addr.starts_with("https://") {
        return Err(ProxyError::UnsupportedScheme {
            scheme: "https".to_owned(),
        });
    }
    let host_port = addr.strip_prefix("http://").unwrap_or(addr);

    let peer_addr = resolve_peer_addr(host_port).await?;

    let (parts, body) = req.into_parts();
    let body_bytes = Limited::new(body, max_body_bytes)
        .collect()
        .await
        .map_err(|err| {
            if err
                .downcast_ref::<http_body_util::LengthLimitError>()
                .is_some()
            {
                ProxyError::BodyTooLarge {
                    limit: max_body_bytes,
                }
            } else {
                ProxyError::MalformedResponse
            }
        })?
        .to_bytes();

    let path_and_query = parts.uri.path_and_query().map_or("/", |pq| pq.as_str());
    let mut outbound = axum::http::Request::builder()
        .method(parts.method)
        .uri(path_and_query)
        .body(full_body(body_bytes))
        .map_err(|_| ProxyError::MalformedResponse)?;
    for (name, value) in &parts.headers {
        if is_hop_by_hop(name) || name == axum::http::header::HOST {
            continue;
        }
        outbound.headers_mut().append(name, value.clone());
    }
    // `host_port` (the original advertised text, not the resolved
    // `SocketAddr`) is what's sent as the `Host` header regardless of
    // whether resolution took the literal or `lookup_host` path. The engine
    // derives `Content-Length` from the buffered body.
    outbound.headers_mut().insert(
        axum::http::header::HOST,
        host_port
            .parse()
            .map_err(|_| ProxyError::MalformedResponse)?,
    );

    let host = host_port
        .rsplit_once(':')
        .map_or(host_port, |(host, _)| host)
        .trim_start_matches('[')
        .trim_end_matches(']');
    let target =
        Target::new(peer_addr, host, Transport::Plain).with_policy(ProtocolPolicy::Http1Only);
    let resp = client
        .send(&target, outbound)
        .await
        .map_err(ProxyError::Upstream)?;

    let (mut head, body) = resp.into_parts();
    let hop_by_hop: Vec<HeaderName> = head
        .headers
        .keys()
        .filter(|name| is_hop_by_hop(name))
        .cloned()
        .collect();
    for name in hop_by_hop {
        head.headers.remove(name);
    }
    Ok(Response::from_parts(head, Body::new(body)))
}
