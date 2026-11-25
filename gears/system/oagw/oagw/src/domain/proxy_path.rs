//! Canonical form of a proxied request path, the form route matching runs
//! on. The proxy and the SDK's `resolve_proxy_target` both go through
//! [`canonical_path`], so they select the same route.

use oagw_sdk::field;
use std::borrow::Cow;
use toolkit_macros::domain_model;
use unicode_normalization::UnicodeNormalization;

use crate::domain::error::DomainError;

/// Reject a path that could resolve differently at the upstream, then
/// normalize it: encoded unreserved characters decoded, other escapes
/// uppercased, `.`/`..` resolved and `//` collapsed. `instance` names the
/// request in a rejection.
pub(crate) fn canonical_path(raw: &str, instance: &str) -> Result<String, DomainError> {
    reject_encoded_dot_segments(raw, instance)?;
    Ok(normalize_path(&normalize_percent_encoding(raw)))
}

/// What [`canonical_path`] removes from a path besides percent-encoding
/// changes, which keep the resource the path names.
#[domain_model]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PathRewrite {
    /// An empty, `.` or `..` segment was removed.
    pub(crate) segments: bool,
    /// The path ends with `/` after a segment; normalization removes it.
    /// The upstream URL gets it back when the route path ends with `/` and
    /// nothing follows it, and gets one the client did not send in that
    /// case too, so callers compare it with the URL as built.
    pub(crate) trailing_slash: bool,
}

/// How [`canonical_path`] rewrites `raw` (a path [`canonical_path`]
/// accepts). Upstreams that resolve `.`/`..` and `//` themselves see the
/// same resource; an object store such as S3 sees another key.
pub(crate) fn path_rewrite(raw: &str) -> PathRewrite {
    let path = normalize_percent_encoding(raw);
    let inner = path.strip_prefix('/').unwrap_or(&path);
    let (body, trailing) = match inner.strip_suffix('/') {
        Some(body) => (body, true),
        None => (inner, false),
    };
    if body.is_empty() {
        // `//` is an empty segment; `/` and the empty path stay as they are.
        return PathRewrite {
            segments: trailing,
            trailing_slash: false,
        };
    }
    PathRewrite {
        segments: body.split('/').any(|s| matches!(s, "" | "." | "..")),
        trailing_slash: trailing,
    }
}

/// Decoding rounds [`reject_encoded_dot_segments`] follows before it gives up
/// on a segment that keeps changing (`%25` nested this deep).
const MAX_DECODE_ROUNDS: usize = 3;

/// Reject a path segment that is a dot-segment once decoded: `%2e%2e`,
/// `.%2E`, `..%2f`, `..\`, a `..;param` path parameter, or the same encoded
/// again (`%252e%252e`). `normalize_path` resolves literal `.` and `..`, but an
/// upstream that decodes the path would resolve these after route matching
/// and escape the route prefix (#4979). Percent-encoding that is not UTF-8
/// (overlong forms such as `%c0%ae`) is rejected too. An encoded `/` in a
/// normal segment (`a%2Fb`) is forwarded as is.
fn reject_encoded_dot_segments(raw_path: &str, instance: &str) -> Result<(), DomainError> {
    let invalid = |detail: &str| DomainError::Validation {
        field: "path",
        reason: field::INVALID_PROXY_PATH,
        detail: detail.to_owned(),
        instance: instance.to_owned(),
    };
    for segment in raw_path.split('/') {
        if matches!(segment, "." | "..") {
            continue;
        }
        if segment.contains(';') {
            // `;` starts a path parameter some servers strip before routing
            // (`/admin;x/y` served as `/admin/y`), which would match here a
            // broader route than the upstream serves.
            return Err(invalid("proxy path has a `;` path parameter"));
        }
        if !is_valid_percent_encoding(segment) {
            return Err(invalid(
                "proxy path has a `%` not followed by two hex digits",
            ));
        }
        let mut current = std::borrow::Cow::Borrowed(segment);
        let mut stable = false;
        for _ in 0..=MAX_DECODE_ROUNDS {
            if is_dot_segment(&current) {
                return Err(invalid("proxy path contains an encoded dot-segment"));
            }
            if current.contains(';') {
                return Err(invalid("proxy path has an encoded `;` path parameter"));
            }
            let decoded = percent_encoding::percent_decode_str(&current)
                .decode_utf8()
                .map_err(|_| invalid("proxy path percent-encoding is not valid UTF-8"))?;
            if decoded == current {
                stable = true;
                break;
            }
            current = std::borrow::Cow::Owned(decoded.into_owned());
        }
        if !stable {
            return Err(invalid("proxy path is percent-encoded too many times"));
        }
    }
    Ok(())
}

/// Whether every `%` in `s` starts an escape of two hex digits (RFC 3986
/// §2.1). A stray `%` would let normalization build a new escape from the
/// characters after it.
fn is_valid_percent_encoding(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.iter().enumerate().all(|(i, b)| {
        *b != b'%'
            || (bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit))
    })
}

/// Whether a (decoded) segment is `.` or `..`, split on `/` and `\` and cut at a
/// `;` path parameter, `?` or `#`, as servers that honour those resolve it.
fn is_dot_segment(segment: &str) -> bool {
    // Upstreams may fold Unicode compatibility forms (NFKC, or a similar
    // best-fit mapping) before they resolve the path: fullwidth and vertical
    // dot forms become `.` or `..`, fullwidth and small solidi `/` and `\\`.
    // NFKC leaves ASCII as it is.
    let folded: Cow<'_, str> = if segment.is_ascii() {
        Cow::Borrowed(segment)
    } else {
        Cow::Owned(segment.nfkc().collect())
    };
    folded.split(['/', '\\']).any(|piece| {
        // A server that re-parses the decoded path ends the segment at a
        // `;` parameter, a `?` query or a `#` fragment.
        let piece = piece.split([';', '?', '#']).next().unwrap_or(piece);
        // `..%00` and `..%20` resolve as `..` on servers that stop at NUL or
        // trim spaces.
        let piece = piece.trim_matches(|c: char| c.is_whitespace() || c.is_control());
        matches!(piece, "." | "..")
    })
}

/// Refuse a route path prefix that a request path could never match: one
/// that is not absolute, or one the proxy rejects in requests (see
/// [`reject_encoded_dot_segments`]).
pub(crate) fn validate_route_prefix(prefix: &str) -> Result<(), DomainError> {
    let invalid = |detail: &str| DomainError::Validation {
        field: "match.http.path",
        reason: field::INVALID_PROXY_PATH,
        detail: detail.into(),
        instance: String::new(),
    };
    // Request paths are absolute (or empty, for the bare alias).
    let Some(inner) = prefix.strip_prefix('/') else {
        return Err(invalid("route path must start with `/`"));
    };
    // Requests are matched with `.`/`..` resolved and `//` collapsed, so a
    // prefix with such a segment cannot match either. Only the leading and
    // a trailing `/` leave an empty segment, and `/` itself is the root.
    if inner.is_empty() {
        return Ok(());
    }
    let inner = inner.strip_suffix('/').unwrap_or(inner);
    if inner
        .split('/')
        .any(|segment| matches!(segment, "" | "." | ".."))
    {
        return Err(invalid("route path has an empty, `.` or `..` segment"));
    }
    reject_encoded_dot_segments(prefix, "").map_err(|e| match e {
        DomainError::Validation { detail, .. } => DomainError::Validation {
            field: "match.http.path",
            reason: field::INVALID_PROXY_PATH,
            detail: detail.replace("proxy path", "route path"),
            instance: String::new(),
        },
        other => other,
    })
}

/// [`normalize_path`] for tests of the steps around it; production code
/// goes through [`canonical_path`].
#[cfg(test)]
pub(crate) fn resolve_dots_for_tests(path: &str) -> String {
    normalize_path(path)
}

/// Normalize a URL path: collapse consecutive slashes and resolve `.`/`..` segments.
/// Segments that would escape above the root are discarded.
fn normalize_path(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            s => segments.push(s),
        }
    }
    let mut result = String::with_capacity(path.len());
    if path.starts_with('/') {
        result.push('/');
    }
    result.push_str(&segments.join("/"));
    result
}

/// Decode percent-encoded unreserved characters (`ALPHA DIGIT - . _ ~`) and
/// uppercase the remaining escapes (RFC 3986 §6.2.2). Route matching runs on
/// this form, so `/v1/ch%61t` is matched (and forwarded) as `/v1/chat`
/// instead of falling through to a broader route.
pub(crate) fn normalize_percent_encoding(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = String::with_capacity(path.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).and_then(|b| (*b as char).to_digit(16)),
                bytes.get(i + 2).and_then(|b| (*b as char).to_digit(16)),
            )
        {
            let byte = u8::try_from(hi * 16 + lo).unwrap_or(0);
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                out.push(char::from(byte));
            } else {
                out.push('%');
                out.push(char::from(bytes[i + 1].to_ascii_uppercase()));
                out.push(char::from(bytes[i + 2].to_ascii_uppercase()));
            }
            i += 3;
            continue;
        }
        // Not an escape: copy one UTF-8 character.
        let ch = path[i..].chars().next().unwrap_or_default();
        out.push(ch);
        i += ch.len_utf8().max(1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn rewrite(segments: bool, trailing_slash: bool) -> PathRewrite {
        PathRewrite {
            segments,
            trailing_slash,
        }
    }

    #[test]
    fn path_rewrite_reports_what_normalization_removes() {
        for (raw, expected) in [
            ("", rewrite(false, false)),
            ("/", rewrite(false, false)),
            ("/a", rewrite(false, false)),
            ("/a/b", rewrite(false, false)),
            ("a", rewrite(false, false)),
            (".json", rewrite(false, false)),
            ("/a..b/.x", rewrite(false, false)),
            ("/a%2Fb", rewrite(false, false)),
            ("/%41/%2f", rewrite(false, false)),
            ("//", rewrite(true, false)),
            ("//a", rewrite(true, false)),
            ("/a//b", rewrite(true, false)),
            ("/a/./b", rewrite(true, false)),
            ("/a/../b", rewrite(true, false)),
            ("/a/.", rewrite(true, false)),
            ("/a/", rewrite(false, true)),
            ("/a/b/", rewrite(false, true)),
            ("/a//", rewrite(true, true)),
            ("/a/./", rewrite(true, true)),
        ] {
            assert_eq!(path_rewrite(raw), expected, "{raw}");
        }
    }

    /// A path [`path_rewrite`] reports as unchanged keeps its segments
    /// through [`canonical_path`]; one it reports as changed does not.
    #[test]
    fn path_rewrite_agrees_with_canonical_path() {
        for raw in [
            "", "/", "/a", "/a/b", "/%41", "/a%2fb", "//", "//a", "/a//b", "/a/./b", "/a/../b",
            "/a/.", "/a/", "/a//", "/./", "/..",
        ] {
            let canonical = canonical_path(raw, "").unwrap();
            let kept = canonical == normalize_percent_encoding(raw);
            assert_eq!(path_rewrite(raw) == PathRewrite::default(), kept, "{raw}");
        }
    }
}
