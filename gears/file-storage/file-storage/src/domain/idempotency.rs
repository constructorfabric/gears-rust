//! Domain types for upload idempotency.

use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::infra::content::hash;

/// The stored response for an idempotency key lookup.
/// Returned to a retrying caller unchanged.
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
    /// SHA-256 over [`compute_request_hash`]'s canonicalized encoding of the
    /// request that created this record. A replay recomputes this hash from
    /// the current request and rejects a mismatch as `Conflict` (P2
    /// remediation 2.1) — see `FileService::create_file`.
    pub request_hash: Vec<u8>,
}

/// Canonicalize and hash the identity-relevant fields of a `POST /files`
/// request, for idempotency-replay body-match verification (P2 remediation
/// 2.1).
///
/// Every field is length-prefixed (a 4-byte little-endian length followed by
/// its bytes) before being fed to the hasher. `hash::sha256_parts` merely
/// concatenates its inputs with no delimiter between them, so a naive
/// `sha256_parts(&[name.as_bytes(), gts.as_bytes()])` would hash
/// `(name="ab", gts="c")` and `(name="a", gts="bc")` identically, letting a
/// caller dodge the mismatch check by shifting bytes across adjacent fields.
/// Length-prefixing removes that ambiguity without changing
/// `sha256_parts`/`sha256` semantics for their other call site
/// (`domain::etag::content_etag`, which only ever hashes fixed-width UUID
/// bytes plus a constant prefix — no ambiguity there, and changing the
/// helper's semantics would alter already-issued `ETags` for no benefit).
///
/// `custom_metadata` is sorted by key before hashing so two textually-
/// identical-but-reordered requests hash identically — the wire order of a
/// JSON object's keys is not guaranteed stable across two otherwise-equal
/// requests.
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

/// Append `bytes` to `buf`, preceded by its length as 4 little-endian bytes —
/// an unambiguous field delimiter so no two distinct field sequences can ever
/// serialize to the same buffer (see [`compute_request_hash`]).
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
        // (name="ab", gts="c") must not hash the same as (name="a", gts="bc") —
        // proves the length-prefix delimiter, not naive concatenation, is in
        // effect.
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
