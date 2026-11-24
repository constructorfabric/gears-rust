//! AWS Signature Version 4 auth plugin (#5842). It signs in the signing
//! phase (ADR-0023), on the request as sent; `authenticate` only checks the
//! config.
//!
//! The canonical request, the path and query encoding and the header
//! selection are built here. Every hash and HMAC goes through
//! `toolkit_security::crypto::sigv4`.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use credstore_sdk::{CredStoreClientV1, SecretRef};
use time::{OffsetDateTime, UtcOffset};
use toolkit_security::SecurityContext;
use toolkit_security::crypto::sigv4::{CredentialScope, SigningKey, sha256_hex};
use zeroize::Zeroizing;

use crate::domain::plugin::aws_sigv4::{AwsSigV4AuthConfig, UNSIGNED_PAYLOAD_FIELD, UriEncoding};
use crate::domain::plugin::credentials::{
    ACCESS_KEY_ID_REF_KEY, SECRET_ACCESS_KEY_REF_KEY, SESSION_TOKEN_REF_KEY,
};
use crate::domain::plugin::{
    AuthContext, AuthPlugin, PluginError, QueryChange, SigningBody, SigningContext, SigningOutput,
};
use crate::infra::plugin::read_credential;

const ALGORITHM: &str = "AWS4-HMAC-SHA256";
const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";
const AUTHORIZATION: &str = "authorization";
const X_AMZ_DATE: &str = "x-amz-date";
const X_AMZ_SECURITY_TOKEN: &str = "x-amz-security-token";
const X_AMZ_CONTENT_SHA256: &str = "x-amz-content-sha256";
/// Longest access key id accepted from the credential store.
const MAX_ACCESS_KEY_ID_LEN: usize = 128;
/// Largest buffered body hashed as plain async code (64 KiB, tens of
/// microseconds); a larger one is hashed under `block_in_place`. The REST
/// proxy buffers up to `max_body_size_bytes` (100 MiB by default).
const INLINE_HASH_MAX_LEN: usize = 64 * 1024;

#[cfg(test)]
thread_local! {
    /// Bodies this thread hashed under `block_in_place` in [`payload_hash`].
    static BLOCKING_HASHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// ---------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------

/// Where the plugin takes the signing time from.
pub(crate) trait SigningClock: Send + Sync {
    /// The current time, in UTC.
    fn now(&self) -> OffsetDateTime;
}

/// The system clock.
pub(crate) struct SystemClock;

impl SigningClock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

/// The credentials a request is signed with. `Debug` shows neither key.
pub(crate) struct AwsCredentials {
    /// The access key id, sent in the `Credential` scope.
    pub(crate) access_key_id: String,
    /// The secret access key the signing key is derived from.
    pub(crate) secret_access_key: Zeroizing<Vec<u8>>,
    /// The session token of temporary credentials, sent as
    /// `X-Amz-Security-Token`.
    pub(crate) session_token: Option<Zeroizing<String>>,
}

impl std::fmt::Debug for AwsCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsCredentials")
            .field("session_token", &self.session_token.is_some())
            .finish_non_exhaustive()
    }
}

/// Where the plugin gets its credentials. CredStore references today;
/// temporary credentials from the runtime's identity (#5844) plug in here.
#[async_trait]
pub(crate) trait AwsCredentialSource: Send + Sync {
    /// The credentials for `config`, read as the caller. Errors name the
    /// config key, never the reference or a value.
    async fn credentials(
        &self,
        config: &AwsSigV4AuthConfig,
        security_context: &SecurityContext,
    ) -> Result<AwsCredentials, PluginError>;
}

/// Credentials from the CredStore references in the auth config.
pub(crate) struct CredStoreCredentialSource {
    credstore: Arc<dyn CredStoreClientV1>,
}

impl CredStoreCredentialSource {
    /// A source that reads through `credstore`.
    pub(crate) fn new(credstore: Arc<dyn CredStoreClientV1>) -> Self {
        Self { credstore }
    }

    /// The secret behind `secret_ref`, read from the auth config `key`.
    async fn read(
        &self,
        security_context: &SecurityContext,
        key: &'static str,
        secret_ref: &SecretRef,
    ) -> Result<Zeroizing<Vec<u8>>, PluginError> {
        let value =
            read_credential(self.credstore.as_ref(), security_context, key, secret_ref).await?;
        Ok(Zeroizing::new(value.as_bytes().to_vec()))
    }
}

#[async_trait]
impl AwsCredentialSource for CredStoreCredentialSource {
    /// The references are read concurrently. If more than one read fails,
    /// the error is the one that fails first (the access key id's when they
    /// fail together).
    async fn credentials(
        &self,
        config: &AwsSigV4AuthConfig,
        security_context: &SecurityContext,
    ) -> Result<AwsCredentials, PluginError> {
        let (access_key_id, secret_access_key, session_token) = tokio::try_join!(
            biased;
            self.read(
                security_context,
                ACCESS_KEY_ID_REF_KEY,
                &config.access_key_id_ref,
            ),
            self.read(
                security_context,
                SECRET_ACCESS_KEY_REF_KEY,
                &config.secret_access_key_ref,
            ),
            async {
                match &config.session_token_ref {
                    Some(token_ref) => self
                        .read(security_context, SESSION_TOKEN_REF_KEY, token_ref)
                        .await
                        .map(Some),
                    None => Ok(None),
                }
            },
        )?;
        // It goes into the `Authorization` header's `Credential=` part,
        // which `/`, `,` or a space would break.
        if access_key_id.is_empty()
            || access_key_id.len() > MAX_ACCESS_KEY_ID_LEN
            || !access_key_id.iter().all(u8::is_ascii_alphanumeric)
        {
            return Err(PluginError::Internal(format!(
                "secret referenced by auth.config.{ACCESS_KEY_ID_REF_KEY} is not an access key id"
            )));
        }
        if secret_access_key.is_empty() {
            return Err(PluginError::Internal(format!(
                "secret referenced by auth.config.{SECRET_ACCESS_KEY_REF_KEY} is empty"
            )));
        }
        let session_token = match session_token {
            Some(token) => {
                // Sent as a header value: visible ASCII only.
                if token.is_empty() || !token.iter().all(u8::is_ascii_graphic) {
                    return Err(PluginError::Internal(format!(
                        "secret referenced by auth.config.{SESSION_TOKEN_REF_KEY} is not a session token"
                    )));
                }
                Some(Zeroizing::new(
                    token.iter().copied().map(char::from).collect::<String>(),
                ))
            }
            None => None,
        };
        Ok(AwsCredentials {
            // Checked above: ASCII alphanumeric.
            access_key_id: access_key_id.iter().copied().map(char::from).collect(),
            secret_access_key,
            session_token,
        })
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Auth plugin that signs the outbound request with AWS Signature Version 4
/// (header signing; no presigned URLs, SigV4a or S3 `aws-chunked`).
pub struct AwsSigV4AuthPlugin {
    credentials: Arc<dyn AwsCredentialSource>,
    clock: Arc<dyn SigningClock>,
}

impl AwsSigV4AuthPlugin {
    /// The plugin reading its credentials from `credstore`, signing with the
    /// system clock.
    #[must_use]
    pub fn new(credstore: Arc<dyn CredStoreClientV1>) -> Self {
        Self {
            credentials: Arc::new(CredStoreCredentialSource::new(credstore)),
            clock: Arc::new(SystemClock),
        }
    }

    /// The plugin with its credentials and time from the given sources.
    #[cfg(test)]
    pub(crate) fn with_sources(
        credentials: Arc<dyn AwsCredentialSource>,
        clock: Arc<dyn SigningClock>,
    ) -> Self {
        Self { credentials, clock }
    }
}

fn parse_config(
    config: &std::collections::HashMap<String, String>,
) -> Result<AwsSigV4AuthConfig, PluginError> {
    AwsSigV4AuthConfig::parse(Some(config)).map_err(|e| PluginError::InvalidConfig(e.detail))
}

#[async_trait]
impl AuthPlugin for AwsSigV4AuthPlugin {
    /// Checks the config; the request is signed in [`Self::sign`].
    async fn authenticate(&self, ctx: &mut AuthContext) -> Result<(), PluginError> {
        parse_config(&ctx.config).map(drop)
    }

    fn signs(&self) -> bool {
        true
    }

    async fn sign(&self, ctx: &SigningContext) -> Result<SigningOutput, PluginError> {
        let config = parse_config(&ctx.config)?;
        // `unsigned_payload` affects only a streamed body; a buffered one is
        // always hashed.
        let body = match &ctx.body {
            SigningBody::Buffered(body) => Some(body),
            SigningBody::Streamed if config.unsigned_payload => None,
            SigningBody::Streamed => {
                return Err(PluginError::InvalidRequest {
                    field: "body",
                    detail: format!(
                        "a streamed request body cannot be hashed for AWS SigV4; set \
                         {UNSIGNED_PAYLOAD_FIELD} to 'true' to send it as {UNSIGNED_PAYLOAD}"
                    ),
                });
            }
        };
        // S3 does not normalize paths: `a//b`, `a/./b`, `dir` and `dir/` are keys
        // of their own, and the path the gateway normalized would sign and
        // address another object.
        if config.uri_encoding == UriEncoding::Single && ctx.path_rewritten {
            return Err(PluginError::InvalidRequest {
                field: "path",
                detail: "the gateway removed an empty, `.` or `..` segment from the request \
                         path, or removed or added a trailing `/`; with single URI encoding \
                         (Amazon S3) the signed path would name another object"
                    .to_owned(),
            });
        }
        // A header can only be replaced, not removed: a client token with
        // long-term keys would be sent and signed as if it were ours.
        if config.session_token_ref.is_none() && has_header(ctx, X_AMZ_SECURITY_TOKEN) {
            return Err(PluginError::InvalidRequest {
                field: X_AMZ_SECURITY_TOKEN,
                detail: format!(
                    "the request carries {X_AMZ_SECURITY_TOKEN} but auth.config.\
                     {SESSION_TOKEN_REF_KEY} is not set; the AWS SigV4 plugin cannot remove \
                     the header, so drop it with headers.request.remove"
                ),
            });
        }
        // The read goes first, so it is in flight while the body is hashed.
        let (credentials, payload_hash) = tokio::try_join!(
            biased;
            self.credentials.credentials(&config, &ctx.security_context),
            async {
                match body {
                    Some(body) => payload_hash(body),
                    None => Ok(UNSIGNED_PAYLOAD.to_owned()),
                }
            },
        )?;
        sign_with(ctx, &config, &credentials, &payload_hash, self.clock.now())
    }
}

/// The SHA-256 of a buffered body, as hex.
///
/// A body over [`INLINE_HASH_MAX_LEN`] takes up to a few hundred
/// milliseconds (100 MiB), so on a multi-threaded runtime it is hashed under
/// `block_in_place`, as `toolkit-onnx-runtime` runs inference: the worker's
/// other tasks move to another thread meanwhile. On a current-thread runtime
/// there is no other worker and `block_in_place` would panic, so it is hashed
/// directly. The request deadline is checked once the hash returns.
fn payload_hash(body: &Bytes) -> Result<String, PluginError> {
    let multi_thread = tokio::runtime::Handle::try_current()
        .is_ok_and(|handle| handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread);
    if body.len() <= INLINE_HASH_MAX_LEN || !multi_thread {
        return sha256_hex(body).map_err(crypto_error);
    }
    #[cfg(test)]
    BLOCKING_HASHES.with(|n| n.set(n.get() + 1));
    tokio::task::block_in_place(|| sha256_hex(body)).map_err(crypto_error)
}

fn crypto_error(e: toolkit_security::crypto::CryptoError) -> PluginError {
    PluginError::Internal(format!("AWS SigV4 signing failed: {e}"))
}

fn has_header(ctx: &SigningContext, name: &str) -> bool {
    ctx.headers.iter().any(|(n, _)| n == name)
}

/// The signing output for `ctx` with the given credentials, payload hash
/// and time.
fn sign_with(
    ctx: &SigningContext,
    config: &AwsSigV4AuthConfig,
    credentials: &AwsCredentials,
    payload_hash: &str,
    now: OffsetDateTime,
) -> Result<SigningOutput, PluginError> {
    let (date, amz_date) = timestamps(now);

    // The headers the plugin sets replace the client's lines of that name.
    let mut set_headers: Vec<(String, Vec<u8>)> =
        vec![(X_AMZ_DATE.to_owned(), amz_date.clone().into_bytes())];
    if config.content_sha256_header || has_header(ctx, X_AMZ_CONTENT_SHA256) {
        set_headers.push((
            X_AMZ_CONTENT_SHA256.to_owned(),
            payload_hash.as_bytes().to_vec(),
        ));
    }
    if let Some(token) = &credentials.session_token {
        set_headers.push((X_AMZ_SECURITY_TOKEN.to_owned(), token.as_bytes().to_vec()));
    }

    let (canonical_query, query) = canonical_query(ctx.query.as_deref());
    let headers = signed_header_lines(&ctx.headers, &set_headers)?;
    let canonical = canonical_request(
        &ctx.method,
        &canonical_path(&ctx.path, config.uri_encoding),
        &canonical_query,
        &headers,
        payload_hash,
    );

    let scope =
        CredentialScope::new(&date, &config.region, &config.service).map_err(crypto_error)?;
    let string_to_sign = format!(
        "{ALGORITHM}\n{amz_date}\n{scope}\n{}",
        sha256_hex(&canonical.request).map_err(crypto_error)?
    );
    let key = SigningKey::derive(&credentials.secret_access_key, &scope).map_err(crypto_error)?;
    let signature = key
        .signature(string_to_sign.as_bytes())
        .map_err(crypto_error)?;
    let authorization = format!(
        "{ALGORITHM} Credential={}/{scope}, SignedHeaders={}, Signature={signature}",
        credentials.access_key_id, canonical.signed_headers
    );
    set_headers.insert(0, (AUTHORIZATION.to_owned(), authorization.into_bytes()));

    let query = match (query, ctx.query.as_deref()) {
        (None, None) => QueryChange::Keep,
        (None, Some(_)) => QueryChange::Remove,
        (Some(q), Some(sent)) if q == sent => QueryChange::Keep,
        (Some(q), _) => QueryChange::Replace(q),
    };
    Ok(SigningOutput { set_headers, query })
}

/// The scope date `YYYYMMDD` and the `X-Amz-Date` value `YYYYMMDDTHHMMSSZ`.
fn timestamps(now: OffsetDateTime) -> (String, String) {
    let now = now.to_offset(UtcOffset::UTC);
    let date = format!(
        "{:04}{:02}{:02}",
        now.year(),
        u8::from(now.month()),
        now.day()
    );
    let amz_date = format!(
        "{date}T{:02}{:02}{:02}Z",
        now.hour(),
        now.minute(),
        now.second()
    );
    (date, amz_date)
}

// ---------------------------------------------------------------------------
// Canonical request
// ---------------------------------------------------------------------------

/// Header lines by lowercase name, every line of a name in the order sent.
pub(crate) type HeaderLines = BTreeMap<String, Vec<Vec<u8>>>;

/// A canonical request and its `SignedHeaders` list.
pub(crate) struct Canonical {
    /// The canonical request bytes the string to sign hashes.
    pub(crate) request: Vec<u8>,
    /// The `SignedHeaders` value: the signed names, sorted, `;`-joined.
    pub(crate) signed_headers: String,
}

/// Whether a header is signed: `host`, every `x-amz-*`, `content-type` and
/// `content-md5`. The engine still rewrites framing and connection fields
/// and joins `Cookie` lines (ADR-0023), and proxies may change `User-Agent`,
/// so those are never signed.
fn is_signed_header(name: &str) -> bool {
    name == "host" || name == "content-type" || name == "content-md5" || name.starts_with("x-amz-")
}

/// The signed headers, by name, with every line in the order sent: the
/// request's lines, except the names in `set`, whose values replace them.
fn signed_header_lines(
    headers: &[(String, Vec<u8>)],
    set: &[(String, Vec<u8>)],
) -> Result<HeaderLines, PluginError> {
    let mut lines: HeaderLines = BTreeMap::new();
    for (name, value) in headers {
        if is_signed_header(name) && !set.iter().any(|(n, _)| n == name) {
            lines.entry(name.clone()).or_default().push(value.clone());
        }
    }
    for (name, value) in set {
        if is_signed_header(name) {
            lines.insert(name.clone(), vec![value.clone()]);
        }
    }
    if !lines.contains_key("host") {
        return Err(PluginError::Internal(
            "AWS SigV4: the request has no Host header to sign".into(),
        ));
    }
    Ok(lines)
}

/// The canonical request (method, path, query, headers, signed headers,
/// payload hash), as bytes: header values need not be UTF-8.
pub(crate) fn canonical_request(
    method: &str,
    canonical_path: &str,
    canonical_query: &str,
    headers: &HeaderLines,
    payload_hash: &str,
) -> Canonical {
    let mut request = Vec::new();
    for part in [method, canonical_path, canonical_query] {
        request.extend_from_slice(part.as_bytes());
        request.push(b'\n');
    }
    for (name, values) in headers {
        request.extend_from_slice(name.as_bytes());
        request.push(b':');
        for (i, value) in values.iter().enumerate() {
            if i > 0 {
                request.push(b',');
            }
            push_trimmed(&mut request, value);
        }
        request.push(b'\n');
    }
    request.push(b'\n');
    let signed_headers = headers
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(";");
    request.extend_from_slice(signed_headers.as_bytes());
    request.push(b'\n');
    request.extend_from_slice(payload_hash.as_bytes());
    Canonical {
        request,
        signed_headers,
    }
}

/// `value` without leading and trailing whitespace, each run of spaces or
/// tabs as one space.
fn push_trimmed(out: &mut Vec<u8>, value: &[u8]) {
    let is_space = |b: &u8| *b == b' ' || *b == b'\t';
    let start = value
        .iter()
        .position(|b| !is_space(b))
        .unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|b| !is_space(b))
        .map_or(start, |i| i + 1);
    let mut in_space = false;
    for b in &value[start..end] {
        if is_space(b) {
            if !in_space {
                out.push(b' ');
            }
            in_space = true;
        } else {
            out.push(*b);
            in_space = false;
        }
    }
}

/// Append `bytes` URI-encoded as SigV4 requires: unreserved characters
/// (`A-Z a-z 0-9 - . _ ~`) as they are, every other byte as `%XX` in
/// uppercase hex; `/` kept when `keep_slash`.
fn uri_encode(out: &mut String, bytes: &[u8], keep_slash: bool) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in bytes {
        if b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'.' | b'_' | b'~')
            || (keep_slash && b == b'/')
        {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 0x0f)]));
        }
    }
}

/// The canonical URI of `path`, the path as sent (percent-encoded once).
/// `Single` (S3) decodes it and encodes it again in SigV4 encoding, as S3
/// does before it checks: a client may send `:`, `(` or `!` unencoded, and
/// S3 signs them as `%3A`, `%28`, `%21`. `/`, also from `%2F`, is kept.
/// `Double` removes `.` and `..` segments and repeated slashes, as AWS
/// services do before they check, and encodes the result again.
pub(crate) fn canonical_path(path: &str, encoding: UriEncoding) -> String {
    match encoding {
        UriEncoding::Single if path.is_empty() => "/".to_owned(),
        UriEncoding::Single => {
            let decoded: Vec<u8> = percent_encoding::percent_decode_str(path).collect();
            let mut out = String::with_capacity(path.len());
            uri_encode(&mut out, &decoded, true);
            out
        }
        UriEncoding::Double => {
            let normalized = remove_dot_segments(path);
            let mut out = String::with_capacity(normalized.len());
            uri_encode(&mut out, normalized.as_bytes(), true);
            out
        }
    }
}

/// `path` without empty, `.` and `..` segments, keeping a leading and a
/// trailing `/`. An empty result is `/`.
fn remove_dot_segments(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    let mut out = String::from("/");
    out.push_str(&segments.join("/"));
    if path.ends_with('/') && !segments.is_empty() {
        out.push('/');
    }
    out
}

/// The canonical query string of the query as sent, and the query to send:
/// the same pairs in their order, each name and value in SigV4 encoding
/// (`None` when there are no pairs). The gateway builds the outbound query
/// form-urlencoded (`+` for a space), so that is how it is decoded; AWS
/// reads `+` as a literal plus, hence the rewrite.
pub(crate) fn canonical_query(query: Option<&str>) -> (String, Option<String>) {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for pair in query.unwrap_or_default().split('&') {
        if pair.is_empty() {
            continue;
        }
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        pairs.push((form_to_uri(name), form_to_uri(value)));
    }
    if pairs.is_empty() {
        return (String::new(), None);
    }
    let join = |pairs: &[(String, String)]| {
        pairs
            .iter()
            .map(|(n, v)| format!("{n}={v}"))
            .collect::<Vec<_>>()
            .join("&")
    };
    let sent = join(&pairs);
    pairs.sort();
    (join(&pairs), Some(sent))
}

/// A form-urlencoded component in SigV4 URI encoding.
fn form_to_uri(component: &str) -> String {
    let spaced = component.replace('+', " ");
    let bytes: Vec<u8> = percent_encoding::percent_decode(spaced.as_bytes()).collect();
    let mut out = String::with_capacity(bytes.len());
    uri_encode(&mut out, &bytes, false);
    out
}

#[cfg(test)]
#[path = "aws_sigv4_auth_tests.rs"]
mod tests;
