//! Pure ETag formula for the file-storage control plane.
//!
//! The content ETag is a deterministic opaque token derived from `(file_id,
//! content_id)` via a SHA-256 over a domain-separation prefix (not a secret key —
//! it is a fingerprint, not a MAC). It is opaque by design — it never encodes the
//! raw content hash that backs the version row — and is defined once here so every
//! call site (service, DTO, handler) reads from the same source of truth.

// Domain terms (ETag, If-Match) appear in comments below.
#![allow(clippy::doc_markdown)]

use uuid::Uuid;

use file_storage_sdk::File;

use crate::infra::content::hash;

/// Derive the opaque content ETag from a `(file_id, content_id)` pair.
///
/// The ETag is quoted (`"<hex>"`) per RFC 9110 §8.8.3. The 16-byte prefix of the
/// SHA-256 digest gives 128 bits of collision resistance — sufficient for an
/// optimistic-concurrency token (DESIGN §3.1, §4.2).
#[must_use]
pub fn content_etag(file_id: Uuid, content_id: Uuid) -> String {
    let digest = hash::sha256_parts(&[b"fs-etag-v1", file_id.as_bytes(), content_id.as_bytes()]);
    format!("\"{}\"", hex::encode(&digest[..16]))
}

/// Return the current content ETag for `file`, or `None` if no content is bound
/// yet (`file.content_id` is `None`).
#[must_use]
pub fn etag_for(file: &File) -> Option<String> {
    file.content_id.map(|cid| content_etag(file.file_id, cid))
}

/// Decide whether a conditional `If-None-Match` is satisfied against
/// `current_etag` — i.e. whether the caller's cached copy is still current
/// and a `304` should be returned instead of the file. Shared by any future
/// caller that needs the exact same match rule, per RFC 9110 §13.1.2: `"*"`, or a comma-separated
/// list of entity-tags (each optionally weak, `W/"..."`, and a tag's own
/// quoted content may itself contain commas) any of which matches
/// `current_etag` under *weak comparison* — the `W/` prefix, if any, is
/// stripped from both sides before comparing the opaque quoted token. No
/// match, or no current ETag to compare against, is never "unchanged".
#[must_use]
pub fn if_none_match_satisfied(if_none_match: Option<&str>, current_etag: Option<&str>) -> bool {
    let (Some(inm), Some(tag)) = (if_none_match, current_etag) else {
        return false;
    };
    let inm = inm.trim();
    if inm == "*" {
        return true;
    }
    split_etag_list(inm).any(|candidate| weak_tag(candidate) == weak_tag(tag))
}

/// Strip an entity-tag's weak-validator prefix (`W/`), if present, leaving
/// the opaque quoted token used for *weak comparison* (RFC 9110 §8.8.3.2).
#[must_use]
fn weak_tag(raw: &str) -> &str {
    raw.strip_prefix("W/").unwrap_or(raw)
}

/// Split an `If-None-Match` field value into trimmed, non-empty entity-tag
/// candidates, separated by commas — except a comma inside a quoted tag
/// (e.g. `"a,b"`), which is part of the tag and must not be split on.
/// Byte-wise, allocation-free: tracks an "inside quotes" flag while
/// scanning for the next unquoted comma.
#[must_use]
fn split_etag_list(list: &str) -> EtagListSplit<'_> {
    EtagListSplit { rest: list }
}

/// Iterator returned by [`split_etag_list`].
struct EtagListSplit<'a> {
    rest: &'a str,
}

impl<'a> Iterator for EtagListSplit<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        while !self.rest.is_empty() {
            let mut in_quotes = false;
            let mut comma_at = None;
            for (i, c) in self.rest.char_indices() {
                match c {
                    '"' => in_quotes = !in_quotes,
                    ',' if !in_quotes => {
                        comma_at = Some(i);
                        break;
                    }
                    _ => {}
                }
            }
            let (item, rest) = match comma_at {
                Some(i) => (&self.rest[..i], &self.rest[i + 1..]),
                None => (self.rest, ""),
            };
            self.rest = rest;
            let item = item.trim();
            if !item.is_empty() {
                return Some(item);
            }
        }
        None
    }
}
