//! CORS domain logic: validation, preflight handling, and response header injection.
//!
//! All functions are pure domain logic with no infrastructure dependencies.

use super::error::DomainError;
use super::model::{CorsConfig, CorsHttpMethod};

/// Maximum allowed value for `max_age` (24 hours in seconds).
const MAX_AGE_LIMIT: u32 = 86400;

// ---------------------------------------------------------------------------
// CorsHttpMethod helpers
// ---------------------------------------------------------------------------

impl CorsHttpMethod {
    /// Convert a method string (case-insensitive) to a `CorsHttpMethod`.
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "GET" => Some(Self::Get),
            "POST" => Some(Self::Post),
            "PUT" => Some(Self::Put),
            "DELETE" => Some(Self::Delete),
            "PATCH" => Some(Self::Patch),
            "HEAD" => Some(Self::Head),
            "OPTIONS" => Some(Self::Options),
            _ => None,
        }
    }

    /// Return the uppercase string representation.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
        }
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate a CORS configuration at creation/update time.
///
/// Returns `Err(DomainError::Validation)` if the configuration is invalid.
pub fn validate_cors_config(config: &CorsConfig) -> Result<(), DomainError> {
    // Credentials + wildcard origin is forbidden per the Fetch specification.
    if config.allow_credentials && config.allowed_origins.iter().any(|o| o == "*") {
        return Err(DomainError::Validation {
            detail: "allow_credentials cannot be true when allowed_origins contains '*'".into(),
            instance: String::new(),
        });
    }

    // max_age must not exceed 24 hours.
    if config.max_age > MAX_AGE_LIMIT {
        return Err(DomainError::Validation {
            detail: format!("max_age must not exceed {MAX_AGE_LIMIT} seconds"),
            instance: String::new(),
        });
    }

    // Validate that origins are either "*" or look like a valid origin
    // (scheme://host or scheme://host:port).
    for origin in &config.allowed_origins {
        if origin == "*" {
            continue;
        }
        if !is_valid_origin(origin) {
            return Err(DomainError::Validation {
                detail: format!(
                    "invalid origin '{origin}': must be '*' or a valid origin (e.g. https://example.com)"
                ),
                instance: String::new(),
            });
        }
    }

    Ok(())
}

/// Check whether a string looks like a valid origin (scheme://host[:port]).
fn is_valid_origin(origin: &str) -> bool {
    // Must have a scheme separator.
    let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };

    // Must have a non-empty host portion.
    if rest.is_empty() {
        return false;
    }

    // IPv6 literal: http://[::1] or http://[::1]:8080
    if let Some(after_bracket) = rest.strip_prefix('[') {
        let Some(close) = after_bracket.find(']') else {
            return false;
        };
        if after_bracket[..close]
            .parse::<std::net::Ipv6Addr>()
            .is_err()
        {
            return false;
        }
        let remainder = &after_bracket[close + 1..];
        return match remainder.strip_prefix(':') {
            Some(port_str) => !port_str.is_empty() && port_str.parse::<u16>().is_ok(),
            None => remainder.is_empty(),
        };
    }

    // Split off optional port.
    let host = if let Some((h, port_str)) = rest.rsplit_once(':') {
        if port_str.parse::<u16>().is_err() {
            return false;
        }
        h
    } else {
        rest
    };

    // Host must be non-empty and contain only valid hostname characters.
    !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
}

// ---------------------------------------------------------------------------
// Preflight detection
// ---------------------------------------------------------------------------

/// Check whether an inbound request is a CORS preflight.
///
/// A CORS preflight is an OPTIONS request with both `Origin` and
/// `Access-Control-Request-Method` headers present.
#[must_use]
pub fn is_cors_preflight(method: &str, headers: &[(String, String)]) -> bool {
    if !method.eq_ignore_ascii_case("OPTIONS") {
        return false;
    }

    let has_origin = headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("origin"));
    let has_request_method = headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("access-control-request-method"));

    has_origin && has_request_method
}

// ---------------------------------------------------------------------------
// Preflight handler
// ---------------------------------------------------------------------------

/// Handle a CORS preflight request.
///
/// Validates the origin, requested method, and requested headers against the
/// CORS configuration. Returns the CORS response headers on success, or a
/// `DomainError` on validation failure.
pub fn handle_cors_preflight(
    config: &CorsConfig,
    origin: &str,
    request_method: &str,
    request_headers: &[String],
    instance: &str,
) -> Result<Vec<(String, String)>, DomainError> {
    // 1. Validate origin.
    if !is_origin_allowed(config, origin) {
        return Err(DomainError::CorsOriginNotAllowed {
            origin: origin.to_string(),
            instance: instance.to_string(),
        });
    }

    // 2. Validate request method.
    let method = CorsHttpMethod::from_str_loose(request_method).ok_or_else(|| {
        DomainError::CorsMethodNotAllowed {
            method: request_method.to_string(),
            instance: instance.to_string(),
        }
    })?;
    if !config.allowed_methods.contains(&method) {
        return Err(DomainError::CorsMethodNotAllowed {
            method: request_method.to_string(),
            instance: instance.to_string(),
        });
    }

    // 3. Validate request headers (case-insensitive).
    for header in request_headers {
        let header_lower = header.trim().to_ascii_lowercase();
        if header_lower.is_empty() {
            continue;
        }
        let allowed = config
            .allowed_headers
            .iter()
            .any(|h| h.eq_ignore_ascii_case(&header_lower));
        if !allowed {
            return Err(DomainError::CorsHeaderNotAllowed {
                header: header.trim().to_string(),
                instance: instance.to_string(),
            });
        }
    }

    // 4. Build response headers.
    let mut resp_headers = Vec::new();

    // Reflect origin (not wildcard) when credentials are allowed.
    let allow_origin = if config.allow_credentials {
        origin.to_string()
    } else if config.allowed_origins.iter().any(|o| o == "*") {
        "*".to_string()
    } else {
        origin.to_string()
    };
    resp_headers.push(("access-control-allow-origin".to_string(), allow_origin));

    // Allowed methods.
    let methods_str: String = config
        .allowed_methods
        .iter()
        .map(CorsHttpMethod::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    resp_headers.push(("access-control-allow-methods".to_string(), methods_str));

    // Allowed headers.
    if !config.allowed_headers.is_empty() {
        resp_headers.push((
            "access-control-allow-headers".to_string(),
            config.allowed_headers.join(", "),
        ));
    }

    // Max-Age.
    resp_headers.push((
        "access-control-max-age".to_string(),
        config.max_age.to_string(),
    ));

    // Credentials.
    if config.allow_credentials {
        resp_headers.push((
            "access-control-allow-credentials".to_string(),
            "true".to_string(),
        ));
    }

    // Vary (prevent cache poisoning).
    resp_headers.push((
        "vary".to_string(),
        "Origin, Access-Control-Request-Method, Access-Control-Request-Headers".to_string(),
    ));

    Ok(resp_headers)
}

// ---------------------------------------------------------------------------
// Actual request CORS headers
// ---------------------------------------------------------------------------

/// Produce CORS headers for an actual (non-preflight) cross-origin request.
///
/// Returns an empty vector if the origin is not in the allowed list, which
/// means the browser will block the response (no CORS headers = CORS failure).
#[must_use]
pub fn apply_cors_headers(config: &CorsConfig, origin: &str) -> Vec<(String, String)> {
    if !is_origin_allowed(config, origin) {
        return Vec::new();
    }

    let mut headers = Vec::new();

    // Allow-Origin: reflect or wildcard.
    let allow_origin = if config.allow_credentials {
        origin.to_string()
    } else if config.allowed_origins.iter().any(|o| o == "*") {
        "*".to_string()
    } else {
        origin.to_string()
    };
    headers.push(("access-control-allow-origin".to_string(), allow_origin));

    // Expose-Headers.
    if !config.expose_headers.is_empty() {
        headers.push((
            "access-control-expose-headers".to_string(),
            config.expose_headers.join(", "),
        ));
    }

    // Credentials.
    if config.allow_credentials {
        headers.push((
            "access-control-allow-credentials".to_string(),
            "true".to_string(),
        ));
    }

    // Vary (prevent cache poisoning).
    headers.push(("vary".to_string(), "Origin".to_string()));

    headers
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check whether the given origin is in the `allowed_origins` list.
///
/// Supports exact string match and the wildcard `"*"`.
fn is_origin_allowed(config: &CorsConfig, origin: &str) -> bool {
    config
        .allowed_origins
        .iter()
        .any(|o| o == "*" || o == origin)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::SharingMode;

    fn make_config() -> CorsConfig {
        CorsConfig {
            sharing: SharingMode::Private,
            enabled: true,
            allowed_origins: vec!["https://example.com".to_string()],
            allowed_methods: vec![CorsHttpMethod::Get, CorsHttpMethod::Post],
            allowed_headers: vec!["content-type".to_string(), "authorization".to_string()],
            expose_headers: vec!["x-request-id".to_string()],
            max_age: 3600,
            allow_credentials: false,
        }
    }

    // -- validate_cors_config --

    #[test]
    fn test_validate_valid_config_accepted() {
        let config = make_config();
        assert!(validate_cors_config(&config).is_ok());
    }

    #[test]
    fn test_validate_credentials_with_wildcard_rejected() {
        let config = CorsConfig {
            allow_credentials: true,
            allowed_origins: vec!["*".to_string()],
            ..make_config()
        };
        let err = validate_cors_config(&config).unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }

    #[test]
    fn test_validate_max_age_boundary() {
        let mut config = make_config();
        config.max_age = MAX_AGE_LIMIT;
        assert!(validate_cors_config(&config).is_ok());

        config.max_age = MAX_AGE_LIMIT + 1;
        assert!(validate_cors_config(&config).is_err());
    }

    #[test]
    fn test_validate_invalid_origin_rejected() {
        let config = CorsConfig {
            allowed_origins: vec!["not-a-url".to_string()],
            ..make_config()
        };
        assert!(validate_cors_config(&config).is_err());
    }

    #[test]
    fn test_validate_wildcard_origin_accepted() {
        let config = CorsConfig {
            allowed_origins: vec!["*".to_string()],
            ..make_config()
        };
        assert!(validate_cors_config(&config).is_ok());
    }

    // -- is_cors_preflight --

    #[test]
    fn test_is_cors_preflight_true() {
        let headers = vec![
            ("origin".to_string(), "https://example.com".to_string()),
            (
                "access-control-request-method".to_string(),
                "POST".to_string(),
            ),
        ];
        assert!(is_cors_preflight("OPTIONS", &headers));
    }

    #[test]
    fn test_is_cors_preflight_wrong_method() {
        let headers = vec![
            ("origin".to_string(), "https://example.com".to_string()),
            (
                "access-control-request-method".to_string(),
                "POST".to_string(),
            ),
        ];
        assert!(!is_cors_preflight("GET", &headers));
    }

    #[test]
    fn test_is_cors_preflight_missing_origin() {
        let headers = vec![(
            "access-control-request-method".to_string(),
            "POST".to_string(),
        )];
        assert!(!is_cors_preflight("OPTIONS", &headers));
    }

    // -- handle_cors_preflight --

    #[test]
    fn test_preflight_valid_origin_returns_headers() {
        let config = make_config();
        let result =
            handle_cors_preflight(&config, "https://example.com", "GET", &[], "/test").unwrap();
        let origin_header = result
            .iter()
            .find(|(k, _)| k == "access-control-allow-origin")
            .unwrap();
        assert_eq!(origin_header.1, "https://example.com");

        let vary = result.iter().find(|(k, _)| k == "vary").unwrap();
        assert!(vary.1.contains("Origin"));
    }

    #[test]
    fn test_preflight_invalid_origin_returns_error() {
        let config = make_config();
        let err =
            handle_cors_preflight(&config, "https://evil.com", "GET", &[], "/test").unwrap_err();
        assert!(matches!(err, DomainError::CorsOriginNotAllowed { .. }));
    }

    #[test]
    fn test_preflight_invalid_method_returns_error() {
        let config = make_config();
        let err = handle_cors_preflight(&config, "https://example.com", "DELETE", &[], "/test")
            .unwrap_err();
        assert!(matches!(err, DomainError::CorsMethodNotAllowed { .. }));
    }

    #[test]
    fn test_preflight_invalid_header_returns_error() {
        let config = make_config();
        let headers = vec!["x-custom-header".to_string()];
        let err = handle_cors_preflight(&config, "https://example.com", "GET", &headers, "/test")
            .unwrap_err();
        assert!(matches!(err, DomainError::CorsHeaderNotAllowed { .. }));
    }

    // -- apply_cors_headers --

    #[test]
    fn test_actual_request_cors_headers() {
        let config = make_config();
        let headers = apply_cors_headers(&config, "https://example.com");
        assert!(!headers.is_empty());

        let origin = headers
            .iter()
            .find(|(k, _)| k == "access-control-allow-origin")
            .unwrap();
        assert_eq!(origin.1, "https://example.com");

        let expose = headers
            .iter()
            .find(|(k, _)| k == "access-control-expose-headers")
            .unwrap();
        assert_eq!(expose.1, "x-request-id");
    }

    #[test]
    fn test_actual_request_disallowed_origin_no_headers() {
        let config = make_config();
        let headers = apply_cors_headers(&config, "https://evil.com");
        assert!(headers.is_empty());
    }

    // -- Origin matching --

    #[test]
    fn test_wildcard_origin_allows_any() {
        let config = CorsConfig {
            allowed_origins: vec!["*".to_string()],
            ..make_config()
        };
        let headers = apply_cors_headers(&config, "https://anything.com");
        let origin = headers
            .iter()
            .find(|(k, _)| k == "access-control-allow-origin")
            .unwrap();
        assert_eq!(origin.1, "*");
    }

    #[test]
    fn test_origin_matching_port_sensitive() {
        let config = CorsConfig {
            allowed_origins: vec!["https://example.com".to_string()],
            ..make_config()
        };
        // Different port should not match.
        assert!(apply_cors_headers(&config, "https://example.com:8443").is_empty());
    }

    #[test]
    fn test_origin_matching_protocol_sensitive() {
        let config = CorsConfig {
            allowed_origins: vec!["https://example.com".to_string()],
            ..make_config()
        };
        // Different protocol should not match.
        assert!(apply_cors_headers(&config, "http://example.com").is_empty());
    }

    // -- is_valid_origin --

    #[test]
    fn test_valid_origins() {
        assert!(is_valid_origin("https://example.com"));
        assert!(is_valid_origin("http://localhost"));
        assert!(is_valid_origin("https://example.com:8443"));
        assert!(is_valid_origin("http://127.0.0.1:3000"));
    }

    #[test]
    fn test_invalid_origins() {
        assert!(!is_valid_origin("example.com"));
        assert!(!is_valid_origin("ftp://example.com"));
        assert!(!is_valid_origin("https://"));
        assert!(!is_valid_origin("https://example.com:notaport"));
        assert!(!is_valid_origin(""));
    }

    #[test]
    fn test_valid_ipv6_origins() {
        assert!(is_valid_origin("http://[::1]"));
        assert!(is_valid_origin("http://[::1]:8080"));
        assert!(is_valid_origin("https://[::1]:443"));
        assert!(is_valid_origin("http://[2001:db8::1]"));
        assert!(is_valid_origin("http://[2001:db8::1]:3000"));
        assert!(is_valid_origin("http://[0:0:0:0:0:0:0:1]"));
    }

    #[test]
    fn test_invalid_ipv6_origins() {
        assert!(!is_valid_origin("http://[::1]:notaport"));
        assert!(!is_valid_origin("http://[::1]:"));
        assert!(!is_valid_origin("http://[not-ipv6]"));
        assert!(!is_valid_origin("http://[::1"));
        assert!(!is_valid_origin("http://::1"));
        assert!(!is_valid_origin("http://[]"));
        assert!(!is_valid_origin("http://[::1]:99999"));
    }

    #[test]
    fn test_validate_config_with_ipv6_origin() {
        let config = CorsConfig {
            allowed_origins: vec!["http://[::1]:8080".to_string()],
            ..make_config()
        };
        assert!(validate_cors_config(&config).is_ok());
    }
}
