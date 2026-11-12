//! Wire `reason` vocabulary for field violations under
//! [`CanonicalError::InvalidArgument`].
//!
//! Each constant is a stable machine-readable code that lands in
//! `CanonicalError::InvalidArgument.ctx.field_violations[].reason`
//! when the OAGW data-plane or control-plane rejects a request field.
//!
//! Consumers dispatch on these to disposition errors precisely without
//! string-typing literals. The impl crate references the same constants
//! at construction time so the SDK vocabulary and the wire never drift.
//!
//! [`CanonicalError::InvalidArgument`]: toolkit_canonical_errors::CanonicalError::InvalidArgument

// ---------------------------------------------------------------------------
// Generic
// ---------------------------------------------------------------------------

/// Generic catch-all when the construction site cannot pin a specific
/// reason. Prefer one of the more specific codes below where possible.
pub const VALIDATION: &str = "VALIDATION";

/// A required field is absent or empty.
pub const REQUIRED: &str = "REQUIRED";

/// The upstream's authentication plugin configuration is malformed —
/// e.g. an apikey plugin missing its required header binding, or a
/// bearer plugin missing its credential reference. Treat as a
/// deployment failure (consumer cannot fix by retrying).
pub const INVALID_PLUGIN_CONFIG: &str = "INVALID_PLUGIN_CONFIG";

// ---------------------------------------------------------------------------
// Target-host header (multi-endpoint upstream routing)
// ---------------------------------------------------------------------------

/// `x-target-host` header is required but missing for a multi-endpoint
/// upstream that needs explicit endpoint selection.
pub const MISSING_TARGET_HOST: &str = "MISSING_TARGET_HOST";

/// `x-target-host` header value is syntactically malformed.
pub const INVALID_TARGET_HOST: &str = "INVALID_TARGET_HOST";

/// `x-target-host` header value does not match any endpoint registered
/// on the resolved upstream.
pub const UNKNOWN_TARGET_HOST: &str = "UNKNOWN_TARGET_HOST";

/// Typed view of the three `x-target-host` field-violation codes.
///
/// Co-located with the constants so a reader sees both the wire vocabulary
/// and the typed dispatch surface in one place. The projection in
/// `crate::error` uses [`Self::from_wire`] to populate
/// [`crate::ServiceGatewayError::InvalidTargetHost`].
///
/// `from_wire` returns `Option<Self>` because only three field-violation
/// `reason` codes are target-host codes — everything else falls through
/// to [`crate::ServiceGatewayError::Validation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetHostCode {
    /// `x-target-host` header is required but missing for a
    /// multi-endpoint upstream that needs explicit endpoint selection.
    Missing,
    /// `x-target-host` header value is syntactically malformed.
    Invalid,
    /// `x-target-host` header value does not match any endpoint
    /// registered on the resolved upstream.
    Unknown,
}

impl TargetHostCode {
    /// Project a wire `field_violations[].reason` string into the typed
    /// discriminator. Returns `None` for any value that is not one of the
    /// three target-host codes.
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            MISSING_TARGET_HOST => Some(Self::Missing),
            INVALID_TARGET_HOST => Some(Self::Invalid),
            UNKNOWN_TARGET_HOST => Some(Self::Unknown),
            _ => None,
        }
    }

    /// Render the discriminator back to its wire `reason` string.
    /// Inverse of [`Self::from_wire`].
    #[must_use]
    pub fn as_wire(&self) -> &'static str {
        match self {
            Self::Missing => MISSING_TARGET_HOST,
            Self::Invalid => INVALID_TARGET_HOST,
            Self::Unknown => UNKNOWN_TARGET_HOST,
        }
    }
}

// ---------------------------------------------------------------------------
// Body / payload
// ---------------------------------------------------------------------------

/// Request body exceeds the configured size limit.
pub const PAYLOAD_TOO_LARGE: &str = "PAYLOAD_TOO_LARGE";

// ---------------------------------------------------------------------------
// Header validation
// ---------------------------------------------------------------------------

/// `Content-Type` (or other media-type header) carries a value that
/// fails MIME-type parsing.
pub const INVALID_MIME_TYPE: &str = "INVALID_MIME_TYPE";

/// A header value fails its specific syntactic validator (numeric
/// range, enum membership, etc.).
pub const INVALID_VALUE: &str = "INVALID_VALUE";

/// A header that must appear at most once is duplicated, or
/// mutually-exclusive headers (e.g. `Content-Length` + `Transfer-Encoding`)
/// were both supplied — a request-smuggling protection.
pub const DUPLICATE_HEADER: &str = "DUPLICATE_HEADER";

// ---------------------------------------------------------------------------
// GTS identifier validation
// ---------------------------------------------------------------------------

/// A GTS identifier field is malformed — wrong segment count, missing
/// trailing `~`, illegal characters.
pub const INVALID_GTS_FORMAT: &str = "INVALID_GTS_FORMAT";

/// A GTS identifier trailer is not a syntactically valid UUID.
pub const INVALID_GTS_UUID: &str = "INVALID_GTS_UUID";

/// A GTS schema identifier (resource type prefix) is unrecognized or
/// malformed at the OAGW boundary.
pub const INVALID_GTS_SCHEMA: &str = "INVALID_GTS_SCHEMA";

/// A GTS resource-type identifier is missing its trailing `~` separator.
pub const MISSING_GTS_TILDE: &str = "MISSING_GTS_TILDE";

// ---------------------------------------------------------------------------
// CORS configuration validation
// ---------------------------------------------------------------------------

/// A CORS `allowed_origins` entry is not a valid origin URL.
pub const INVALID_CORS_ORIGIN: &str = "INVALID_CORS_ORIGIN";

/// CORS `allow_credentials = true` was combined with the wildcard
/// `*` origin, which the CORS spec forbids.
pub const CORS_CREDENTIALS_WITH_WILDCARD: &str = "CORS_CREDENTIALS_WITH_WILDCARD";

// ---------------------------------------------------------------------------
// WebSocket upgrade preconditions
// ---------------------------------------------------------------------------

/// WebSocket upgrade request did not use HTTP GET.
pub const WS_UPGRADE_REQUIRES_GET: &str = "WS_UPGRADE_REQUIRES_GET";

/// WebSocket upgrade request carried a body, which the protocol forbids.
pub const WS_UPGRADE_BODY_FORBIDDEN: &str = "WS_UPGRADE_BODY_FORBIDDEN";

// ---------------------------------------------------------------------------
// Proxy request validation
// ---------------------------------------------------------------------------

/// The request URI does not follow the `/<alias>/<path_suffix>` convention.
pub const INVALID_PROXY_PATH: &str = "INVALID_PROXY_PATH";

/// The request URI lacks an upstream alias segment (e.g. raw `/`).
pub const MISSING_ALIAS: &str = "MISSING_ALIAS";

/// Reading the request body failed for a reason other than its size, for
/// example the client aborted the upload.
pub const BODY_READ_FAILED: &str = "BODY_READ_FAILED";

/// The route does not allow query parameters but the request supplied
/// one or more.
pub const QUERY_NOT_ALLOWED: &str = "QUERY_NOT_ALLOWED";

/// The route does not allow path-suffix routing but the request URI
/// includes a suffix beyond the alias.
pub const PATH_SUFFIX_NOT_ALLOWED: &str = "PATH_SUFFIX_NOT_ALLOWED";

/// The upstream endpoint scheme is not supported by the configured
/// upstream protocol (e.g. `wss://` on an HTTP-only upstream).
pub const UNSUPPORTED_SCHEME: &str = "UNSUPPORTED_SCHEME";

/// The configured upstream endpoint uses `http://` in a configuration
/// that requires TLS — refused by SSRF protection.
pub const HTTP_UPSTREAM_FORBIDDEN: &str = "HTTP_UPSTREAM_FORBIDDEN";

/// `Content-Length` header is present but fails numeric parsing or is
/// inconsistent with the body.
pub const INVALID_CONTENT_LENGTH: &str = "INVALID_CONTENT_LENGTH";

/// The proxy pipeline could not produce a valid upstream URI from the
/// configured rewrite rules (resulting URI fails `http::Uri` parsing).
pub const INVALID_REWRITTEN_URI: &str = "INVALID_REWRITTEN_URI";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_host_code_round_trips() {
        for (wire, expected) in [
            (MISSING_TARGET_HOST, TargetHostCode::Missing),
            (INVALID_TARGET_HOST, TargetHostCode::Invalid),
            (UNKNOWN_TARGET_HOST, TargetHostCode::Unknown),
        ] {
            assert_eq!(TargetHostCode::from_wire(wire), Some(expected.clone()));
            assert_eq!(expected.as_wire(), wire);
        }
    }

    #[test]
    fn target_host_code_rejects_unrelated_codes() {
        assert!(TargetHostCode::from_wire(PAYLOAD_TOO_LARGE).is_none());
        assert!(TargetHostCode::from_wire("").is_none());
        assert!(TargetHostCode::from_wire("CUSTOM_CODE").is_none());
    }
}
