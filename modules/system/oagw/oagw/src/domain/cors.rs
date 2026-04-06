//! CORS domain logic: validation, preflight handling, and response header injection.
//!
//! All functions are pure domain logic with no infrastructure dependencies.

use super::error::DomainError;
use super::model::{CorsConfig, CorsHttpMethod};

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
// Actual request CORS enforcement
// ---------------------------------------------------------------------------

/// Check whether the request method is in the `allowed_methods` list.
pub fn is_method_allowed(config: &CorsConfig, method: &str) -> bool {
    CorsHttpMethod::from_str_loose(method).is_some_and(|m| config.allowed_methods.contains(&m))
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
pub fn is_origin_allowed(config: &CorsConfig, origin: &str) -> bool {
    config
        .allowed_origins
        .iter()
        .any(|o| o == "*" || o == origin)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "cors_tests.rs"]
mod cors_tests;
