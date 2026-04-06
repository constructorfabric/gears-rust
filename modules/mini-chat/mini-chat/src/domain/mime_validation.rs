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

/// Maximum filename length in characters to match the `VARCHAR(255)` DB column.
const MAX_FILENAME_CHARS: usize = 255;

/// Truncate a filename to at most 255 **characters** (not bytes), preserving the
/// file extension so MIME inference and LLM context remain intact.
///
/// If the filename has an extension (determined via `rsplit_once('.')`), the stem
/// is shortened to make room for `.{ext}` within the 255-char budget.
#[must_use]
pub fn truncate_filename(filename: &str) -> String {
    let char_count = filename.chars().count();
    if char_count <= MAX_FILENAME_CHARS {
        return filename.to_owned();
    }

    let Some((stem, ext)) = filename
        .rsplit_once('.')
        .filter(|(stem, ext)| !stem.is_empty() && !ext.is_empty())
    else {
        // No meaningful extension (no dot, dotfile like ".bashrc", or trailing
        // dot like "file.") — truncate the whole string to 255 characters.
        return filename.chars().take(MAX_FILENAME_CHARS).collect();
    };

    let ext_chars = ext.chars().count();
    let dot_plus_ext = 1 + ext_chars;

    if dot_plus_ext >= MAX_FILENAME_CHARS {
        // Extension is so long there is no room for the stem — fall back to
        // keeping the last 255 characters of the original filename as-is.
        return filename
            .char_indices()
            .rev()
            .nth(MAX_FILENAME_CHARS - 1)
            .map_or_else(|| filename.to_owned(), |(i, _)| filename[i..].to_owned());
    }

    let max_stem_chars = MAX_FILENAME_CHARS - dot_plus_ext;
    let truncated_stem: String = stem.chars().take(max_stem_chars).collect();
    format!("{truncated_stem}.{ext}")
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
#[path = "mime_validation_tests.rs"]
mod tests;
