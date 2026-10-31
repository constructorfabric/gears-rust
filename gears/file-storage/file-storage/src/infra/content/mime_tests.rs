use super::*;

const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
const PDF_MAGIC: &[u8] = b"%PDF-1.4\n";

#[test]
fn detect_recognizes_signatures_only() {
    assert_eq!(detect(PNG_MAGIC), Some("image/png"));
    assert_eq!(detect(b"just some text, no signature"), None);
}

#[test]
fn validate_compares_the_declared_essence_with_the_detected_type() {
    // (declared, bytes, accepted). The essence comparison ignores `; charset=...`; content
    // without a signature cannot prove a mismatch, so it is accepted as declared.
    for (declared, bytes, accepted) in [
        ("image/png", PNG_MAGIC, true),
        ("image/png; charset=binary", PNG_MAGIC, true),
        ("text/csv", b"a,b,c\n1,2,3".as_slice(), true),
        ("image/png", PDF_MAGIC, false),
    ] {
        match validate(declared, bytes) {
            Ok(()) => assert!(accepted, "{declared} must be rejected"),
            Err(DomainError::MimeMismatch { .. }) => assert!(!accepted, "{declared}"),
            Err(other) => panic!("{declared}: expected MimeMismatch, got {other:?}"),
        }
    }
}
