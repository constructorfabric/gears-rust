use thiserror::Error;

/// Errors returned by the outbound `OAuth2` client credentials flow.
///
/// All variants are deliberately constructed so that secret values
/// (`client_secret`, access tokens) can never appear in the formatted output.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TokenError {
    /// HTTP transport or status error during token acquisition.
    ///
    /// The inner string is produced by
    /// [`format_http_error`](crate::http_error::format_http_error) and never
    /// contains secrets.
    #[error("{0}")]
    Http(String),

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

    /// The token watcher is not ready or has been shut down.
    #[error("token unavailable: {0}")]
    Unavailable(String),
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "error_tests.rs"]
mod tests;
