use crate::domain::error::DomainError;
use axum::body::Body;
use axum::extract::{Extension, Request};
use axum::response::Response;
use modkit_security::SecurityContext;
use oagw_sdk::api::ErrorSource;

use crate::api::rest::error::error_response;
use crate::module::AppState;

/// Proxy handler for `/oagw/v1/proxy/{alias}/{path:.*}`.
///
/// Parses the alias and path suffix from the URL, validates the request,
/// builds an `http::Request<oagw_sdk::Body>`, and delegates to the Data Plane service.
pub async fn proxy_handler(
    Extension(state): Extension<AppState>,
    Extension(ctx): Extension<SecurityContext>,
    req: Request,
) -> Result<Response, Response> {
    let max_body_size = state.config.max_body_size_bytes;
    let (mut parts, body) = req.into_parts();

    // Parse alias from the URI to validate it's present.
    let path = parts.uri.path();
    let prefix = "/oagw/v1/proxy/";
    let remaining = path.strip_prefix(prefix).ok_or_else(|| {
        error_response(DomainError::Validation {
            detail: "invalid proxy path".into(),
            instance: path.to_string(),
        })
    })?;

    // Validate alias is not empty.
    let alias_end = remaining.find('/').unwrap_or(remaining.len());
    if alias_end == 0 {
        return Err(error_response(DomainError::Validation {
            detail: "missing alias in proxy path".into(),
            instance: path.to_string(),
        }));
    }

    // Validate Content-Length if present.
    if let Some(cl) = parts.headers.get(http::header::CONTENT_LENGTH) {
        let cl_str = cl.to_str().map_err(|_| {
            error_response(DomainError::Validation {
                detail: "invalid Content-Length header".into(),
                instance: path.to_string(),
            })
        })?;
        let cl_val: usize = cl_str.parse().map_err(|_| {
            error_response(DomainError::Validation {
                detail: format!("Content-Length is not a valid integer: '{cl_str}'"),
                instance: path.to_string(),
            })
        })?;
        if cl_val > max_body_size {
            return Err(error_response(DomainError::PayloadTooLarge {
                detail: format!(
                    "request body of {cl_val} bytes exceeds maximum of {max_body_size} bytes"
                ),
                instance: path.to_string(),
            }));
        }
    }

    // Read body bytes (limited to max_body_size).
    let body_bytes = axum::body::to_bytes(body, max_body_size)
        .await
        .map_err(|_| {
            error_response(DomainError::PayloadTooLarge {
                detail: format!("request body exceeds maximum of {max_body_size} bytes"),
                instance: path.to_string(),
            })
        })?;

    // Strip the proxy prefix from the URI so the DP receives /{alias}/{path}?query.
    let new_uri_str = if let Some(query) = parts.uri.query() {
        format!("/{remaining}?{query}")
    } else {
        format!("/{remaining}")
    };
    parts.uri = new_uri_str.parse().map_err(|_| {
        error_response(DomainError::Validation {
            detail: "failed to parse proxy URI".into(),
            instance: path.to_string(),
        })
    })?;

    // Build http::Request<Body> for the DP service.
    let sdk_body = oagw_sdk::Body::from(body_bytes);
    let proxy_req = http::Request::from_parts(parts, sdk_body);

    // Execute proxy pipeline.
    let proxy_resp = state
        .dp
        .proxy_request(ctx, proxy_req)
        .await
        .map_err(error_response)?;

    // Convert http::Response<oagw_sdk::Body> to axum Response.
    let (resp_parts, sdk_body) = proxy_resp.into_parts();

    let error_source = resp_parts
        .extensions
        .get::<ErrorSource>()
        .copied()
        .unwrap_or(ErrorSource::Gateway);

    // Build axum response.
    // Response headers are already sanitized by the DP service layer.
    let mut builder = Response::builder().status(resp_parts.status);

    for (name, value) in &resp_parts.headers {
        builder = builder.header(name, value);
    }

    // Add error source header.
    builder = builder.header("x-oagw-error-source", error_source.as_str());

    // Stream the response body.
    let body = Body::from_stream(sdk_body.into_stream());

    builder.body(body).map_err(|e| {
        error_response(DomainError::DownstreamError {
            detail: format!("failed to build response: {e}"),
            instance: String::new(),
        })
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_alias_and_suffix() {
        let path = "/oagw/v1/proxy/api.openai.com/v1/chat/completions";
        let prefix = "/oagw/v1/proxy/";
        let remaining = path.strip_prefix(prefix).unwrap();
        let (alias, suffix) = match remaining.find('/') {
            Some(pos) => (&remaining[..pos], &remaining[pos..]),
            None => (remaining, ""),
        };
        assert_eq!(alias, "api.openai.com");
        assert_eq!(suffix, "/v1/chat/completions");
    }

    #[test]
    fn parse_alias_with_port() {
        let path = "/oagw/v1/proxy/host:8443/path";
        let prefix = "/oagw/v1/proxy/";
        let remaining = path.strip_prefix(prefix).unwrap();
        let (alias, suffix) = match remaining.find('/') {
            Some(pos) => (&remaining[..pos], &remaining[pos..]),
            None => (remaining, ""),
        };
        assert_eq!(alias, "host:8443");
        assert_eq!(suffix, "/path");
    }

    #[test]
    fn parse_alias_no_suffix() {
        let path = "/oagw/v1/proxy/api.openai.com";
        let prefix = "/oagw/v1/proxy/";
        let remaining = path.strip_prefix(prefix).unwrap();
        let (alias, suffix) = match remaining.find('/') {
            Some(pos) => (&remaining[..pos], &remaining[pos..]),
            None => (remaining, ""),
        };
        assert_eq!(alias, "api.openai.com");
        assert_eq!(suffix, "");
    }

    #[test]
    fn parse_query_params() {
        let query = "version=2&model=gpt-4";
        let params: Vec<(String, String)> = form_urlencoded::parse(query.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(params.len(), 2);
        assert_eq!(params[0], ("version".into(), "2".into()));
        assert_eq!(params[1], ("model".into(), "gpt-4".into()));
    }

    #[test]
    fn percent_encoded_param_name_decoded() {
        let query = "my%20key=value";
        let params: Vec<(String, String)> = form_urlencoded::parse(query.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(params.len(), 1);
        assert_eq!(params[0], ("my key".into(), "value".into()));
    }
}
