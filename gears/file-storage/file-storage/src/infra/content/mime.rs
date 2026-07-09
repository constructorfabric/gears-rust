//! Content-type validation against the actual bytes (magic-byte / signature
//! sniffing), `cpt-cf-file-storage-fr-content-type-validation`.

use crate::domain::error::DomainError;

/// Detect the content type from the leading bytes. Returns `None` when the
/// content has no recognizable signature (e.g. plain text, CSV, custom binary).
#[must_use]
pub fn detect(bytes: &[u8]) -> Option<&'static str> {
    infer::get(bytes).map(|t| t.mime_type())
}

/// Validate the client-declared `declared` mime against the detected signature.
///
/// A declared type is rejected only when the bytes have a *recognizable* and
/// *different* signature. Unrecognized content (no magic bytes — text, CSV,
/// arbitrary binary) is accepted as declared, since absence of a signature is
/// not evidence of a mismatch.
pub fn validate(declared: &str, bytes: &[u8]) -> Result<(), DomainError> {
    match detect(bytes) {
        Some(detected) if !mime_equivalent(declared, detected) => {
            Err(DomainError::mime_mismatch(declared, detected))
        }
        _ => Ok(()),
    }
}

/// Compare two mime strings ignoring case and parameters (`; charset=...`).
fn mime_equivalent(a: &str, b: &str) -> bool {
    fn essence(s: &str) -> String {
        s.split(';').next().unwrap_or(s).trim().to_ascii_lowercase()
    }
    essence(a) == essence(b)
}

#[cfg(test)]
#[path = "mime_tests.rs"]
mod mime_tests;
