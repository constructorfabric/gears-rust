//! Domain types for upload idempotency.

use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::infra::content::hash;

/// The stored response for an idempotency key, returned unchanged to a retrying caller.
#[domain_model]
#[derive(Debug, Clone)]
pub struct IdempotencyRecord {
    pub file_id: Uuid,
    /// The authenticated subject that created this record
    /// (`ctx.subject_id()` at insert time). The domain layer must verify this
    /// matches the replaying caller before handing back `response_body` —
    /// see `FileService::create_file`.
    pub subject_id: Uuid,
    /// HTTP status code of the original response (e.g. 201).
    pub response_status: u16,
    /// JSON-serialized `UploadTicketDto` body.
    pub response_body: String,
    pub response_etag: String,
    /// SHA-256 over [`compute_request_hash`]'s encoding of the creating request. A replay
    /// recomputes it and rejects a mismatch as `Conflict` (see `FileService::create_file`).
    pub request_hash: Vec<u8>,
}

/// Canonicalize and hash the identity-relevant fields of a `POST /files` request, for
/// idempotency-replay body-match verification.
///
/// Every field is length-prefixed (4-byte little-endian): `hash::sha256_parts` concatenates
/// without delimiters, so `(name="ab", gts="c")` and `(name="a", gts="bc")` would otherwise
/// collide. The helper itself is left unchanged because `content_etag` also uses it and
/// altering it would change already-issued `ETags`.
///
/// `custom_metadata` is sorted by key so wire key order does not affect the hash.
#[must_use]
pub fn compute_request_hash(
    owner_kind: &str,
    owner_id: Uuid,
    name: &str,
    gts_file_type: &str,
    mime_type: &str,
    custom_metadata: &[(String, String)],
) -> Vec<u8> {
    let mut sorted_metadata: Vec<&(String, String)> = custom_metadata.iter().collect();
    sorted_metadata.sort_by(|a, b| a.0.cmp(&b.0));

    let mut buf = Vec::new();
    push_field(&mut buf, owner_kind.as_bytes());
    push_field(&mut buf, owner_id.as_bytes());
    push_field(&mut buf, name.as_bytes());
    push_field(&mut buf, gts_file_type.as_bytes());
    push_field(&mut buf, mime_type.as_bytes());
    for (key, value) in sorted_metadata {
        push_field(&mut buf, key.as_bytes());
        push_field(&mut buf, value.as_bytes());
    }
    hash::sha256(&buf)
}

/// Append `bytes` to `buf`, preceded by its length as 4 little-endian bytes (an
/// unambiguous delimiter; see [`compute_request_hash`]).
fn push_field(buf: &mut Vec<u8>, bytes: &[u8]) {
    #[allow(clippy::cast_possible_truncation)]
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn compute_request_hash_is_order_independent_for_metadata() {
        let hash = |m: &[(String, String)]| {
            compute_request_hash("user", Uuid::nil(), "n", "gts", "mime", m)
        };
        let ab = meta(&[("a", "1"), ("b", "2")]);
        let ba = meta(&[("b", "2"), ("a", "1")]);
        assert_eq!(hash(&ab), hash(&ab), "the hash must be deterministic");
        assert_eq!(
            hash(&ab),
            hash(&ba),
            "metadata order must not affect the hash"
        );
    }

    #[test]
    fn compute_request_hash_covers_every_field_without_boundary_collisions() {
        let u = Uuid::from_u128;
        // (name, hash of a variant); every variant must differ from `base`.
        let base = compute_request_hash("user", u(1), "n", "gts", "mime", &meta(&[("k", "v1")]));
        for (name, variant) in [
            (
                "owner_id",
                compute_request_hash("user", u(2), "n", "gts", "mime", &meta(&[("k", "v1")])),
            ),
            (
                "metadata value",
                compute_request_hash("user", u(1), "n", "gts", "mime", &meta(&[("k", "v2")])),
            ),
            (
                "field boundary shift",
                compute_request_hash("user", u(1), "n", "gts", "mime", &meta(&[("kv", "1")])),
            ),
            (
                "name/gts boundary shift",
                compute_request_hash("user", u(1), "ng", "ts", "mime", &meta(&[("k", "v1")])),
            ),
        ] {
            assert_ne!(base, variant, "{name} must be covered by the hash");
        }
    }
}
