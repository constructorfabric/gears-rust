//! Interactive `OAuth2` authorization-code flow client (per-user).
//!
//! Unlike the client-credentials path elsewhere in this module, this
//! implements the browser-driven authorization-code flow used to obtain
//! per-user tokens for corporate resources (e.g. MCP servers behind OAGW):
//!
//! - Metadata discovery: RFC 9728 protected-resource metadata to locate the
//!   authorization server, then RFC 8414 authorization-server metadata.
//! - Dynamic Client Registration (RFC 7591) for public clients.
//! - PKCE (RFC 7636, S256) challenge/verifier generation.
//! - Authorization-request URL construction.
//! - Authorization-code exchange and refresh-token rotation, with a distinct
//!   [`TokenError::RefreshRejected`] so callers can trigger re-authorization
//!   instead of retrying.
//!
//! All secret material is wrapped in [`SecretString`] and never logged.

use std::fmt;
use std::time::Duration;

// The single SHA-256 call site in this crate: the PKCE (RFC 7636) `S256`
// code-challenge `BASE64URL(SHA256(verifier))`. It routes through the
// FIPS-validated `aws-lc-rs` provider (the same crypto backend the platform
// installs at bootstrap) rather than the pure-Rust RustCrypto `sha2`, so no
// non-FIPS hasher enters the graph (DE0708 clean, and no direct `sha2` dep for
// the FIPS dependency-policy gate).
use aws_lc_rs::digest::{SHA256, digest as sha256};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use toolkit_http::HttpClient;
use toolkit_utils::SecretString;
use url::Url;

use super::error::TokenError;
use crate::http_error::format_http_error;

/// Default access-token lifetime applied when the authorization server omits
/// `expires_in` from a token response.
const DEFAULT_TOKEN_TTL: Duration = Duration::from_hours(1);

// ---------------------------------------------------------------------------
// Metadata discovery
// ---------------------------------------------------------------------------

/// RFC 9728 protected-resource metadata (subset).
#[derive(Debug, Clone, Deserialize)]
pub struct ProtectedResourceMetadata {
    /// Authorization server issuer identifiers that can issue tokens for this
    /// resource. The first entry is used.
    #[serde(default)]
    pub authorization_servers: Vec<String>,
}

/// RFC 8414 authorization-server metadata (subset).
#[derive(Debug, Clone, Deserialize)]
pub struct AuthorizationServerMetadata {
    /// Authorization server issuer identifier.
    pub issuer: String,
    /// Endpoint used to build the browser authorization request.
    pub authorization_endpoint: String,
    /// Endpoint used for code exchange and refresh.
    pub token_endpoint: String,
    /// Optional RFC 7591 dynamic client registration endpoint.
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    /// Scopes advertised by the server; callers must not request scopes
    /// outside this list when present.
    #[serde(default)]
    pub scopes_supported: Option<Vec<String>>,
    /// PKCE code-challenge methods supported (should contain `S256`).
    #[serde(default)]
    pub code_challenge_methods_supported: Option<Vec<String>>,
}

/// Return `true` if `url`'s host is a loopback address, permitting the
/// `http` development exception (`localhost`, `127.0.0.0/8`, `::1`).
fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Validate a discovered OAuth endpoint before it is used: it must parse, use a
/// secure scheme (`https`, or `http` for the loopback dev exception), and share
/// the authorization server's `issuer` origin. This prevents OAuth mix-up /
/// SSRF where malicious metadata redirects the token exchange (auth code, PKCE
/// verifier, refresh token) to attacker-controlled or internal endpoints.
fn validate_discovered_endpoint(
    endpoint: &str,
    issuer: &Url,
    name: &str,
) -> Result<(), TokenError> {
    let url = Url::parse(endpoint)
        .map_err(|e| TokenError::InvalidResponse(format!("invalid {name} URL: {e}")))?;
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(&url) => {}
        other => {
            return Err(TokenError::InvalidResponse(format!(
                "{name} must use https (got scheme '{other}')"
            )));
        }
    }
    if url.origin() != issuer.origin() {
        return Err(TokenError::InvalidResponse(format!(
            "{name} origin does not match issuer origin"
        )));
    }
    Ok(())
}

/// Return the scheme/host/port origin of `url`, dropping path/query/fragment.
fn origin_of(url: &Url) -> Url {
    let mut origin = url.clone();
    origin.set_path("");
    origin.set_query(None);
    origin.set_fragment(None);
    origin
}

/// Build a metadata discovery URL by inserting `/.well-known/{segment}`
/// immediately after `url`'s authority and re-appending the original path
/// (RFC 8414 §3.1, RFC 9728 §3.1). Preserving the path ensures distinct
/// resource/issuer paths map to distinct discovery URLs.
fn well_known_url(url: &Url, segment: &str) -> String {
    let origin = origin_of(url);
    let authority = origin.as_str().trim_end_matches('/');
    // `Url::path` yields "/" for a root URL; trimming leaves it empty so the
    // well-known segment is not followed by a stray slash.
    let path = url.path().trim_end_matches('/');
    format!("{authority}/.well-known/{segment}{path}")
}

async fn fetch_json<T>(client: &HttpClient, url: &str, ctx: &str) -> Result<T, TokenError>
where
    T: serde::de::DeserializeOwned,
{
    client
        .get(url)
        .send()
        .await
        .map_err(|e| TokenError::Http(format_http_error(&e, ctx)))?
        .error_for_status()
        .map_err(|e| TokenError::Http(format_http_error(&e, ctx)))?
        .json()
        .await
        .map_err(|e| TokenError::InvalidResponse(format_http_error(&e, ctx)))
}

/// Fetch RFC 9728 protected-resource metadata for `resource_url`.
///
/// The `/.well-known/oauth-protected-resource` segment is inserted immediately
/// after the authority and the resource path (e.g. `/mcp`) is preserved, so
/// path-scoped resources map to distinct discovery URLs (RFC 9728 §3.1).
///
/// # Errors
///
/// Returns [`TokenError::Http`] on transport/status failures and
/// [`TokenError::InvalidResponse`] on unparseable bodies.
pub async fn discover_protected_resource(
    client: &HttpClient,
    resource_url: &Url,
) -> Result<ProtectedResourceMetadata, TokenError> {
    let url = well_known_url(resource_url, "oauth-protected-resource");
    fetch_json(client, &url, "OAuth protected-resource discovery").await
}

/// Fetch RFC 8414 authorization-server metadata for `issuer`.
///
/// # Errors
///
/// Returns [`TokenError::Http`] on transport/status failures,
/// [`TokenError::InvalidResponse`] on unparseable bodies,
/// [`TokenError::InvalidResponse`] if the returned `issuer` does not match the
/// requested issuer (RFC 8414 §3.3 issuer-consistency check), and
/// [`TokenError::InvalidResponse`] if any advertised endpoint does not use a
/// secure scheme or does not share the issuer origin.
pub async fn discover_authorization_server(
    client: &HttpClient,
    issuer: &Url,
) -> Result<AuthorizationServerMetadata, TokenError> {
    let url = well_known_url(issuer, "oauth-authorization-server");
    let metadata: AuthorizationServerMetadata =
        fetch_json(client, &url, "OAuth authorization-server discovery").await?;
    let issuer_id = issuer.as_str().trim_end_matches('/');
    if metadata.issuer.trim_end_matches('/') != issuer_id {
        return Err(TokenError::InvalidResponse(
            "authorization-server metadata issuer does not match requested issuer".to_owned(),
        ));
    }
    // Endpoints are attacker-influenced (via protected-resource metadata), so
    // require each to use a secure scheme and share the validated issuer origin
    // before any credentials are sent to them.
    validate_discovered_endpoint(
        &metadata.authorization_endpoint,
        issuer,
        "authorization_endpoint",
    )?;
    validate_discovered_endpoint(&metadata.token_endpoint, issuer, "token_endpoint")?;
    if let Some(registration_endpoint) = &metadata.registration_endpoint {
        validate_discovered_endpoint(registration_endpoint, issuer, "registration_endpoint")?;
    }
    Ok(metadata)
}

/// Resolve authorization-server metadata starting from a protected resource
/// URL: protected-resource metadata -> first authorization server -> its
/// authorization-server metadata.
///
/// # Errors
///
/// Returns [`TokenError::InvalidResponse`] if no authorization server is
/// advertised or the advertised URL is invalid, otherwise propagates the
/// discovery errors.
pub async fn discover_from_resource(
    client: &HttpClient,
    resource_url: &Url,
) -> Result<AuthorizationServerMetadata, TokenError> {
    let prm = discover_protected_resource(client, resource_url).await?;
    let issuer_str = prm
        .authorization_servers
        .into_iter()
        .next()
        .ok_or_else(|| {
            TokenError::InvalidResponse(
                "protected-resource metadata advertised no authorization_servers".to_owned(),
            )
        })?;
    let issuer = Url::parse(&issuer_str).map_err(|e| {
        TokenError::InvalidResponse(format!("invalid authorization server URL: {e}"))
    })?;
    discover_authorization_server(client, &issuer).await
}

/// Extract the `resource_metadata` parameter (RFC 9728 §5.1) from a
/// `WWW-Authenticate` challenge header value, if present.
///
/// Matches the parameter key case-insensitively (with a left word boundary so
/// `xresource_metadata` does not match) and accepts both the quoted
/// (`resource_metadata="https://..."`, the RFC form) and bare-token forms. The
/// returned value has surrounding quotes removed.
fn parse_resource_metadata_param(header: &str) -> Option<String> {
    const KEY: &str = "resource_metadata";
    let bytes = header.as_bytes();
    let lower = header.to_ascii_lowercase();
    let mut from = 0;
    while let Some(rel) = lower[from..].find(KEY) {
        let idx = from + rel;
        let after = idx + KEY.len();
        let boundary =
            idx == 0 || !(bytes[idx - 1].is_ascii_alphanumeric() || bytes[idx - 1] == b'_');
        if boundary {
            if let Some(value) = header[after..].trim_start().strip_prefix('=') {
                let value = value.trim_start();
                if let Some(quoted) = value.strip_prefix('"') {
                    if let Some(end) = quoted.find('"') {
                        return Some(quoted[..end].to_owned());
                    }
                } else {
                    let end = value
                        .find(|c: char| c == ',' || c.is_whitespace())
                        .unwrap_or(value.len());
                    if end > 0 {
                        return Some(value[..end].to_owned());
                    }
                }
            }
        }
        from = after;
    }
    None
}

/// Resolve authorization-server metadata by probing a protected resource that
/// advertises its metadata via an RFC 9728 §5.1 `WWW-Authenticate` challenge.
///
/// Sends an unauthenticated request to `probe_url`, reads the
/// `resource_metadata` parameter from the returned `WWW-Authenticate` header
/// (typically on a `401`), fetches that protected-resource metadata document,
/// and continues to authorization-server discovery. This is the fallback used
/// when the resource is not known ahead of time and does not expose the
/// well-known metadata path at a caller-supplied URL.
///
/// # Errors
///
/// Returns [`TokenError::Http`] on transport failures and
/// [`TokenError::InvalidResponse`] when the challenge is missing, carries no
/// `resource_metadata` parameter, names an insecure URL, or the discovered
/// documents are unparseable / advertise no authorization server.
pub async fn discover_from_resource_challenge(
    client: &HttpClient,
    probe_url: &Url,
) -> Result<AuthorizationServerMetadata, TokenError> {
    let response = client
        .get(probe_url.as_str())
        .send()
        .await
        .map_err(|e| TokenError::Http(format_http_error(&e, "OAuth resource probe")))?;
    let challenge = response
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            TokenError::InvalidResponse(
                "resource probe returned no WWW-Authenticate challenge".to_owned(),
            )
        })?;
    let metadata_url = parse_resource_metadata_param(challenge).ok_or_else(|| {
        TokenError::InvalidResponse(
            "WWW-Authenticate challenge has no resource_metadata parameter".to_owned(),
        )
    })?;
    let metadata_url = Url::parse(&metadata_url)
        .map_err(|e| TokenError::InvalidResponse(format!("invalid resource_metadata URL: {e}")))?;
    // The metadata URL is attacker-influenced (from the probed response), so it
    // must use a secure scheme before we fetch it.
    match metadata_url.scheme() {
        "https" => {}
        "http" if is_loopback_host(&metadata_url) => {}
        other => {
            return Err(TokenError::InvalidResponse(format!(
                "resource_metadata must use https (got scheme '{other}')"
            )));
        }
    }
    let prm: ProtectedResourceMetadata = fetch_json(
        client,
        metadata_url.as_str(),
        "OAuth protected-resource discovery",
    )
    .await?;
    let issuer_str = prm
        .authorization_servers
        .into_iter()
        .next()
        .ok_or_else(|| {
            TokenError::InvalidResponse(
                "protected-resource metadata advertised no authorization_servers".to_owned(),
            )
        })?;
    let issuer = Url::parse(&issuer_str).map_err(|e| {
        TokenError::InvalidResponse(format!("invalid authorization server URL: {e}"))
    })?;
    discover_authorization_server(client, &issuer).await
}

// ---------------------------------------------------------------------------
// PKCE
// ---------------------------------------------------------------------------

/// A PKCE (RFC 7636) verifier/challenge pair using the `S256` method.
pub struct Pkce {
    /// High-entropy verifier, sent with the code exchange. Kept secret.
    pub verifier: SecretString,
    /// `BASE64URL(SHA256(verifier))`, sent in the authorization request.
    pub challenge: String,
}

impl fmt::Debug for Pkce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pkce")
            .field("verifier", &"[REDACTED]")
            .field("challenge", &self.challenge)
            .finish()
    }
}

impl Pkce {
    /// Generate a fresh PKCE pair from 32 bytes of randomness.
    #[must_use]
    pub fn generate() -> Self {
        let raw: [u8; 32] = rand::random();
        let verifier = URL_SAFE_NO_PAD.encode(raw);
        let digest = sha256(&SHA256, verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(digest.as_ref());
        Self {
            verifier: SecretString::new(verifier),
            challenge,
        }
    }

    /// The code-challenge method identifier (`S256`).
    #[must_use]
    pub fn method(&self) -> &'static str {
        "S256"
    }
}

/// Generate an opaque, URL-safe CSRF `state` value (32 bytes of entropy).
#[must_use]
pub fn generate_state() -> String {
    let raw: [u8; 32] = rand::random();
    URL_SAFE_NO_PAD.encode(raw)
}

// ---------------------------------------------------------------------------
// Dynamic Client Registration (RFC 7591)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct RegistrationRequest<'a> {
    client_name: &'a str,
    redirect_uris: &'a [String],
    grant_types: Vec<&'a str>,
    response_types: Vec<&'a str>,
    token_endpoint_auth_method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
}

#[derive(Deserialize)]
struct RegistrationResponse {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
}

/// A client registered via RFC 7591 Dynamic Client Registration.
pub struct RegisteredClient {
    /// Issued client identifier.
    pub client_id: String,
    /// Issued client secret, if the server registered a confidential client.
    /// Public (PKCE-only) clients have `None`.
    pub client_secret: Option<SecretString>,
}

impl fmt::Debug for RegisteredClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegisteredClient")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

/// Register a public (PKCE) client via RFC 7591 Dynamic Client Registration.
///
/// # Errors
///
/// Returns [`TokenError::RegistrationFailed`] on non-success status and
/// [`TokenError::InvalidResponse`] if the response cannot be parsed.
pub async fn register_client(
    client: &HttpClient,
    registration_endpoint: &str,
    client_name: &str,
    redirect_uris: &[String],
    scopes: &[String],
) -> Result<RegisteredClient, TokenError> {
    let scope = if scopes.is_empty() {
        None
    } else {
        Some(scopes.join(" "))
    };
    let body = RegistrationRequest {
        client_name,
        redirect_uris,
        grant_types: vec!["authorization_code", "refresh_token"],
        response_types: vec!["code"],
        token_endpoint_auth_method: "none",
        scope,
    };

    let ctx = "OAuth dynamic client registration";
    let resp = client
        .post(registration_endpoint)
        .json(&body)
        .map_err(|e| TokenError::Http(format_http_error(&e, ctx)))?
        .send()
        .await
        .map_err(|e| TokenError::Http(format_http_error(&e, ctx)))?;

    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| TokenError::Http(format_http_error(&e, ctx)))?;
    if !status.is_success() {
        return Err(TokenError::RegistrationFailed(format!(
            "{ctx}: HTTP {}",
            status.as_u16()
        )));
    }

    let parsed: RegistrationResponse = serde_json::from_slice(&bytes)
        .map_err(|e| TokenError::InvalidResponse(format!("{ctx}: {e}")))?;
    Ok(RegisteredClient {
        client_id: parsed.client_id,
        client_secret: parsed.client_secret.map(SecretString::new),
    })
}

// ---------------------------------------------------------------------------
// Authorization request URL
// ---------------------------------------------------------------------------

/// Build the browser authorization-request URL (RFC 6749 §4.1.1 + PKCE).
///
/// Only scopes advertised by the server should be passed in `scopes`.
///
/// # Errors
///
/// Returns [`TokenError::ConfigError`] if `authorization_endpoint` is not a
/// valid URL, or if its scheme is neither `https` nor the permitted `http`
/// loopback development exception (`localhost`, `127.0.0.1`, `::1`).
pub fn build_authorize_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    scopes: &[String],
    state: &str,
    pkce_challenge: &str,
) -> Result<Url, TokenError> {
    let mut url = Url::parse(authorization_endpoint)
        .map_err(|e| TokenError::ConfigError(format!("invalid authorization_endpoint URL: {e}")))?;
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(&url) => {}
        other => {
            return Err(TokenError::ConfigError(format!(
                "authorization_endpoint must use https (got scheme '{other}')"
            )));
        }
    }
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code");
        q.append_pair("client_id", client_id);
        q.append_pair("redirect_uri", redirect_uri);
        q.append_pair("state", state);
        q.append_pair("code_challenge", pkce_challenge);
        q.append_pair("code_challenge_method", "S256");
        if !scopes.is_empty() {
            q.append_pair("scope", &scopes.join(" "));
        }
    }
    Ok(url)
}

// ---------------------------------------------------------------------------
// Token exchange / refresh
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AuthCodeTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

/// Tokens returned by an authorization-code exchange or refresh.
pub struct TokenSet {
    /// Bearer access token.
    pub access_token: SecretString,
    /// Refresh token, when the server issues one (or rotates it).
    pub refresh_token: Option<SecretString>,
    /// Access-token lifetime (server `expires_in`, or a default).
    pub expires_in: Duration,
    /// Granted scope, if the server echoed one.
    pub scope: Option<String>,
}

impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_in", &self.expires_in)
            .field("scope", &self.scope)
            .finish()
    }
}

/// Parameters for an authorization-code exchange.
pub struct AuthCodeExchange<'a> {
    /// Authorization-server token endpoint.
    pub token_endpoint: &'a str,
    /// Client identifier (from config or DCR).
    pub client_id: &'a str,
    /// Client secret for confidential clients; `None` for public/PKCE clients.
    pub client_secret: Option<&'a SecretString>,
    /// Authorization code returned to the redirect URI.
    pub code: &'a str,
    /// Redirect URI used in the authorization request (must match).
    pub redirect_uri: &'a str,
    /// PKCE code verifier matching the challenge sent earlier.
    pub code_verifier: &'a SecretString,
}

/// Exchange an authorization code for tokens (RFC 6749 §4.1.3 + PKCE).
///
/// # Errors
///
/// Returns [`TokenError::Http`] on transport/non-success status,
/// [`TokenError::InvalidResponse`] on parse failures, and
/// [`TokenError::UnsupportedTokenType`] for non-Bearer tokens.
pub async fn exchange_code(
    client: &HttpClient,
    req: &AuthCodeExchange<'_>,
) -> Result<TokenSet, TokenError> {
    let secret_expose = req.client_secret.map(|s| s.expose().to_owned());
    let mut fields: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", req.code),
        ("redirect_uri", req.redirect_uri),
        ("client_id", req.client_id),
        ("code_verifier", req.code_verifier.expose()),
    ];
    if let Some(ref secret) = secret_expose {
        fields.push(("client_secret", secret));
    }

    post_token(
        client,
        req.token_endpoint,
        &fields,
        "OAuth code exchange",
        false,
    )
    .await
}

/// Perform a refresh-token grant (RFC 6749 §6), rotating the refresh token
/// when the server returns a new one.
///
/// # Errors
///
/// Returns [`TokenError::RefreshRejected`] when the server rejects the refresh
/// token (HTTP 400/401 or `invalid_grant`), signalling that the caller should
/// re-authorize. Other failures map to [`TokenError::Http`] /
/// [`TokenError::InvalidResponse`].
pub async fn refresh_token(
    client: &HttpClient,
    token_endpoint: &str,
    client_id: &str,
    client_secret: Option<&SecretString>,
    refresh_token: &SecretString,
    scopes: &[String],
) -> Result<TokenSet, TokenError> {
    let secret_expose = client_secret.map(|s| s.expose().to_owned());
    let scope_joined = if scopes.is_empty() {
        None
    } else {
        Some(scopes.join(" "))
    };

    let mut fields: Vec<(&str, &str)> = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.expose()),
        ("client_id", client_id),
    ];
    if let Some(ref secret) = secret_expose {
        fields.push(("client_secret", secret));
    }
    if let Some(ref scope) = scope_joined {
        fields.push(("scope", scope));
    }

    post_token(client, token_endpoint, &fields, "OAuth token refresh", true).await
}

/// POST a token-endpoint form and parse the response.
///
/// When `reject_as_refresh` is set, HTTP 400/401 or an `invalid_grant` body is
/// surfaced as [`TokenError::RefreshRejected`] rather than a generic HTTP
/// error.
async fn post_token(
    client: &HttpClient,
    token_endpoint: &str,
    fields: &[(&str, &str)],
    ctx: &str,
    reject_as_refresh: bool,
) -> Result<TokenSet, TokenError> {
    let resp = client
        .post(token_endpoint)
        .form(fields)
        .map_err(|e| TokenError::Http(format_http_error(&e, ctx)))?
        .send()
        .await
        .map_err(|e| TokenError::Http(format_http_error(&e, ctx)))?;

    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| TokenError::Http(format_http_error(&e, ctx)))?;

    if !status.is_success() {
        let body = String::from_utf8_lossy(&bytes);
        let is_invalid_grant = body.contains("invalid_grant");
        if reject_as_refresh
            && (status.as_u16() == 400 || status.as_u16() == 401 || is_invalid_grant)
        {
            return Err(TokenError::RefreshRejected(format!(
                "{ctx}: HTTP {}",
                status.as_u16()
            )));
        }
        return Err(TokenError::Http(format!("{ctx}: HTTP {}", status.as_u16())));
    }

    let parsed: AuthCodeTokenResponse = serde_json::from_slice(&bytes)
        .map_err(|e| TokenError::InvalidResponse(format!("{ctx}: {e}")))?;

    if let Some(ref tt) = parsed.token_type
        && !tt.eq_ignore_ascii_case("bearer")
    {
        return Err(TokenError::UnsupportedTokenType(tt.clone()));
    }

    Ok(TokenSet {
        access_token: SecretString::new(parsed.access_token),
        refresh_token: parsed.refresh_token.map(SecretString::new),
        expires_in: parsed
            .expires_in
            .map_or(DEFAULT_TOKEN_TTL, Duration::from_secs),
        scope: parsed.scope,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issuer() -> Url {
        Url::parse("https://issuer.example.com").unwrap()
    }

    #[test]
    fn endpoint_matching_issuer_origin_is_accepted() {
        let r =
            validate_discovered_endpoint("https://issuer.example.com/token", &issuer(), "token");
        assert!(r.is_ok());
    }

    #[test]
    fn endpoint_on_different_origin_is_rejected() {
        let r = validate_discovered_endpoint("https://evil.example.com/token", &issuer(), "token");
        let err = r.unwrap_err();
        assert!(
            matches!(err, TokenError::InvalidResponse(ref m) if m.contains("origin")),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn insecure_http_endpoint_is_rejected() {
        // Same host as issuer but plain http (non-loopback) must be rejected.
        let r = validate_discovered_endpoint("http://issuer.example.com/token", &issuer(), "token");
        let err = r.unwrap_err();
        assert!(
            matches!(err, TokenError::InvalidResponse(ref m) if m.contains("https")),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn http_loopback_endpoint_is_allowed_for_loopback_issuer() {
        let issuer = Url::parse("http://localhost:8080").unwrap();
        let r = validate_discovered_endpoint("http://localhost:8080/token", &issuer, "token");
        assert!(r.is_ok());
    }

    #[test]
    fn malformed_endpoint_is_rejected() {
        let r = validate_discovered_endpoint("not a url", &issuer(), "token");
        assert!(matches!(r, Err(TokenError::InvalidResponse(_))));
    }

    #[test]
    fn resource_metadata_param_quoted_is_parsed() {
        let header = r#"Bearer realm="x", resource_metadata="https://rs.example.com/.well-known/oauth-protected-resource", error="invalid_token""#;
        assert_eq!(
            parse_resource_metadata_param(header).as_deref(),
            Some("https://rs.example.com/.well-known/oauth-protected-resource")
        );
    }

    #[test]
    fn resource_metadata_param_bare_token_is_parsed() {
        let header = "Bearer resource_metadata=https://rs.example.com/meta, error=invalid_token";
        assert_eq!(
            parse_resource_metadata_param(header).as_deref(),
            Some("https://rs.example.com/meta")
        );
    }

    #[test]
    fn resource_metadata_param_is_case_insensitive_key() {
        let header = r#"Bearer Resource_Metadata="https://rs.example.com/meta""#;
        assert_eq!(
            parse_resource_metadata_param(header).as_deref(),
            Some("https://rs.example.com/meta")
        );
    }

    #[test]
    fn resource_metadata_param_absent_returns_none() {
        assert!(
            parse_resource_metadata_param(r#"Bearer realm="x", error="invalid_token""#).is_none()
        );
    }

    #[test]
    fn resource_metadata_param_rejects_key_without_word_boundary() {
        // `xresource_metadata` must not be treated as the parameter.
        assert!(
            parse_resource_metadata_param(r#"Bearer xresource_metadata="https://e.example""#)
                .is_none()
        );
    }

    fn query_map(url: &Url) -> std::collections::HashMap<String, String> {
        url.query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    #[test]
    fn build_authorize_url_emits_expected_query_params() {
        let scopes = vec!["read".to_owned(), "write".to_owned()];
        let url = build_authorize_url(
            "https://as.example.com/authorize",
            "client-123",
            "https://app.example.com/callback",
            &scopes,
            "state-abc",
            "challenge-xyz",
        )
        .unwrap();

        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("as.example.com"));
        assert_eq!(url.path(), "/authorize");

        let q = query_map(&url);
        assert_eq!(q.get("response_type").map(String::as_str), Some("code"));
        assert_eq!(q.get("client_id").map(String::as_str), Some("client-123"));
        assert_eq!(
            q.get("redirect_uri").map(String::as_str),
            Some("https://app.example.com/callback")
        );
        assert_eq!(q.get("state").map(String::as_str), Some("state-abc"));
        assert_eq!(
            q.get("code_challenge").map(String::as_str),
            Some("challenge-xyz")
        );
        assert_eq!(
            q.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        // Scopes are space-joined into a single `scope` parameter.
        assert_eq!(q.get("scope").map(String::as_str), Some("read write"));
    }

    #[test]
    fn build_authorize_url_omits_scope_when_empty() {
        let url = build_authorize_url(
            "https://as.example.com/authorize",
            "client-123",
            "https://app.example.com/callback",
            &[],
            "state-abc",
            "challenge-xyz",
        )
        .unwrap();
        assert!(!query_map(&url).contains_key("scope"));
    }

    #[test]
    fn build_authorize_url_rejects_malformed_endpoint() {
        let err = build_authorize_url(
            "not a url",
            "client-123",
            "https://app.example.com/callback",
            &[],
            "state-abc",
            "challenge-xyz",
        )
        .unwrap_err();
        assert!(matches!(err, TokenError::ConfigError(_)));
    }

    #[test]
    fn build_authorize_url_rejects_insecure_non_loopback_endpoint() {
        let err = build_authorize_url(
            "http://as.example.com/authorize",
            "client-123",
            "https://app.example.com/callback",
            &[],
            "state-abc",
            "challenge-xyz",
        )
        .unwrap_err();
        assert!(matches!(err, TokenError::ConfigError(_)));
    }

    #[test]
    fn pkce_generate_satisfies_s256_contract() {
        let pkce = Pkce::generate();
        assert_eq!(pkce.method(), "S256");

        let verifier = pkce.verifier.expose();
        // Verifier must be URL-safe (RFC 7636 unreserved set) and high-entropy:
        // 32 random bytes base64url-encoded => 43 chars, no padding.
        assert_eq!(verifier.len(), 43, "verifier: {verifier}");
        assert!(
            verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "verifier not URL-safe: {verifier}"
        );

        // challenge == BASE64URL_NO_PAD(SHA256(verifier)).
        let expected = URL_SAFE_NO_PAD.encode(sha256(&SHA256, verifier.as_bytes()).as_ref());
        assert_eq!(pkce.challenge, expected);
    }

    #[test]
    fn pkce_generate_produces_distinct_verifiers() {
        // Sanity check that each call draws fresh randomness.
        let a = Pkce::generate();
        let b = Pkce::generate();
        assert_ne!(a.verifier.expose(), b.verifier.expose());
        assert_ne!(a.challenge, b.challenge);
    }
}
