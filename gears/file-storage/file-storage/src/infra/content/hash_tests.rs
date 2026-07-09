use super::*;

// SHA-256("") and SHA-256("abc") are well-known fixed vectors.
const EMPTY_HEX: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const ABC_HEX: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

#[test]
fn sha256_known_vectors() {
    assert_eq!(sha256_hex(b""), EMPTY_HEX);
    assert_eq!(sha256_hex(b"abc"), ABC_HEX);
}

#[test]
fn sha256_digest_is_32_bytes() {
    assert_eq!(sha256(b"anything").len(), 32);
}

#[test]
fn streaming_hasher_matches_oneshot() {
    let mut h = Hasher::new();
    h.update(b"ab");
    h.update(b"c");
    assert_eq!(h.len(), 3);
    assert!(!h.is_empty());
    assert_eq!(hex::encode(h.finalize()), ABC_HEX);
}

#[test]
fn empty_hasher_reports_empty() {
    let h = Hasher::new();
    assert!(h.is_empty());
    assert_eq!(h.len(), 0);
    assert_eq!(hex::encode(h.finalize()), EMPTY_HEX);
}
