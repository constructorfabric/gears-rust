//! Resolve the client address once per request and hand it to handlers as a
//! [`ClientIp`] extension, so they need not trust forwarding headers.

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use toolkit::http::ClientIp;

/// The client address, or `None` when neither a trusted forwarding header nor
/// the peer address gives one.
///
/// Client-supplied forwarding headers (`X-Forwarded-For` / `X-Real-IP`) are
/// only honored when the gateway sits behind a known number of trusted reverse
/// proxies, given by `trusted_proxy_hops`:
///
/// - `0` (the default): the headers are fully client-controlled and are
///   ignored. The peer address from `ConnectInfo` is used, so a caller cannot
///   spoof or rotate the address.
/// - `n >= 1`: the address is the `X-Forwarded-For` entry `n` positions from
///   the right — the value appended by the outermost trusted proxy, which an
///   untrusted client cannot forge (any spoofed entries it prepends only shift
///   this index further right). When `X-Forwarded-For` is absent or too short,
///   or that entry is not an IP address, the immediate (trusted) proxy's
///   `X-Real-IP` is used, then the peer address.
///
/// A proxy may append its entry as a separate `X-Forwarded-For` field line
/// rather than to the client's line, so the entries of every line are read,
/// in order. An entry that is not valid UTF-8 is unparseable but keeps its
/// position. More than one `X-Real-IP` line is ambiguous (the client may have
/// sent one of them) and is ignored, as is a non-UTF-8 one.
pub(crate) fn resolve(req: &Request, trusted_proxy_hops: usize) -> Option<IpAddr> {
    if trusted_proxy_hops == 0 {
        return peer_ip(req);
    }
    let headers = req.headers();
    let entries = forwarded_for(headers);
    if let Some(idx) = entries.len().checked_sub(trusted_proxy_hops)
        && let Some(Some(ip)) = entries.get(idx)
    {
        return Some(*ip);
    }
    real_ip(headers).or_else(|| peer_ip(req))
}

/// Every `X-Forwarded-For` entry across all field lines, left to right;
/// `None` for an entry that is not an IP address.
fn forwarded_for(headers: &HeaderMap) -> Vec<Option<IpAddr>> {
    headers
        .get_all("x-forwarded-for")
        .iter()
        // Split bytes: `,` never occurs inside a multi-byte UTF-8 sequence.
        .flat_map(|line| line.as_bytes().split(|b| *b == b','))
        .map(<[u8]>::trim_ascii)
        .filter(|entry| !entry.is_empty())
        .map(parse_ip)
        .collect()
}

/// The single `X-Real-IP` line, when it is an IP address.
fn real_ip(headers: &HeaderMap) -> Option<IpAddr> {
    let mut lines = headers.get_all("x-real-ip").iter();
    match (lines.next(), lines.next()) {
        (Some(line), None) => parse_ip(line.as_bytes().trim_ascii()),
        _ => None,
    }
}

fn parse_ip(entry: &[u8]) -> Option<IpAddr> {
    std::str::from_utf8(entry).ok()?.parse().ok()
}

fn peer_ip(req: &Request) -> Option<IpAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip())
}

/// Insert the [`ClientIp`] extension when the address is known. Any value
/// already present is replaced, so only this resolution reaches handlers.
pub async fn client_ip_middleware(
    State(trusted_proxy_hops): State<usize>,
    mut req: Request,
    next: Next,
) -> Response {
    match resolve(&req, trusted_proxy_hops) {
        Some(ip) => {
            req.extensions_mut().insert(ClientIp(ip));
        }
        None => {
            req.extensions_mut().remove::<ClientIp>();
        }
    }
    next.run(req).await
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::HeaderValue;
    use axum::middleware::from_fn_with_state;
    use axum::routing::get;
    use tower::ServiceExt;

    /// What a handler behind the middleware sees.
    async fn seen(trusted_proxy_hops: usize, req: Request) -> String {
        let app = Router::new()
            .route(
                "/",
                get(|req: Request| async move {
                    req.extensions()
                        .get::<ClientIp>()
                        .map_or_else(|| "none".to_owned(), |ip| ip.0.to_string())
                }),
            )
            .layer(from_fn_with_state(trusted_proxy_hops, client_ip_middleware));
        let resp = app.oneshot(req).await.unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        String::from_utf8(body.to_vec()).unwrap()
    }

    fn request(headers: &[(&str, &str)], peer: Option<&str>) -> Request {
        let mut builder = Request::builder().uri("/");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let mut req = builder.body(Body::empty()).unwrap();
        if let Some(peer) = peer {
            req.extensions_mut()
                .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        }
        req
    }

    #[tokio::test]
    async fn without_trusted_proxies_the_peer_address_is_used() {
        let req = request(
            &[
                ("x-forwarded-for", "203.0.113.7, 10.0.0.1"),
                ("x-real-ip", "198.51.100.9"),
            ],
            Some("192.168.1.5:1234"),
        );
        assert_eq!(seen(0, req).await, "192.168.1.5");

        // Forwarding headers alone give no address.
        let req = request(&[("x-forwarded-for", "203.0.113.7")], None);
        assert_eq!(seen(0, req).await, "none");
    }

    #[tokio::test]
    async fn trusted_proxy_hops_pick_the_entry_the_outermost_proxy_wrote() {
        let req = request(&[("x-forwarded-for", "1.1.1.1, 203.0.113.7")], None);
        assert_eq!(seen(1, req).await, "203.0.113.7");

        let req = request(
            &[("x-forwarded-for", "9.9.9.9, 203.0.113.7, 10.0.0.1")],
            None,
        );
        assert_eq!(seen(2, req).await, "203.0.113.7");
    }

    #[tokio::test]
    async fn trusted_proxy_falls_back_to_real_ip_then_peer() {
        let req = request(
            &[
                ("x-forwarded-for", "203.0.113.7"),
                ("x-real-ip", "198.51.100.9"),
            ],
            None,
        );
        assert_eq!(seen(3, req).await, "198.51.100.9");

        let req = request(
            &[("x-forwarded-for", "not-an-ip")],
            Some("192.168.1.5:1234"),
        );
        assert_eq!(seen(1, req).await, "192.168.1.5");
    }

    /// A proxy that adds its own field line instead of appending to the
    /// client's must still win: a spoofed first line cannot pick the key.
    #[tokio::test]
    async fn trusted_entries_are_read_across_field_lines() {
        for spoofed in ["198.51.100.1", "198.51.100.2"] {
            let req = request(
                &[
                    ("x-forwarded-for", spoofed),
                    ("x-forwarded-for", "203.0.113.7"),
                ],
                None,
            );
            assert_eq!(seen(1, req).await, "203.0.113.7", "spoofed {spoofed}");
        }

        // Lines with several entries and lines with one, in order.
        let headers = [
            ("x-forwarded-for", "9.9.9.9, 198.51.100.1"),
            ("x-forwarded-for", "203.0.113.7"),
            ("x-forwarded-for", "10.0.0.1, 10.0.0.2"),
        ];
        for (hops, expected) in [
            (1, "10.0.0.2"),
            (2, "10.0.0.1"),
            (3, "203.0.113.7"),
            (4, "198.51.100.1"),
        ] {
            assert_eq!(
                seen(hops, request(&headers, None)).await,
                expected,
                "{hops}"
            );
        }

        // Without trusted proxies the lines are still ignored.
        let req = request(
            &[
                ("x-forwarded-for", "198.51.100.1"),
                ("x-forwarded-for", "203.0.113.7"),
            ],
            Some("192.168.1.5:1234"),
        );
        assert_eq!(seen(0, req).await, "192.168.1.5");
    }

    /// A non-UTF-8 entry is unparseable but still counts, so it cannot shift
    /// the trusted entries or hide them.
    #[tokio::test]
    async fn a_non_utf8_entry_keeps_its_position() {
        let mut req = request(&[("x-real-ip", "198.51.100.9")], Some("192.168.1.5:1234"));
        req.headers_mut().append(
            "x-forwarded-for",
            HeaderValue::from_bytes(b"\xff, 203.0.113.7").unwrap(),
        );
        req.headers_mut()
            .append("x-forwarded-for", HeaderValue::from_static("10.0.0.1"));
        assert_eq!(seen(2, req).await, "203.0.113.7");

        let mut req = request(&[], Some("192.168.1.5:1234"));
        req.headers_mut().append(
            "x-forwarded-for",
            HeaderValue::from_bytes(b"\xff, 203.0.113.7").unwrap(),
        );
        assert_eq!(seen(2, req).await, "192.168.1.5");
    }

    /// Several `X-Real-IP` lines are ambiguous: the peer is used instead.
    #[tokio::test]
    async fn real_ip_is_used_only_when_it_is_a_single_line() {
        let req = request(
            &[("x-real-ip", "198.51.100.1"), ("x-real-ip", "198.51.100.9")],
            Some("192.168.1.5:1234"),
        );
        assert_eq!(seen(1, req).await, "192.168.1.5");

        let mut req = request(&[], Some("192.168.1.5:1234"));
        req.headers_mut()
            .append("x-real-ip", HeaderValue::from_bytes(b"\xff").unwrap());
        assert_eq!(seen(1, req).await, "192.168.1.5");

        let req = request(&[("x-real-ip", " 198.51.100.9 ")], Some("192.168.1.5:1234"));
        assert_eq!(seen(1, req).await, "198.51.100.9");
    }

    #[tokio::test]
    async fn a_value_inserted_before_the_middleware_is_replaced() {
        let mut req = request(&[], Some("192.168.1.5:1234"));
        req.extensions_mut()
            .insert(ClientIp("203.0.113.7".parse().unwrap()));
        assert_eq!(seen(0, req).await, "192.168.1.5");

        let mut req = request(&[], None);
        req.extensions_mut()
            .insert(ClientIp("203.0.113.7".parse().unwrap()));
        assert_eq!(seen(0, req).await, "none");
    }
}
