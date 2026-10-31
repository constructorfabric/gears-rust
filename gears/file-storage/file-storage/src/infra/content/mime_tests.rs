use super::*;

const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
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
