//! Pure validators for the test-only microchat module.

use crate::error::MicrochatError;
use crate::service::MicrochatLimits;

/// MIME types accepted by the test microchat. Production code would
/// load this from configuration; here it is a fixed allowlist that
/// covers the cases exercised by the test suite.
pub const MIME_ALLOWLIST: &[&str] = &[
    "application/pdf",
    "image/png",
    "image/jpeg",
    "text/plain",
    "text/csv",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
];

/// Reject MIME types outside the configured allowlist. The comparison
/// is case-insensitive and operates on the bare type — any
/// `; charset=…`, `; boundary=…` etc. parameters are stripped first.
pub fn validate_mime(mime: &str, limits: &MicrochatLimits) -> Result<(), MicrochatError> {
    let bare = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if bare.is_empty() {
        return Err(MicrochatError::MimeNotAllowed(mime.to_string()));
    }
    let ok = limits
        .allowed_mimes
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(&bare));
    if ok {
        Ok(())
    } else {
        Err(MicrochatError::MimeNotAllowed(mime.to_string()))
    }
}

/// Reject filenames that:
/// - are empty,
/// - exceed `limits.max_filename_len` chars,
/// - contain `/` or `\` (path traversal),
/// - contain a `..` segment,
/// - start or end with whitespace,
/// - contain any control character.
pub fn validate_filename(name: &str, limits: &MicrochatLimits) -> Result<(), MicrochatError> {
    if name.is_empty() {
        return Err(MicrochatError::InvalidFilename("empty"));
    }
    if name.chars().count() > limits.max_filename_len {
        return Err(MicrochatError::InvalidFilename("too long"));
    }
    if name.contains('/') {
        return Err(MicrochatError::InvalidFilename("forward slash"));
    }
    if name.contains('\\') {
        return Err(MicrochatError::InvalidFilename("backslash"));
    }
    // `..` only as a complete path segment is impossible without a
    // separator, so a literal `..` substring is the safest check.
    if name.contains("..") {
        return Err(MicrochatError::InvalidFilename("parent segment"));
    }
    let first = name.chars().next().expect("non-empty");
    let last = name.chars().last().expect("non-empty");
    if first.is_whitespace() || last.is_whitespace() {
        return Err(MicrochatError::InvalidFilename("leading/trailing whitespace"));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(MicrochatError::InvalidFilename("control character"));
    }
    Ok(())
}
