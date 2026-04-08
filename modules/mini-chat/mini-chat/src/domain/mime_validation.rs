use std::fmt;

use modkit_macros::domain_model;

use crate::domain::error::DomainError;

// ── MIME type constants ──────────────────────────────────────────────────

// Document types (17)
pub const MIME_PDF: &str = "application/pdf";
pub const MIME_PLAIN: &str = "text/plain";
pub const MIME_MARKDOWN: &str = "text/markdown";
pub const MIME_HTML: &str = "text/html";
pub const MIME_DOCX: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
pub const MIME_PPTX: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation";
pub const MIME_XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
pub const MIME_JSON: &str = "application/json";
pub const MIME_PYTHON: &str = "text/x-python";
pub const MIME_JAVA: &str = "text/x-java";
pub const MIME_JAVASCRIPT: &str = "text/javascript";
pub const MIME_TYPESCRIPT: &str = "text/typescript";
pub const MIME_RUST: &str = "text/x-rust";
pub const MIME_GO: &str = "text/x-go";
pub const MIME_CSHARP: &str = "text/x-csharp";
pub const MIME_RUBY: &str = "text/x-ruby";
pub const MIME_SQL: &str = "text/x-sql";

// Image types (4)
pub const MIME_PNG: &str = "image/png";
pub const MIME_JPEG: &str = "image/jpeg";
pub const MIME_WEBP: &str = "image/webp";
pub const MIME_GIF: &str = "image/gif";

// Special types (not in the allowlist but used for inference/remapping)
pub(crate) const MIME_CSV: &str = "text/csv";
pub(crate) const MIME_OCTET_STREAM: &str = "application/octet-stream";

// ── Lookup table ─────────────────────────────────────────────────────────

/// One entry in the MIME allowlist: canonical type, attachment kind, and file
/// extension. Drives `validate_mime` and `mime_to_extension` from a single
/// source of truth.
#[domain_model]
struct MimeSpec {
    mime: &'static str,
    kind: AttachmentKind,
    ext: &'static str,
}

const ACCEPTED_MIMES: &[MimeSpec] = &[
    // Document types (17)
    MimeSpec {
        mime: MIME_PDF,
        kind: AttachmentKind::Document,
        ext: "pdf",
    },
    MimeSpec {
        mime: MIME_PLAIN,
        kind: AttachmentKind::Document,
        ext: "txt",
    },
    MimeSpec {
        mime: MIME_MARKDOWN,
        kind: AttachmentKind::Document,
        ext: "md",
    },
    MimeSpec {
        mime: MIME_HTML,
        kind: AttachmentKind::Document,
        ext: "html",
    },
    MimeSpec {
        mime: MIME_DOCX,
        kind: AttachmentKind::Document,
        ext: "docx",
    },
    MimeSpec {
        mime: MIME_PPTX,
        kind: AttachmentKind::Document,
        ext: "pptx",
    },
    MimeSpec {
        mime: MIME_XLSX,
        kind: AttachmentKind::Document,
        ext: "xlsx",
    },
    MimeSpec {
        mime: MIME_JSON,
        kind: AttachmentKind::Document,
        ext: "json",
    },
    MimeSpec {
        mime: MIME_PYTHON,
        kind: AttachmentKind::Document,
        ext: "py",
    },
    MimeSpec {
        mime: MIME_JAVA,
        kind: AttachmentKind::Document,
        ext: "java",
    },
    MimeSpec {
        mime: MIME_JAVASCRIPT,
        kind: AttachmentKind::Document,
        ext: "js",
    },
    MimeSpec {
        mime: MIME_TYPESCRIPT,
        kind: AttachmentKind::Document,
        ext: "ts",
    },
    MimeSpec {
        mime: MIME_RUST,
        kind: AttachmentKind::Document,
        ext: "rs",
    },
    MimeSpec {
        mime: MIME_GO,
        kind: AttachmentKind::Document,
        ext: "go",
    },
    MimeSpec {
        mime: MIME_CSHARP,
        kind: AttachmentKind::Document,
        ext: "cs",
    },
    MimeSpec {
        mime: MIME_RUBY,
        kind: AttachmentKind::Document,
        ext: "rb",
    },
    MimeSpec {
        mime: MIME_SQL,
        kind: AttachmentKind::Document,
        ext: "sql",
    },
    // Image types (4)
    MimeSpec {
        mime: MIME_PNG,
        kind: AttachmentKind::Image,
        ext: "png",
    },
    MimeSpec {
        mime: MIME_JPEG,
        kind: AttachmentKind::Image,
        ext: "jpg",
    },
    MimeSpec {
        mime: MIME_WEBP,
        kind: AttachmentKind::Image,
        ext: "webp",
    },
    MimeSpec {
        mime: MIME_GIF,
        kind: AttachmentKind::Image,
        ext: "gif",
    },
];

// ── Domain types ─────────────────────────────────────────────────────────

/// Classification of attachment content (domain-layer enum, no ORM deps).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    Document,
    Image,
}

/// Determines how an uploaded file will be used in the LLM request pipeline.
///
/// Variants are ordered alphabetically by their string representation
/// (`code_interpreter` < `file_search`) so that derived `Ord` produces
/// a canonical sort order for DB storage.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AttachmentPurpose {
    /// Passed directly to the `code_interpreter` tool.
    CodeInterpreter,
    /// Indexed in a vector store for `file_search` tool.
    FileSearch,
}

impl fmt::Display for AttachmentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Document => write!(f, "document"),
            Self::Image => write!(f, "image"),
        }
    }
}

/// Validated MIME result: the canonical MIME type string and the attachment kind.
#[domain_model]
pub struct ValidatedMime {
    pub mime: &'static str,
    pub kind: AttachmentKind,
}

// ── Public API ───────────────────────────────────────────────────────────

/// Strip charset and other parameters: `text/plain; charset=utf-8` → `text/plain`.
pub(crate) fn normalize_mime(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase()
}

/// MIME allowlist: 21 types (19 from spec + image/gif per spec:64 + XLSX for `code_interpreter`).
///
/// Strips charset parameters (e.g., `text/plain; charset=utf-8` → `text/plain`).
/// Rejects `application/octet-stream` and any unlisted types.
///
/// Returns the canonical MIME string and the attachment kind (Document or Image).
pub fn validate_mime(content_type: &str) -> Result<ValidatedMime, DomainError> {
    let mime = normalize_mime(content_type);
    ACCEPTED_MIMES
        .iter()
        .find(|spec| spec.mime == mime)
        .map(|spec| ValidatedMime {
            mime: spec.mime,
            kind: spec.kind,
        })
        .ok_or(DomainError::UnsupportedFileType { mime })
}

/// Map a MIME type to its intended usage(s) in the LLM pipeline.
///
/// Called after MIME validation to keep validation and routing separate.
/// Returns a `Vec` because a single attachment may serve multiple purposes
/// (e.g., XLSX in both `FileSearch` and `CodeInterpreter` in the future).
#[must_use]
pub fn resolve_purposes(mime: &str) -> Vec<AttachmentPurpose> {
    match ACCEPTED_MIMES
        .iter()
        .find(|spec| spec.mime == mime)
        .map(|spec| spec.kind)
    {
        Some(AttachmentKind::Document) if mime == MIME_XLSX => {
            vec![AttachmentPurpose::CodeInterpreter]
        }
        Some(AttachmentKind::Document) => vec![AttachmentPurpose::FileSearch],
        Some(AttachmentKind::Image) | None => vec![],
    }
}

/// Infer MIME type from filename extension when the client sends an unhelpful
/// Content-Type (e.g. `application/octet-stream`). Returns `None` if the
/// extension is unknown — the caller should keep the original Content-Type.
#[must_use]
pub fn infer_mime_from_extension(filename: &str) -> Option<&'static str> {
    let (_, ext_raw) = filename.rsplit_once('.')?;
    let ext = ext_raw.to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => Some(MIME_PDF),
        "txt" => Some(MIME_PLAIN),
        "md" | "markdown" => Some(MIME_MARKDOWN),
        "html" | "htm" => Some(MIME_HTML),
        "json" => Some(MIME_JSON),
        "docx" => Some(MIME_DOCX),
        "pptx" => Some(MIME_PPTX),
        "xlsx" => Some(MIME_XLSX),
        "py" => Some(MIME_PYTHON),
        "java" => Some(MIME_JAVA),
        "js" | "mjs" => Some(MIME_JAVASCRIPT),
        "ts" | "mts" => Some(MIME_TYPESCRIPT),
        "rs" => Some(MIME_RUST),
        "go" => Some(MIME_GO),
        "cs" => Some(MIME_CSHARP),
        "rb" => Some(MIME_RUBY),
        "sql" => Some(MIME_SQL),
        "csv" => Some(MIME_CSV),
        "png" => Some(MIME_PNG),
        "jpg" | "jpeg" => Some(MIME_JPEG),
        "webp" => Some(MIME_WEBP),
        "gif" => Some(MIME_GIF),
        _ => None,
    }
}

/// Remap `text/csv` to `text/plain` so it passes [`validate_mime`] and is indexed
/// as plain text by the provider. Returns `None` for non-CSV content types.
#[must_use]
pub fn remap_csv_to_plain(content_type: &str) -> Option<&'static str> {
    if normalize_mime(content_type) == MIME_CSV {
        Some(MIME_PLAIN)
    } else {
        None
    }
}

/// Build a structured filename for provider upload: `{chat_id}_{attachment_id}.{ext}`.
///
/// The extension is derived from the validated MIME type. All accepted MIME
/// types have a known extension — unsupported types are rejected before
/// reaching this point.
#[must_use]
pub fn structured_filename(chat_id: uuid::Uuid, attachment_id: uuid::Uuid, mime: &str) -> String {
    let ext = mime_to_extension(mime);
    format!("{chat_id}_{attachment_id}.{ext}")
}

fn mime_to_extension(mime: &str) -> &'static str {
    ACCEPTED_MIMES
        .iter()
        .find(|spec| spec.mime == mime)
        .map_or("bin", |spec| spec.ext)
}

#[cfg(test)]
mod tests {
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
}
