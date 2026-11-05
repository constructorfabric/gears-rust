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

#[test]
fn detect_recognizes_pdf_at_offset_zero() {
    assert_eq!(detect(PDF_MAGIC), Some("application/pdf"));
}

#[test]
fn detect_ignores_pdf_marker_in_markdown() {
    let md = b"# File formats\n\nA PDF file starts with the `%PDF-1.7` header.\n";
    assert_eq!(detect(md), None);
}

#[test]
fn validate_accepts_markdown_mentioning_pdf_marker() {
    let md = b"# File formats\n\nA PDF file starts with the `%PDF-1.7` header.\n";
    assert!(validate("text/markdown", md).is_ok());
}

#[test]
fn detect_keeps_html_mentioning_pdf_marker() {
    let html = b"<!DOCTYPE html><html><body>%PDF magic</body></html>";
    assert_eq!(detect(html), Some("text/html"));
}

#[test]
fn detect_recognizes_matroska() {
    let mut mkv = vec![0x1a, 0x45, 0xdf, 0xa3, 0x9f, 0x42, 0x82, 0x88];
    mkv.extend_from_slice(b"matroska");
    mkv.resize(300, 0);
    assert_eq!(detect(&mkv), Some("video/x-matroska"));
}

#[test]
fn detect_recognizes_rtf_mentioning_pdf_marker() {
    assert_eq!(
        detect(b"{\\rtf1\\ansi The %PDF-1.7 header.\\par}"),
        Some("application/rtf")
    );
}

#[test]
fn validate_rejects_rtf_with_pdf_marker_declared_as_png() {
    assert!(validate("image/png", b"{\\rtf1\\ansi The %PDF-1.7 header.\\par}").is_err());
}
