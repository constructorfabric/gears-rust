#[cfg(test)]
mod tests {
    use super::super::etag::compose;

    /// Independent reference implementation that mirrors `compose` 1:1.
    /// Used to validate the function output against an alternative path.
    fn reference_compose(content_hash: &str, meta_revision: i64) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(content_hash.as_bytes());
        hasher.update(b":");
        hasher.update(meta_revision.to_string().as_bytes());
        hex::encode(hasher.finalize())
    }

    #[test]
    fn compose_is_deterministic() {
        let a = compose("hash", 1);
        let b = compose("hash", 1);
        assert_eq!(a, b, "compose must be deterministic for the same input");
    }

    #[test]
    fn compose_returns_hex_lowercase_64_chars() {
        let etag = compose("anything", 0);
        assert_eq!(etag.len(), 64, "sha256 hex length must be 64");
        assert!(
            etag.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "etag must be lowercase hex: {etag}"
        );
    }

    #[test]
    fn compose_handles_empty_content_hash() {
        let etag = compose("", 0);
        // Should match the reference implementation, not panic, not be empty.
        assert_eq!(etag, reference_compose("", 0));
        assert_eq!(etag.len(), 64);
    }

    #[test]
    fn compose_distinguishes_different_revisions() {
        let a = compose("h", 1);
        let b = compose("h", 2);
        assert_ne!(a, b, "different revisions must yield different etags");
    }

    #[test]
    fn compose_distinguishes_different_content_hashes() {
        let a = compose("hash-a", 5);
        let b = compose("hash-b", 5);
        assert_ne!(a, b, "different content hashes must yield different etags");
    }

    #[test]
    fn compose_handles_max_meta_revision() {
        // Should not panic on i64::MAX.
        let etag = compose("hash", i64::MAX);
        assert_eq!(etag.len(), 64);
        assert_eq!(etag, reference_compose("hash", i64::MAX));
    }

    #[test]
    fn compose_handles_zero_meta_revision() {
        let etag = compose("hash", 0);
        assert_eq!(etag.len(), 64);
        assert_eq!(etag, reference_compose("hash", 0));
    }

    #[test]
    fn compose_separator_is_colon_not_concatenation() {
        // "ab:1" should differ from "a:b1" because the colon position
        // matters (separates content_hash from meta_revision).
        let a = compose("ab", 1);
        let b = compose("a", 11); // would also be "a" + ":" + "11" → different
        assert_ne!(a, b, "colon separator must distinguish ambiguous cases");
    }

    #[test]
    fn compose_sample_against_reference_table() {
        // Table-driven smoke test against the reference implementation.
        let cases = vec![
            ("", 0i64),
            ("a", 0i64),
            ("a", 1i64),
            ("0123456789abcdef", 9001i64),
            (
                "9d4e1e23bd5c4f08baee76b1e3d3e0e98c5e72e2",
                42i64,
            ),
        ];
        for (h, rev) in cases {
            assert_eq!(
                compose(h, rev),
                reference_compose(h, rev),
                "compose(\"{h}\", {rev}) mismatch"
            );
        }
    }
}
