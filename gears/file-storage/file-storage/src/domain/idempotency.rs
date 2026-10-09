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

    #[test]
    fn compute_request_hash_is_deterministic() {
        let meta = vec![
            ("a".to_owned(), "1".to_owned()),
            ("b".to_owned(), "2".to_owned()),
        ];
        let h1 = compute_request_hash("user", Uuid::nil(), "n", "gts", "mime", &meta);
        let h2 = compute_request_hash("user", Uuid::nil(), "n", "gts", "mime", &meta);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 32, "SHA-256 digest must be 32 bytes");
    }

    #[test]
    fn compute_request_hash_is_order_independent_for_metadata() {
        let meta_a = vec![
            ("a".to_owned(), "1".to_owned()),
            ("b".to_owned(), "2".to_owned()),
        ];
        let meta_b = vec![
            ("b".to_owned(), "2".to_owned()),
            ("a".to_owned(), "1".to_owned()),
        ];
        let h_a = compute_request_hash("user", Uuid::nil(), "n", "gts", "mime", &meta_a);
        let h_b = compute_request_hash("user", Uuid::nil(), "n", "gts", "mime", &meta_b);
        assert_eq!(h_a, h_b, "metadata order must not affect the hash");
    }

    #[test]
    fn compute_request_hash_does_not_collide_across_field_boundaries() {
        let h1 = compute_request_hash("user", Uuid::nil(), "ab", "c", "mime", &[]);
        let h2 = compute_request_hash("user", Uuid::nil(), "a", "bc", "mime", &[]);
        assert_ne!(h1, h2, "field-boundary shift must not collide");
    }

    #[test]
    fn compute_request_hash_differs_on_owner_id() {
        let owner_a = Uuid::from_u128(1);
        let owner_b = Uuid::from_u128(2);
        let h_a = compute_request_hash("user", owner_a, "n", "gts", "mime", &[]);
        let h_b = compute_request_hash("user", owner_b, "n", "gts", "mime", &[]);
        assert_ne!(h_a, h_b, "owner_id must be covered by the hash");
    }

    #[test]
    fn compute_request_hash_differs_on_metadata_value() {
        let meta_a = vec![("k".to_owned(), "v1".to_owned())];
        let meta_b = vec![("k".to_owned(), "v2".to_owned())];
        let h_a = compute_request_hash("user", Uuid::nil(), "n", "gts", "mime", &meta_a);
        let h_b = compute_request_hash("user", Uuid::nil(), "n", "gts", "mime", &meta_b);
        assert_ne!(h_a, h_b, "metadata values must be covered by the hash");
    }
}
