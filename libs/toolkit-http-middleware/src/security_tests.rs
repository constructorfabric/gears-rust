use super::*;
use secrecy::ExposeSecret;

#[test]
fn extract_missing_header() {
    let headers = HeaderMap::new();
    assert_eq!(
        extract_bearer_http(&headers).unwrap_err(),
        SecurityContextHttpError::MissingAuthHeader
    );
}

#[test]
fn extract_case_insensitive_scheme() {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("bEaReR my-token"));
    assert_eq!(
        extract_bearer_http(&headers).unwrap().expose_secret(),
        "my-token"
    );
}

#[test]
fn extract_trims_surrounding_whitespace() {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static("  Bearer   my-token  "),
    );
    assert_eq!(
        extract_bearer_http(&headers).unwrap().expose_secret(),
        "my-token"
    );
}

#[test]
fn extract_wrong_scheme() {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static("Basic dXNlcjpwYXNz"),
    );
    assert_eq!(
        extract_bearer_http(&headers).unwrap_err(),
        SecurityContextHttpError::InvalidAuthHeader
    );
}

#[test]
fn extract_no_separator() {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer"));
    assert_eq!(
        extract_bearer_http(&headers).unwrap_err(),
        SecurityContextHttpError::InvalidAuthHeader
    );
}

#[test]
fn extract_empty_token() {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer    "));
    assert_eq!(
        extract_bearer_http(&headers).unwrap_err(),
        SecurityContextHttpError::EmptyToken
    );
}

#[test]
fn extract_bearer_extracts_token() {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer header.payload.signature"),
    );
    assert_eq!(
        extract_bearer_http(&headers).unwrap().expose_secret(),
        "header.payload.signature"
    );
}

#[test]
fn extract_rejects_duplicate_authorization() {
    let mut headers = HeaderMap::new();
    headers.append(AUTHORIZATION, HeaderValue::from_static("Bearer token-a"));
    headers.append(AUTHORIZATION, HeaderValue::from_static("Bearer token-b"));
    assert_eq!(
        extract_bearer_http(&headers).unwrap_err(),
        SecurityContextHttpError::InvalidAuthHeader
    );
}

#[test]
fn extract_internal_token_extracts_token() {
    let mut headers = HeaderMap::new();
    headers.insert(
        INTERNAL_TOKEN_HEADER,
        HeaderValue::from_static("sa.jwt.token"),
    );
    assert_eq!(
        extract_internal_token_http(&headers)
            .unwrap()
            .expose_secret(),
        "sa.jwt.token"
    );
}

#[test]
fn extract_internal_token_missing_header() {
    let headers = HeaderMap::new();
    assert_eq!(
        extract_internal_token_http(&headers).unwrap_err(),
        InternalTokenHttpError::MissingHeader
    );
}

#[test]
fn extract_internal_token_trims_whitespace() {
    let mut headers = HeaderMap::new();
    headers.insert(INTERNAL_TOKEN_HEADER, HeaderValue::from_static("  tok  "));
    assert_eq!(
        extract_internal_token_http(&headers)
            .unwrap()
            .expose_secret(),
        "tok"
    );
}

#[test]
fn extract_internal_token_empty() {
    let mut headers = HeaderMap::new();
    headers.insert(INTERNAL_TOKEN_HEADER, HeaderValue::from_static("   "));
    assert_eq!(
        extract_internal_token_http(&headers).unwrap_err(),
        InternalTokenHttpError::EmptyToken
    );
}

#[test]
fn extract_internal_token_rejects_duplicates() {
    let mut headers = HeaderMap::new();
    headers.append(INTERNAL_TOKEN_HEADER, HeaderValue::from_static("tok-a"));
    headers.append(INTERNAL_TOKEN_HEADER, HeaderValue::from_static("tok-b"));
    assert_eq!(
        extract_internal_token_http(&headers).unwrap_err(),
        InternalTokenHttpError::InvalidHeader
    );
}
