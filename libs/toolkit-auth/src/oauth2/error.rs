use thiserror::Error;

/// HTTP operation that failed during outbound authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HttpOperation {
    /// Fetching the OIDC discovery document.
    Discovery,
    /// Exchanging client credentials for a token.
    TokenRequest,
}

impl HttpOperation {
    /// Human-readable context for diagnostics; callers should match the enum.
    fn label(self) -> &'static str {
        match self {
            Self::Discovery => "OIDC discovery",
            Self::TokenRequest => "OAuth2 token",
        }
    }
}

impl std::fmt::Display for HttpOperation {
    /// Format the operation without request URLs or credentials.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Errors returned by the outbound `OAuth2` client credentials flow.
///
/// All variants are deliberately constructed so that secret values
/// (`client_secret`, access tokens) can never appear in the formatted output.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum TokenError {
    /// HTTP client or transport error during token acquisition.
    ///
    /// The inner string is produced by
    /// [`format_http_error`](crate::http_error::format_http_error) and never
    /// contains secrets.
    #[error("{0}")]
    Http(String),

    /// An HTTP response rejected token acquisition or OIDC discovery.
    /// Response bodies and headers are deliberately not retained.
    #[error("{operation} HTTP {status} ({target_url})")]
    #[non_exhaustive]
    HttpStatus {
        /// Operation that failed.
        operation: HttpOperation,
        /// Status returned by the remote endpoint.
        status: http::StatusCode,
        /// Request URL with userinfo, query and fragment removed.
        target_url: String,
    },

    /// The token endpoint returned an unparseable or incomplete response.
    #[error("invalid token response: {0}")]
    InvalidResponse(String),

    /// The token endpoint returned a `token_type` that is not `Bearer`.
    #[error("unsupported token type: {0}")]
    UnsupportedTokenType(String),

    /// Configuration is invalid (e.g. both `token_endpoint` and `issuer_url`
    /// are set, or neither is set).
    #[error("OAuth2 config error: {0}")]
    ConfigError(String),

    /// The provider has been permanently stopped.
    #[error("token provider shut down")]
    Shutdown,

    /// No usable token can be returned.
    #[error("token unavailable: {0}")]
    Unavailable(String),

    /// The token endpoint returned a token with an invalid lifetime or
    /// freshness ratio.
    #[error("invalid token lifetime: {0}")]
    InvalidTokenLifetime(String),
}

impl TokenError {
    /// HTTP response status, when the failure came from a rejected response.
    #[must_use]
    pub fn status(&self) -> Option<http::StatusCode> {
        match self {
            Self::HttpStatus { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// Preserve response status while discarding response bodies and headers.
    /// Other failures use the shared HTTP error formatter.
    pub(crate) fn from_http(
        error: &toolkit_http::HttpError,
        operation: HttpOperation,
        target: &url::Url,
    ) -> Self {
        match error {
            toolkit_http::HttpError::HttpStatus { status, .. } => Self::HttpStatus {
                operation,
                status: *status,
                target_url: sanitized_target(target),
            },
            _ => Self::Http(crate::http_error::format_http_error(
                error,
                operation.label(),
            )),
        }
    }
}

/// Retain endpoint identity without URL credentials or sensitive parameters.
fn sanitized_target(target: &url::Url) -> String {
    let mut clean = target.clone();
    clean.set_username("").unwrap_or_default();
    clean.set_password(None).unwrap_or_default();
    clean.set_query(None);
    clean.set_fragment(None);
    clean.to_string()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// Verify typed statuses retain no response diagnostics and transport errors have no status.
    #[test]
    fn status_is_typed_and_response_diagnostics_are_discarded() {
        for code in [400, 401, 403, 404, 429, 500, 503] {
            let status = http::StatusCode::from_u16(code).unwrap();
            let error = TokenError::from_http(
                &toolkit_http::HttpError::HttpStatus {
                    status,
                    body_preview: "secret-token".into(),
                    content_type: Some("secret-header".into()),
                    retry_after: None,
                },
                HttpOperation::TokenRequest,
                &url::Url::parse("https://user:secret@example.com/oauth/token?key=secret#secret")
                    .unwrap(),
            );
            assert_eq!(error.status(), Some(status));
            assert!(
                matches!(&error, TokenError::HttpStatus { target_url, .. } if target_url == "https://example.com/oauth/token")
            );
            assert!(!format!("{error:?} {error}").contains("secret"));
        }
        let error = TokenError::from_http(
            &toolkit_http::HttpError::Timeout(std::time::Duration::from_secs(1)),
            HttpOperation::TokenRequest,
            &url::Url::parse("https://example.com/oauth/token").unwrap(),
        );
        assert_eq!(error.status(), None);
    }

    #[test]
    fn config_error_renders() {
        let e = TokenError::ConfigError("both endpoints set".into());
        assert_eq!(e.to_string(), "OAuth2 config error: both endpoints set");
    }

    #[test]
    fn http_error_renders() {
        let e = TokenError::Http("OAuth2 token HTTP 401 Unauthorized".into());
        assert_eq!(e.to_string(), "OAuth2 token HTTP 401 Unauthorized");
    }

    #[test]
    fn invalid_response_renders() {
        let e = TokenError::InvalidResponse("missing access_token".into());
        assert_eq!(
            e.to_string(),
            "invalid token response: missing access_token"
        );
    }

    #[test]
    fn unsupported_token_type_renders() {
        let e = TokenError::UnsupportedTokenType("mac".into());
        assert_eq!(e.to_string(), "unsupported token type: mac");
    }

    #[test]
    fn unavailable_renders() {
        let e = TokenError::Unavailable("watcher shut down".into());
        assert_eq!(e.to_string(), "token unavailable: watcher shut down");
    }

    #[test]
    fn invalid_token_lifetime_renders() {
        let e = TokenError::InvalidTokenLifetime("lifetime_secs must be > 0".into());
        assert_eq!(
            e.to_string(),
            "invalid token lifetime: lifetime_secs must be > 0"
        );
    }
}
