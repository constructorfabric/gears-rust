//! Media Resolver (DESIGN §3.2): image and file input parts.
//!
//! # Contract
//!
//! Pure helpers (shared with the Capability Checker):
//! - `parse_data_uri`: `data:<type>/<subtype>[;param=value]*;base64,<payload>`;
//!   the scheme is case-insensitive, the MIME type is lowercased and its
//!   parameters are dropped; both halves of the MIME type must be tokens
//!   (`is_mime_token`, so no `*`, quotes, commas or backslashes). Anything else (no `;base64`, no comma, empty
//!   MIME type, empty payload) is `None`.
//! - `decoded_base64_len`: strict standard-alphabet base64 with padding, no
//!   whitespace, no non-canonical trailing bits; gives the decoded size.
//! - `mime_allowed`: exact (case-insensitive) match or `type/*` wildcard.
//! - `mime_for_filename`: MIME type of a plain-base64 `file_data` from the
//!   filename extension (`pdf`, `txt`, `md`, `csv`, `json`; case-insensitive).
//! - `has_https_scheme_without_user_info`: the URL rule of the Media
//!   Resolver (case-insensitive `https` scheme, a host, no user info).
//!
//! [`MediaResolver::resolve`] for a plugin in `UrlForward` mode leaves every
//! part as it is. In `PrefetchRequired` mode every media part reaches the
//! plugin as inline data: `image_url` is a `data:<mime>;base64,...` URI,
//! `file_data` is a data URI too (`file_url` is cleared), plain base64
//! `file_data` becomes a data URI with the MIME type from the filename, and
//! an `https` URL is fetched through the [`MediaFetcher`] (timeout
//! `media.fetch_timeout`, size `media.max_bytes`, content type from the
//! allow-list of the part kind). Every failure is `media_not_found` with the
//! part path (`input[i].content[j]`) as `param` and a fixed message. Media
//! content and URLs are never logged.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use futures_util::StreamExt as _;
use llm_gateway_sdk::models::items::InputContent;
use llm_gateway_sdk::{
    CreateResponseBody, InputContentPart, InputItem, LlmGatewayError, MediaInputMode, ResponseInput,
};
use toolkit_macros::domain_model;

use super::ports::{MediaFetchLimits, MediaFetcher};
use crate::config::{MediaConfig, is_mime_token};

/// A parsed base64 data URI.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataUri<'a> {
    /// Lowercased MIME type without parameters.
    pub mime: String,
    /// The base64 payload (not validated).
    pub payload: &'a str,
}

/// Parse a base64 data URI; see the module docs.
#[must_use]
pub fn parse_data_uri(value: &str) -> Option<DataUri<'_>> {
    if !starts_with_data_scheme(value) {
        return None;
    }
    let rest = &value[DATA_SCHEME.len()..];
    let (header, payload) = rest.split_once(',')?;
    if payload.is_empty() {
        return None;
    }
    let mut segments = header.split(';');
    let mime = segments.next()?;
    let last = segments.next_back()?;
    if !last.eq_ignore_ascii_case("base64") {
        return None;
    }
    Some(DataUri {
        mime: canonical_mime(mime)?,
        payload,
    })
}

/// The lowercased `type/subtype` of `mime` when both halves are RFC 7230
/// tokens of the allow-list charset (see [`is_mime_token`]); `None` for
/// anything else, wildcards (`text/*`) and parameters (`;charset=x`) included.
fn canonical_mime(mime: &str) -> Option<String> {
    let (kind, subtype) = mime.split_once('/')?;
    (is_mime_token(kind) && is_mime_token(subtype)).then(|| mime.to_ascii_lowercase())
}

const DATA_SCHEME: &str = "data:";

/// Whether `value` starts with the (case-insensitive) `data:` scheme.
#[must_use]
pub fn starts_with_data_scheme(value: &str) -> bool {
    value
        .get(..DATA_SCHEME.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(DATA_SCHEME))
}

/// Decoded size of `payload` when it is strict canonical base64.
#[must_use]
pub fn decoded_base64_len(payload: &str) -> Option<usize> {
    let bytes = payload.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return None;
    }
    let padding = bytes.iter().rev().take_while(|b| **b == b'=').count();
    if padding > 2 {
        return None;
    }
    let data = &bytes[..bytes.len() - padding];
    let value = |b: u8| match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    if data.iter().any(|b| value(*b).is_none()) {
        return None;
    }
    // The bits of the last character that encode no data must be zero.
    let unused_bits_clear = match padding {
        1 => data
            .last()
            .and_then(|b| value(*b))
            .is_some_and(|v| v.trailing_zeros() >= 2),
        2 => data
            .last()
            .and_then(|b| value(*b))
            .is_some_and(|v| v.trailing_zeros() >= 4),
        _ => true,
    };
    if !unused_bits_clear {
        return None;
    }
    Some(bytes.len().div_ceil(4) * 3 - padding)
}

/// Whether `mime` matches an entry of `allowed`.
#[must_use]
pub fn mime_allowed(allowed: &[String], mime: &str) -> bool {
    allowed.iter().any(|entry| match entry.strip_suffix("/*") {
        Some(kind) => mime
            .split_once('/')
            .is_some_and(|(k, sub)| !sub.is_empty() && k.eq_ignore_ascii_case(kind)),
        None => entry.eq_ignore_ascii_case(mime),
    })
}

/// MIME type implied by the extension of `filename`.
#[must_use]
pub fn mime_for_filename(filename: &str) -> Option<&'static str> {
    let (_, extension) = filename.rsplit_once('.')?;
    match extension.to_ascii_lowercase().as_str() {
        "pdf" => Some("application/pdf"),
        "txt" => Some("text/plain"),
        "md" => Some("text/markdown"),
        "csv" => Some("text/csv"),
        "json" => Some("application/json"),
        _ => None,
    }
}

/// The Media Resolver URL rule: a case-insensitive `https` scheme, a host,
/// no user info, no whitespace, control characters or backslashes, an
/// optional port in `1..=65535`. The host must be printable ASCII (IDNA
/// hosts are rejected for now; a punycode form passes). A bracketed host must
/// be closed and only followed by an optional `:port`.
#[must_use]
pub fn has_https_scheme_without_user_info(url: &str) -> bool {
    const SCHEME: &str = "https://";
    if url
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return false;
    }
    let Some(head) = url.get(..SCHEME.len()) else {
        return false;
    };
    if !head.eq_ignore_ascii_case(SCHEME) {
        return false;
    }
    let rest = &url[SCHEME.len()..];
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return false;
    }
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let Some((inside, after)) = bracketed.split_once(']') else {
            return false;
        };
        let port = match after {
            "" => None,
            _ => match after.strip_prefix(':') {
                Some(port) => Some(port),
                None => return false,
            },
        };
        (inside, port)
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_graphic() && !"[]".contains(c))
        && port.is_none_or(is_valid_port)
}

/// `1..=65535` in plain ASCII digits.
fn is_valid_port(port: &str) -> bool {
    port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u16>().is_ok_and(|p| p > 0)
}

/// `data:<mime>;base64,<standard base64 of bytes>`.
#[must_use]
pub fn data_uri(mime: &str, bytes: &[u8]) -> String {
    const HEAD: &str = "data:";
    const BASE64: &str = ";base64,";
    let mut out =
        String::with_capacity(HEAD.len() + mime.len() + BASE64.len() + bytes.len().div_ceil(3) * 4);
    out.push_str(HEAD);
    out.push_str(mime);
    out.push_str(BASE64);
    STANDARD.encode_string(bytes, &mut out);
    out
}

/// Part kind: which allow-list applies.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Image,
    File,
}

/// Resolves the media parts of a request for the plugin's media input mode.
#[domain_model]
pub struct MediaResolver {
    fetcher: Option<Arc<dyn MediaFetcher>>,
    limits: MediaFetchLimits,
    config: MediaConfig,
}

impl MediaResolver {
    /// Resolver fetching through `fetcher` under `config`.
    #[must_use]
    pub fn new(fetcher: Arc<dyn MediaFetcher>, config: &MediaConfig) -> Self {
        Self {
            fetcher: Some(fetcher),
            limits: Self::limits_of(config),
            config: config.clone(),
        }
    }

    /// Resolver without any media egress route: every fetch fails.
    #[must_use]
    pub fn unrouted(config: &MediaConfig) -> Self {
        Self {
            fetcher: None,
            limits: Self::limits_of(config),
            config: config.clone(),
        }
    }

    fn limits_of(config: &MediaConfig) -> MediaFetchLimits {
        MediaFetchLimits {
            timeout: config.fetch_timeout,
            max_bytes: config.max_bytes,
        }
    }

    /// Whether a media egress route is configured.
    #[must_use]
    pub fn has_route(&self) -> bool {
        self.fetcher.as_ref().is_some_and(|f| f.has_route())
    }

    /// Whether `body` holds an `input_image` or `input_file` part.
    #[must_use]
    pub fn has_media(body: &CreateResponseBody) -> bool {
        indexed_content_parts(body).any(|(_, part)| {
            matches!(
                part,
                InputContentPart::InputImage(_) | InputContentPart::InputFile(_)
            )
        })
    }

    /// Resolve the media parts of `body` for a plugin with `mode`.
    ///
    /// # Errors
    ///
    /// `media_not_found` (param `input[i].content[j]`).
    pub async fn resolve(
        &self,
        mode: MediaInputMode,
        mut body: CreateResponseBody,
    ) -> Result<CreateResponseBody, LlmGatewayError> {
        if mode == MediaInputMode::UrlForward {
            return Ok(body);
        }
        let Some(ResponseInput::Items(items)) = body.input.as_mut() else {
            return Ok(body);
        };
        let mut pending = Vec::new();
        let mut paths = Vec::new();
        for (i, item) in items.iter_mut().enumerate() {
            let InputItem::Message(message) = item else {
                continue;
            };
            let InputContent::Parts(content) = &mut message.content else {
                continue;
            };
            for (j, part) in content.iter_mut().enumerate() {
                if matches!(
                    part,
                    InputContentPart::InputImage(_) | InputContentPart::InputFile(_)
                ) {
                    let path = format!("input[{i}].content[{j}]");
                    paths.push(path.clone());
                    pending.push(self.resolve_part(path, part));
                }
            }
        }

        // Bounded concurrency; results stay in document order, so the first
        // failure in document order is the one reported. The whole resolution
        // is bounded by `media.total_timeout`: on expiry the first part still
        // unresolved (in document order) is reported as `media_not_found`.
        let mut resolved = futures_util::stream::iter(pending).buffered(MAX_CONCURRENT_FETCHES);
        let mut done = 0usize;
        let run = async {
            while let Some(outcome) = resolved.next().await {
                outcome?;
                done += 1;
            }
            Ok::<(), LlmGatewayError>(())
        };
        let outcome = tokio::time::timeout(self.config.total_timeout, run).await;
        drop(resolved);
        match outcome {
            Ok(result) => result?,
            Err(_elapsed) => {
                let path = paths
                    .get(done)
                    .or(paths.last())
                    .cloned()
                    .unwrap_or_default();
                return Err(LlmGatewayError::media_not_found_param(path));
            }
        }
        Ok(body)
    }

    async fn resolve_part(
        &self,
        path: String,
        part: &mut InputContentPart,
    ) -> Result<(), LlmGatewayError> {
        let not_found = || LlmGatewayError::media_not_found_param(path.as_str());
        match part {
            InputContentPart::InputImage(image) => {
                if let Some(url) = image.image_url.take() {
                    image.image_url =
                        Some(self.inline(url, Kind::Image).await.ok_or_else(not_found)?);
                }
            }
            InputContentPart::InputFile(file) => {
                if let Some(data) = file.file_data.take() {
                    let uri = if starts_with_data_scheme(&data) {
                        normalized_data_uri(data)
                    } else {
                        file.filename
                            .as_deref()
                            .and_then(mime_for_filename)
                            .map(|mime| prepend_data_header(mime, data))
                    };
                    file.file_data = Some(uri.ok_or_else(not_found)?);
                    // `file_data` wins over a second source.
                    file.file_url = None;
                } else if let Some(url) = file.file_url.take() {
                    file.file_data =
                        Some(self.inline(url, Kind::File).await.ok_or_else(not_found)?);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The data URI form of `value`: a data URI is normalized, an `https`
    /// URL is fetched. `None` on any failure.
    async fn inline(&self, value: String, kind: Kind) -> Option<String> {
        if starts_with_data_scheme(&value) {
            return normalized_data_uri(value);
        }
        if !has_https_scheme_without_user_info(&value) || !self.has_route() {
            return None;
        }
        let fetcher = self.fetcher.as_ref()?;
        let response =
            tokio::time::timeout(self.limits.timeout, fetcher.fetch(&value, &self.limits))
                .await
                .ok()?
                .ok()?;
        let content_type = canonical_mime(
            response
                .content_type
                .split(';')
                .next()
                .unwrap_or_default()
                .trim(),
        )?;
        let allowed = match kind {
            Kind::Image => &self.config.allowed_image_types,
            Kind::File => &self.config.allowed_file_types,
        };
        if !mime_allowed(allowed, &content_type)
            || response.bytes.is_empty()
            || response.bytes.len() > self.limits.max_bytes
        {
            return None;
        }
        Some(data_uri(&content_type, &response.bytes))
    }
}

/// Parallel fetches of one request.
const MAX_CONCURRENT_FETCHES: usize = 4;

/// `data:<mime>;base64,` in front of plain base64 `data` (no second copy of
/// the payload).
fn prepend_data_header(mime: &str, mut data: String) -> String {
    data.insert_str(0, &format!("{DATA_SCHEME}{mime};base64,"));
    data
}

/// The canonical form `data:<lowercase mime>;base64,<payload>` of a data URI:
/// `value` itself when it is canonical already, otherwise only the header is
/// rewritten in place. `None` when `value` is not a valid base64 data URI.
fn normalized_data_uri(mut value: String) -> Option<String> {
    let parsed = parse_data_uri(&value)?;
    let mime = parsed.mime;
    // Header = everything before the comma that starts the payload.
    let header_len = value.len() - parsed.payload.len() - 1;
    let canonical = format!("{DATA_SCHEME}{mime};base64");
    if value[..header_len] != canonical {
        value.replace_range(..header_len, &canonical);
    }
    Some(value)
}

/// Content parts of the message items with their `(i, j)` index (the
/// `input[i].content[j]` path).
pub(super) fn indexed_content_parts(
    body: &CreateResponseBody,
) -> impl Iterator<Item = ((usize, usize), &InputContentPart)> {
    let items: &[InputItem] = match &body.input {
        Some(ResponseInput::Items(items)) => items,
        _ => &[],
    };
    items.iter().enumerate().flat_map(|(i, item)| {
        let parts: &[InputContentPart] = match item {
            InputItem::Message(message) => match &message.content {
                InputContent::Parts(parts) => parts,
                InputContent::Text(_) => &[],
            },
            _ => &[],
        };
        parts
            .iter()
            .enumerate()
            .map(move |(j, part)| ((i, j), part))
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "media_tests.rs"]
mod media_tests;
