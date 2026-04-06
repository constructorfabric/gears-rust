use crate::domain::error::DomainError;
use crate::domain::model::{Endpoint, Scheme};

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

    // Combine route path + path suffix, avoiding double slashes.
    let path = if path_suffix.is_empty() {
        route_path.to_string()
    } else if route_path.ends_with('/') && path_suffix.starts_with('/') {
        format!("{}{}", route_path, &path_suffix[1..])
    } else if !route_path.ends_with('/') && !path_suffix.starts_with('/') {
        format!("{route_path}/{path_suffix}")
    } else {
        format!("{route_path}{path_suffix}")
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
#[path = "request_builder_tests.rs"]
mod request_builder_tests;
