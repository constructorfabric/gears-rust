use crate::domain::error::DomainError;
use crate::domain::model::{Endpoint, Scheme};
use oagw_sdk::field;

/// Build the full upstream URL from endpoint, route path, path suffix, and query params.
///
/// # Errors
///
/// Returns `DomainError::Validation` if the endpoint uses an unsupported scheme (e.g. gRPC).
pub fn build_upstream_url(
    endpoint: &Endpoint,
    route_path: &str,
    path_suffix: &str,
    query_params: &[(String, String)],
) -> Result<String, DomainError> {
    let scheme = match endpoint.scheme {
        Scheme::Http => "http",
        Scheme::Https => "https",
        Scheme::Wss => "wss",
        Scheme::Wt => "https",
        Scheme::Grpc => {
            return Err(DomainError::Validation {
                field: "server.endpoints.scheme",
                reason: field::UNSUPPORTED_SCHEME,
                detail: "gRPC scheme is not supported for HTTP proxy".into(),
                instance: String::new(),
            });
        }
    };

    let host_port = if is_default_port(scheme, endpoint.port) {
        endpoint.host.clone()
    } else {
        format!("{}:{}", endpoint.host, endpoint.port)
    };

    // Combine route path + path suffix. A suffix that does not start with
    // `/` continues the request's last segment (`.json`, `:verb`, `%2Fx`)
    // and is appended as the client sent it. Route matching ignores a
    // trailing `/` on the route path (`/v1/admin/` covers `/v1/admin.json`
    // with suffix `.json`), so it is dropped before a non-empty suffix:
    // no `//`, and no `/` the client did not send before `.json`.
    let path = match route_path.strip_suffix('/') {
        Some(base) if !path_suffix.is_empty() => format!("{base}{path_suffix}"),
        _ => format!("{route_path}{path_suffix}"),
    };

    let mut url = format!("{scheme}://{host_port}{path}");

    if !query_params.is_empty() {
        url.push('?');
        let qs = form_urlencoded::Serializer::new(String::new())
            .extend_pairs(query_params)
            .finish();
        url.push_str(&qs);
    }

    Ok(url)
}

fn is_default_port(scheme: &str, port: u16) -> bool {
    matches!((scheme, port), ("https" | "wss", 443) | ("http" | "ws", 80))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(host: &str, port: u16) -> Endpoint {
        Endpoint {
            scheme: Scheme::Https,
            host: host.into(),
            port,
        }
    }

    #[test]
    fn standard_url() {
        let url = build_upstream_url(
            &endpoint("api.openai.com", 443),
            "/v1/chat",
            "/completions",
            &[],
        )
        .unwrap();
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
    }

    #[test]
    fn with_query_params() {
        let url = build_upstream_url(
            &endpoint("api.openai.com", 443),
            "/v1/chat",
            "/models/gpt-4",
            &[("version".into(), "2".into())],
        )
        .unwrap();
        assert_eq!(url, "https://api.openai.com/v1/chat/models/gpt-4?version=2");
    }

    #[test]
    fn nonstandard_port() {
        let url =
            build_upstream_url(&endpoint("api.openai.com", 443), "/v1/chat", ".json", &[]).unwrap();
        assert_eq!(url, "https://api.openai.com/v1/chat.json");
        let url = build_upstream_url(&endpoint("localhost", 8080), "/api", "", &[]).unwrap();
        assert_eq!(url, "https://localhost:8080/api");
    }

    #[test]
    fn empty_suffix() {
        let url =
            build_upstream_url(&endpoint("api.openai.com", 443), "/v1/models", "", &[]).unwrap();
        assert_eq!(url, "https://api.openai.com/v1/models");
    }

    #[test]
    fn avoids_double_slash() {
        let url =
            build_upstream_url(&endpoint("api.openai.com", 443), "/v1/", "/chat", &[]).unwrap();
        assert_eq!(url, "https://api.openai.com/v1/chat");
    }

    /// A route with a trailing `/` matches a request that continues its last
    /// segment (`/v1/admin.json`); the upstream gets the path the client
    /// sent, not `/v1/admin/.json`.
    #[test]
    fn trailing_slash_route_with_segment_suffix() {
        let ep = endpoint("api.example.com", 443);
        let url = build_upstream_url(&ep, "/v1/admin/", ".json", &[]).unwrap();
        assert_eq!(url, "https://api.example.com/v1/admin.json");
        let url = build_upstream_url(&ep, "/v1/admin/", "", &[]).unwrap();
        assert_eq!(url, "https://api.example.com/v1/admin/");
        let url = build_upstream_url(&ep, "/", "/x", &[]).unwrap();
        assert_eq!(url, "https://api.example.com/x");
    }

    #[test]
    fn multiple_query_params() {
        let url = build_upstream_url(
            &endpoint("example.com", 443),
            "/api",
            "/data",
            &[("key".into(), "val".into()), ("foo".into(), "bar".into())],
        )
        .unwrap();
        assert_eq!(url, "https://example.com/api/data?key=val&foo=bar");
    }

    #[test]
    fn http_scheme() {
        let ep = Endpoint {
            scheme: Scheme::Http,
            host: "127.0.0.1".into(),
            port: 3000,
        };
        let url = build_upstream_url(&ep, "/v1/test", "", &[]).unwrap();
        assert_eq!(url, "http://127.0.0.1:3000/v1/test");
    }

    #[test]
    fn http_default_port() {
        let ep = Endpoint {
            scheme: Scheme::Http,
            host: "example.com".into(),
            port: 80,
        };
        let url = build_upstream_url(&ep, "/api", "", &[]).unwrap();
        assert_eq!(url, "http://example.com/api");
    }

    #[test]
    fn query_value_with_ampersand_is_encoded() {
        let url = build_upstream_url(
            &endpoint("api.openai.com", 443),
            "/v1/search",
            "",
            &[("q".into(), "a&b".into())],
        )
        .unwrap();
        assert_eq!(url, "https://api.openai.com/v1/search?q=a%26b");
    }

    #[test]
    fn grpc_scheme_returns_error() {
        let ep = Endpoint {
            scheme: Scheme::Grpc,
            host: "grpc.example.com".into(),
            port: 443,
        };
        let err = build_upstream_url(&ep, "/service", "", &[]).unwrap_err();
        match err {
            DomainError::Validation { field, reason, .. } => {
                assert_eq!(field, "server.endpoints.scheme");
                assert_eq!(reason, oagw_sdk::field::UNSUPPORTED_SCHEME);
            }
            other => panic!("expected UNSUPPORTED_SCHEME, got {other:?}"),
        }
    }
}
