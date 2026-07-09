use super::*;

// Minimal PNG signature (8-byte magic).
const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
// %PDF-1.4 header.
const PDF_MAGIC: &[u8] = b"%PDF-1.4\n";

#[test]
fn detect_recognizes_png() {
    assert_eq!(detect(PNG_MAGIC), Some("image/png"));
}

#[test]
fn detect_returns_none_for_plain_text() {
    assert_eq!(detect(b"just some text, no signature"), None);
}

#[test]
fn validate_accepts_matching_declared_type() {
    assert!(validate("image/png", PNG_MAGIC).is_ok());
}

#[test]
fn validate_accepts_declared_with_charset_param() {
    // essence comparison ignores `; charset=...`
    assert!(validate("image/png; charset=binary", PNG_MAGIC).is_ok());
}

#[test]
fn validate_rejects_mismatch() {
    // declared png but bytes are pdf → mismatch
    let err = validate("image/png", PDF_MAGIC).unwrap_err();
    assert!(
        matches!(err, DomainError::MimeMismatch { .. }),
        "expected MimeMismatch, got {err:?}"
    );
}

#[test]
fn validate_accepts_unrecognized_content_as_declared() {
    // no signature → cannot prove a mismatch → accept
    assert!(validate("text/csv", b"a,b,c\n1,2,3").is_ok());
}
