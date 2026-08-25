//! Canonical extension `<->` MIME type mapping.
//!
//! This is the single source of truth for the gateway
//! (`domain::service::FileParserService`) and every parser backend that
//! needs to derive one from the other. It replaces three separately
//! maintained tables that had already drifted (the gateway's own table, and
//! the `KreuzbergParser`/`ImageParser` copies).

/// `(extension, MIME type)` pairs. Extensions are lowercase, without the leading dot.
const EXTENSION_MIME_TABLE: &[(&str, &str)] = &[
    ("pdf", "application/pdf"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("txt", "text/plain"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    ("xls", "application/vnd.ms-excel"),
    ("xlsm", "application/vnd.ms-excel.sheet.macroEnabled.12"),
    (
        "xlsb",
        "application/vnd.ms-excel.sheet.binary.macroEnabled.12",
    ),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("webp", "image/webp"),
    ("gif", "image/gif"),
];

/// Look up the canonical MIME type for a (case-insensitive) extension.
#[must_use]
pub fn mime_for_extension(ext: &str) -> Option<&'static str> {
    EXTENSION_MIME_TABLE
        .iter()
        .find(|(e, _)| e.eq_ignore_ascii_case(ext))
        .map(|(_, mime)| *mime)
}

/// Look up the extension for a MIME essence string (e.g. from a parsed
/// `Content-Type` header). `application/xhtml+xml` is special-cased to
/// `html` since it isn't a canonical MIME type for any extension in the
/// table but should still route to the HTML parser.
#[must_use]
pub fn extension_for_mime(mime_essence: &str) -> Option<&'static str> {
    if mime_essence.eq_ignore_ascii_case("application/xhtml+xml") {
        return Some("html");
    }
    EXTENSION_MIME_TABLE
        .iter()
        .find(|(_, m)| m.eq_ignore_ascii_case(mime_essence))
        .map(|(ext, _)| *ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_for_extension_is_case_insensitive() {
        assert_eq!(mime_for_extension("PDF"), Some("application/pdf"));
        assert_eq!(mime_for_extension("pdf"), Some("application/pdf"));
    }

    #[test]
    fn mime_for_extension_unknown_returns_none() {
        assert_eq!(mime_for_extension("zip"), None);
    }

    #[test]
    fn extension_for_mime_round_trips_for_every_registered_extension() {
        for (ext, mime) in EXTENSION_MIME_TABLE {
            let round_tripped = extension_for_mime(mime).expect("mime should resolve");
            assert_eq!(
                mime_for_extension(round_tripped),
                Some(*mime),
                "round-tripping the MIME for extension {ext} should resolve to an \
                 extension with the same canonical MIME type"
            );
        }
    }

    #[test]
    fn extension_for_mime_resolves_aliases_to_their_canonical_extension() {
        // Unlike the round-trip test above, this pins which alias
        // (`jpg`/`jpeg`, `htm`/`html`) wins for each shared MIME type.
        for (mime, expected_extension) in [
            ("application/pdf", "pdf"),
            ("text/html", "html"),
            ("text/plain", "txt"),
            (
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                "docx",
            ),
            (
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                "xlsx",
            ),
            ("application/vnd.ms-excel", "xls"),
            ("application/vnd.ms-excel.sheet.macroEnabled.12", "xlsm"),
            (
                "application/vnd.ms-excel.sheet.binary.macroEnabled.12",
                "xlsb",
            ),
            (
                "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                "pptx",
            ),
            ("image/png", "png"),
            ("image/jpeg", "jpg"),
            ("image/webp", "webp"),
            ("image/gif", "gif"),
        ] {
            assert_eq!(
                extension_for_mime(mime),
                Some(expected_extension),
                "MIME {mime} should resolve to the canonical extension {expected_extension}"
            );
        }
    }

    #[test]
    fn extension_for_mime_txt() {
        assert_eq!(extension_for_mime("text/plain"), Some("txt"));
        assert_eq!(mime_for_extension("txt"), Some("text/plain"));
    }

    #[test]
    fn extension_for_mime_xhtml_special_case() {
        assert_eq!(extension_for_mime("application/xhtml+xml"), Some("html"));
    }

    #[test]
    fn extension_for_mime_unknown_returns_none() {
        assert_eq!(extension_for_mime("application/zip"), None);
    }
}
