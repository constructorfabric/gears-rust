use axum::extract::Request;
use axum::response::Response;
use http::HeaderValue;
use http::header::WWW_AUTHENTICATE;

/// An RFC 6750 §3 `WWW-Authenticate: Bearer` challenge.
///
/// The `Bearer` scheme MUST be followed by at least one auth-param, so there is
/// deliberately no bare-`Bearer` variant — each variant below carries exactly
/// the params RFC 6750 prescribes for its situation.
#[derive(Clone, Copy, Debug)]
pub enum BearerChallenge {
    /// A token was presented but rejected — RFC 6750 §3.1 `invalid_token`. Only
    /// the `error` code is sent; `realm`/`error_description` are omitted so the
    /// challenge discloses no internal detail.
    InvalidToken,
    /// A valid token lacked the required scope — RFC 6750 §3.1
    /// `insufficient_scope`.
    InsufficientScope,
    /// No credentials were presented. §3 says the server SHOULD NOT disclose an
    /// `error` code in that case, so `realm` is the sole auth-param — it meets
    /// the "one or more" rule without leaking any detail.
    NoCredentials,
}

impl BearerChallenge {
    fn header_value(self) -> &'static str {
        match self {
            Self::InvalidToken => r#"Bearer error="invalid_token""#,
            Self::InsufficientScope => r#"Bearer error="insufficient_scope""#,
            Self::NoCredentials => r#"Bearer realm="api""#,
        }
    }
}

/// Append `challenge` to the response as a `WWW-Authenticate` header.
pub fn append_bearer_challenge(response: &mut Response, challenge: BearerChallenge) {
    response.headers_mut().append(
        WWW_AUTHENTICATE,
        HeaderValue::from_static(challenge.header_value()),
    );
}

pub fn resolve_path(req: &Request, matched_path: &str) -> String {
    req.extensions()
        .get::<axum::extract::NestedPath>()
        .and_then(|np| strip_path_prefix(matched_path, np.as_str()))
        .unwrap_or_else(|| matched_path.to_owned())
}

/// Strip `prefix` from `path` only at a segment boundary.
///
/// Returns `None` when the prefix doesn't match.  When it does match the
/// result always starts with `/` (or is `/` when the path equals the prefix).
fn strip_path_prefix(path: &str, prefix: &str) -> Option<String> {
    let rest = path.strip_prefix(prefix)?;
    if rest.is_empty() {
        // path == prefix exactly  →  root
        Some("/".to_owned())
    } else if rest.starts_with('/') {
        // clean segment boundary  →  keep the slash
        Some(rest.to_owned())
    } else {
        // partial segment overlap (e.g. prefix="/cf", path="/cfish")  →  no match
        None
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    fn challenge_header(challenge: BearerChallenge) -> String {
        let mut response = axum::http::StatusCode::UNAUTHORIZED.into_response();
        append_bearer_challenge(&mut response, challenge);
        response
            .headers()
            .get(WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn error_challenges_carry_their_rfc_error_code() {
        assert_eq!(
            challenge_header(BearerChallenge::InvalidToken),
            r#"Bearer error="invalid_token""#
        );
        assert_eq!(
            challenge_header(BearerChallenge::InsufficientScope),
            r#"Bearer error="insufficient_scope""#
        );
    }

    #[test]
    fn no_credentials_challenge_omits_error_code() {
        let value = challenge_header(BearerChallenge::NoCredentials);
        assert_eq!(value, r#"Bearer realm="api""#);
        // RFC 6750 §3: when no token was supplied the challenge must not
        // disclose an `error` code, but still needs an auth-param (`realm`).
        assert!(
            !value.contains("error="),
            "no-credentials challenge leaked an error code"
        );
        assert!(
            value.starts_with("Bearer "),
            "challenge must carry an auth-param"
        );
    }

    #[test]
    fn exact_match_returns_root() {
        assert_eq!(strip_path_prefix("/cf", "/cf"), Some("/".to_owned()));
    }

    #[test]
    fn segment_boundary_strips_correctly() {
        assert_eq!(
            strip_path_prefix("/cf/users", "/cf"),
            Some("/users".to_owned())
        );
    }

    #[test]
    fn partial_segment_overlap_rejected() {
        assert_eq!(strip_path_prefix("/cfish", "/cf"), None);
    }

    #[test]
    fn no_prefix_match_returns_none() {
        assert_eq!(strip_path_prefix("/other/path", "/cf"), None);
    }

    #[test]
    fn nested_prefix_strips_correctly() {
        assert_eq!(
            strip_path_prefix("/api/v1/users", "/api/v1"),
            Some("/users".to_owned())
        );
    }

    #[test]
    fn path_with_params_strips_correctly() {
        assert_eq!(
            strip_path_prefix("/cf/users/{id}", "/cf"),
            Some("/users/{id}".to_owned())
        );
    }

    #[test]
    fn empty_prefix_returns_full_path() {
        assert_eq!(strip_path_prefix("/users", ""), Some("/users".to_owned()));
    }
}
