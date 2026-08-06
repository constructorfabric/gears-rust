//! Minimal Pingora-based HTTP/1 forwarding (design.md D7):
//! `pingora_core::connectors::http::v1::Connector`, a pooled outbound
//! client, called directly from an ordinary axum handler - not
//! `pingora-proxy`'s full session-owning engine (the dispatcher has no
//! auth/policy logic of its own to run first, unlike `oagw`). The request
//! body is buffered (bounded per DESIGN.md's own batch/event size limits);
//! the response body streams back with an idle-timeout on each read
//! (design.md D9), so long-poll/SSE connections stay open as long as bytes
//! - heartbeat or data - keep arriving.

use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{HeaderName, StatusCode};
use axum::response::Response;
use http_body_util::BodyExt;
use pingora_core::connectors::http::v1::Connector;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_http::RequestHeader;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// The in-flight channel buffer for streamed response chunks.
const RESPONSE_CHANNEL_CAPACITY: usize = 4;

/// Forwarding to `addr` (`host:port`, no scheme) failed - the caller maps
/// this to the "instance unreachable" `503` (design.md D10), distinct from
/// "no instance registered".
#[derive(Debug)]
pub struct ProxyError;

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

/// Forwards `req` to `addr` (a resolved `ServiceInstance::address`, e.g.
/// `"http://host:port"` - design.md D5) using the shared, pooled
/// `connector`, and returns the relayed response, streaming the response
/// body back with an idle-timeout on each read: `idle_timeout` resets on
/// every byte received (heartbeat or data), not tied to total connection
/// duration (design.md D9) - the caller (`forward()`) supplies the
/// hardcoded production constant; tests supply a short one so the
/// close-on-idle behavior doesn't require a real wait.
pub async fn proxy(
    addr: &str,
    connector: &Connector,
    req: Request,
    idle_timeout: Duration,
) -> Result<Response, ProxyError> {
    let host_port = addr
        .strip_prefix("http://")
        .or_else(|| addr.strip_prefix("https://"))
        .unwrap_or(addr);
    let peer_addr: std::net::SocketAddr = host_port.parse().map_err(|_| ProxyError)?;
    let peer = HttpPeer::new(peer_addr, false, String::new());

    let (parts, body) = req.into_parts();
    let body_bytes = body.collect().await.map_err(|_| ProxyError)?.to_bytes();

    let (mut session, _reused) = connector
        .get_http_session(&peer)
        .await
        .map_err(|_| ProxyError)?;

    let path_and_query = parts.uri.path_and_query().map_or("/", |pq| pq.as_str());
    let mut header = RequestHeader::build(parts.method.as_str(), path_and_query.as_bytes(), None)
        .map_err(|_| ProxyError)?;
    for (name, value) in &parts.headers {
        if is_hop_by_hop(name) || name == axum::http::header::HOST {
            continue;
        }
        header
            .append_header(name, value.as_bytes())
            .map_err(|_| ProxyError)?;
    }
    header
        .insert_header("host", host_port)
        .map_err(|_| ProxyError)?;
    header
        .insert_header("content-length", body_bytes.len().to_string())
        .map_err(|_| ProxyError)?;

    session
        .write_request_header(Box::new(header))
        .await
        .map_err(|_| ProxyError)?;
    if !body_bytes.is_empty() {
        session
            .write_body(&body_bytes)
            .await
            .map_err(|_| ProxyError)?;
    }
    session.finish_body().await.map_err(|_| ProxyError)?;

    session.read_response().await.map_err(|_| ProxyError)?;
    let status = session.get_status().ok_or(ProxyError)?;
    let resp_headers = session.resp_header().ok_or(ProxyError)?.headers.clone();

    let mut builder =
        Response::builder().status(StatusCode::from_u16(status.as_u16()).map_err(|_| ProxyError)?);
    for (name, value) in &resp_headers {
        if is_hop_by_hop(name) {
            continue;
        }
        builder = builder.header(name, value);
    }

    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(RESPONSE_CHANNEL_CAPACITY);
    tokio::spawn(async move {
        loop {
            let read = tokio::time::timeout(idle_timeout, session.read_body_bytes()).await;
            match read {
                Ok(Ok(Some(chunk))) => {
                    if tx.send(Ok(chunk)).await.is_err() {
                        break;
                    }
                }
                Ok(Ok(None)) => break,
                Ok(Err(_)) | Err(_) => {
                    tx.send(Err(std::io::Error::other(
                        "upstream response read failed or idle-timed out",
                    )))
                    .await
                    .ok();
                    break;
                }
            }
        }
    });
    let body = Body::from_stream(ReceiverStream::new(rx));

    builder.body(body).map_err(|_| ProxyError)
}
