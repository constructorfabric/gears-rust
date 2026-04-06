use super::*;

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
