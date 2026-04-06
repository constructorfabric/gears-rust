use super::*;

#[test]
fn accepts_all_document_types() {
    let doc_types = [
        MIME_PDF,
        MIME_PLAIN,
        MIME_MARKDOWN,
        MIME_HTML,
        MIME_DOCX,
        MIME_PPTX,
        MIME_JSON,
        MIME_PYTHON,
        MIME_JAVA,
        MIME_JAVASCRIPT,
        MIME_TYPESCRIPT,
        MIME_RUST,
        MIME_GO,
        MIME_CSHARP,
        MIME_RUBY,
        MIME_SQL,
    ];
    for mime in doc_types {
        let result = validate_mime(mime).unwrap_or_else(|_| panic!("should accept {mime}"));
        assert_eq!(result.mime, mime);
        assert!(
            matches!(result.kind, AttachmentKind::Document),
            "{mime} should be Document"
        );
    }
}

#[test]
fn accepts_all_image_types() {
    let img_types = [MIME_PNG, MIME_JPEG, MIME_WEBP, MIME_GIF];
    for mime in img_types {
        let result = validate_mime(mime).unwrap_or_else(|_| panic!("should accept {mime}"));
        assert_eq!(result.mime, mime);
        assert!(
            matches!(result.kind, AttachmentKind::Image),
            "{mime} should be Image"
        );
    }
}

#[test]
fn total_accepted_types_is_21() {
    assert_eq!(ACCEPTED_MIMES.len(), 21);
    for spec in ACCEPTED_MIMES {
        assert!(
            validate_mime(spec.mime).is_ok(),
            "should accept {}",
            spec.mime
        );
    }
}

#[test]
fn strips_charset_parameter() {
    let result = validate_mime("text/plain; charset=utf-8").unwrap();
    assert_eq!(result.mime, MIME_PLAIN);
    assert!(matches!(result.kind, AttachmentKind::Document));
}

#[test]
fn strips_multiple_parameters() {
    let result = validate_mime("text/html; charset=utf-8; boundary=something").unwrap();
    assert_eq!(result.mime, MIME_HTML);
}

#[test]
fn case_insensitive() {
    let result = validate_mime("Application/PDF").unwrap();
    assert_eq!(result.mime, MIME_PDF);

    let result = validate_mime("IMAGE/PNG").unwrap();
    assert_eq!(result.mime, MIME_PNG);
}

#[test]
fn rejects_octet_stream() {
    assert!(validate_mime(MIME_OCTET_STREAM).is_err());
}

#[test]
fn rejects_unknown_types() {
    assert!(validate_mime("application/xml").is_err());
    assert!(validate_mime("video/mp4").is_err());
    assert!(validate_mime("audio/mpeg").is_err());
    assert!(validate_mime("application/zip").is_err());
    // CSV is only accepted via remap_csv_to_plain; validate_mime alone rejects it.
    assert!(validate_mime(MIME_CSV).is_err());
}

#[test]
fn rejects_empty_string() {
    assert!(validate_mime("").is_err());
}

#[test]
fn handles_whitespace() {
    let result = validate_mime("  text/plain  ").unwrap();
    assert_eq!(result.mime, MIME_PLAIN);
}

#[test]
fn structured_filename_format() {
    let chat = uuid::Uuid::nil();
    let att = uuid::Uuid::nil();
    let name = structured_filename(chat, att, MIME_PDF);
    assert!(
        std::path::Path::new(&name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
    );
    assert!(name.contains('_'));
}

#[test]
fn infer_md_from_extension() {
    assert_eq!(infer_mime_from_extension("readme.md"), Some(MIME_MARKDOWN));
    assert_eq!(infer_mime_from_extension("NOTES.MD"), Some(MIME_MARKDOWN));
    assert_eq!(
        infer_mime_from_extension("doc.markdown"),
        Some(MIME_MARKDOWN)
    );
}

#[test]
fn infer_csv_from_extension() {
    assert_eq!(infer_mime_from_extension("data.csv"), Some(MIME_CSV));
}

#[test]
fn infer_common_extensions() {
    assert_eq!(infer_mime_from_extension("file.pdf"), Some(MIME_PDF));
    assert_eq!(infer_mime_from_extension("code.rs"), Some(MIME_RUST));
    assert_eq!(infer_mime_from_extension("photo.jpg"), Some(MIME_JPEG));
    assert_eq!(infer_mime_from_extension("photo.jpeg"), Some(MIME_JPEG));
    assert_eq!(infer_mime_from_extension("app.ts"), Some(MIME_TYPESCRIPT));
    assert_eq!(infer_mime_from_extension("app.mts"), Some(MIME_TYPESCRIPT));
}

#[test]
fn infer_unknown_extension_returns_none() {
    assert_eq!(infer_mime_from_extension("archive.zip"), None);
    assert_eq!(infer_mime_from_extension("video.mp4"), None);
    assert_eq!(infer_mime_from_extension("noext"), None);
    // Dotless filename that coincides with a known extension must not match.
    assert_eq!(infer_mime_from_extension("md"), None);
    assert_eq!(infer_mime_from_extension("pdf"), None);
}

#[test]
fn infer_then_validate_md() {
    let mime = infer_mime_from_extension("readme.md").unwrap();
    let result = validate_mime(mime).unwrap();
    assert_eq!(result.mime, MIME_MARKDOWN);
    assert!(matches!(result.kind, AttachmentKind::Document));
}

#[test]
fn csv_remapped_to_plain() {
    assert_eq!(remap_csv_to_plain(MIME_CSV), Some(MIME_PLAIN));
    assert_eq!(
        remap_csv_to_plain("text/csv; charset=utf-8"),
        Some(MIME_PLAIN)
    );
    assert_eq!(remap_csv_to_plain("TEXT/CSV"), Some(MIME_PLAIN));
}

#[test]
fn remap_csv_ignores_non_csv() {
    assert_eq!(remap_csv_to_plain(MIME_PLAIN), None);
    assert_eq!(remap_csv_to_plain(MIME_PDF), None);
}

#[test]
fn csv_after_remap_passes_validation() {
    let remapped = remap_csv_to_plain(MIME_CSV).unwrap();
    let result = validate_mime(remapped).unwrap();
    assert_eq!(result.mime, MIME_PLAIN);
    assert!(matches!(result.kind, AttachmentKind::Document));
}

#[test]
fn all_mimes_have_extensions() {
    for spec in ACCEPTED_MIMES {
        let ext = mime_to_extension(spec.mime);
        assert_ne!(
            ext, "bin",
            "MIME {} should not fall back to .bin",
            spec.mime
        );
    }
}

#[test]
fn xlsx_is_accepted_as_document() {
    let result = validate_mime(MIME_XLSX);
    assert!(result.is_ok());
    let validated = result.unwrap();
    assert_eq!(validated.kind, AttachmentKind::Document);
}

#[test]
fn xlsx_extension_infers_correct_mime() {
    assert_eq!(infer_mime_from_extension("data.xlsx"), Some(MIME_XLSX));
}

#[test]
fn xlsx_mime_maps_to_extension() {
    assert_eq!(mime_to_extension(MIME_XLSX), "xlsx");
}

#[test]
fn xlsx_resolves_to_code_interpreter_purpose() {
    let purposes = resolve_purposes(MIME_XLSX);
    assert_eq!(purposes, vec![AttachmentPurpose::CodeInterpreter]);
}

#[test]
fn pdf_resolves_to_file_search_purpose() {
    let purposes = resolve_purposes(MIME_PDF);
    assert_eq!(purposes, vec![AttachmentPurpose::FileSearch]);
}

// ── truncate_filename tests ─────────────────────────────────────────

#[test]
#[allow(
    clippy::non_ascii_literal,
    clippy::manual_str_repeat,
    clippy::manual_repeat_n
)]
fn truncate_filename_cases() {
    let long_a = "a".repeat(260);
    let emoji_stem: String = std::iter::repeat('🎉').take(260).collect();
    let cjk_stem: String = std::iter::repeat('中').take(256).collect();
    let long_no_ext = "x".repeat(300);
    let long_ext = "x".repeat(300);
    let multi_dot_stem = "a".repeat(260);

    // (input, expected_len, expected_suffix)
    let cases: Vec<(String, usize, &str)> = vec![
        // Short filename — unchanged
        ("report.pdf".into(), 10, "report.pdf"),
        // Exactly 255 chars — unchanged
        (format!("{}.pdf", "a".repeat(251)), 255, ".pdf"),
        // ASCII overflow — stem truncated, extension kept
        (format!("{long_a}.pdf"), 255, ".pdf"),
        // Emoji overflow — 4-byte chars, extension kept
        (format!("{emoji_stem}.pdf"), 255, ".pdf"),
        // CJK boundary — 3-byte chars, extension kept
        (format!("{cjk_stem}.txt"), 255, ".txt"),
        // No extension — plain truncation
        (long_no_ext, 255, ""),
        // Empty filename
        (String::new(), 0, ""),
        // Dotfile short — unchanged
        (".hidden".into(), 7, ".hidden"),
        // Multiple dots — last extension preserved
        (format!("{multi_dot_stem}.tar.gz"), 255, ".gz"),
        // Degenerate long extension — keeps trailing 255 chars
        (format!("a.{long_ext}"), 255, ""),
        // Long dotfile — treated as extensionless, plain truncation
        (format!(".{}", "x".repeat(300)), 255, ""),
        // Trailing dot — treated as extensionless, plain truncation
        (format!("{}.", "y".repeat(300)), 255, ""),
    ];

    for (i, (input, expected_len, suffix)) in cases.iter().enumerate() {
        let result = truncate_filename(input);
        assert_eq!(
            result.chars().count(),
            *expected_len,
            "case {i}: expected {expected_len} chars, got {} for input len {}",
            result.chars().count(),
            input.chars().count(),
        );
        if !suffix.is_empty() {
            assert!(
                result.ends_with(suffix),
                "case {i}: expected suffix {suffix:?}, got {result:?}",
            );
        }
        // Verify the result is always <= 255 chars
        assert!(
            result.chars().count() <= 255,
            "case {i}: result exceeds 255 chars",
        );
        // Verify stem has correct length when extension is preserved
        if !suffix.is_empty() && *expected_len == 255 {
            assert_eq!(
                result.chars().count(),
                255,
                "case {i}: truncated result should be exactly 255 chars",
            );
        }
    }

    // Extra: verify exact stem length for the emoji case
    let emoji_result = truncate_filename(&format!(
        "{}.pdf",
        std::iter::repeat('🎉').take(260).collect::<String>()
    ));
    let stem = &emoji_result[..emoji_result.rfind('.').unwrap()];
    assert_eq!(stem.chars().count(), 251, "emoji stem should be 251 chars");
}
