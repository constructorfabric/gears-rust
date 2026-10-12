//! Content-type validation against the actual bytes (magic-byte sniffing).

use crate::domain::error::DomainError;
use crate::domain::policy::{EffectivePolicy, PolicyResolver};

/// Detect the content type from the leading bytes; `None` if there is no recognizable
/// signature (plain text, CSV, custom binary).
#[must_use]
pub fn detect(bytes: &[u8]) -> Option<&'static str> {
    infer::get(bytes).map(|t| t.mime_type())
}

/// Validate the declared mime against the detected signature. Rejected only when the bytes
/// have a recognizable and different signature; unrecognized content is accepted as declared.
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

/// How many leading bytes of the stored blob are read for MIME sniffing. The deepest `infer`
/// matcher (a legacy RAR signature) looks at offset 261, so 8 KiB never changes a sniff result.
/// Shared by the single-part and multipart finalize paths.
pub(crate) const MIME_SNIFF_PREFIX_BYTES: usize = 8 * 1024;

/// Validate the stored blob's bytes against the declared MIME type and return the type to
/// persist: the sniffed type if recognizable, otherwise `declared_mime`. An empty
/// `declared_mime` is passed through untouched (nothing to validate against).
pub(crate) fn validate_and_resolve_mime(
    declared_mime: &str,
    blob: &[u8],
) -> Result<String, DomainError> {
    if declared_mime.is_empty() {
        return Ok(declared_mime.to_owned());
    }
    validate(declared_mime, blob)?;
    Ok(detect(blob).map_or_else(|| declared_mime.to_owned(), str::to_owned))
}

/// Re-enforce the per-MIME size ceiling against the validated type, so a generous declared
/// type cannot smuggle in bytes of a more tightly restricted true type. A no-op when
/// `validated_mime` equals `declared_mime`.
pub(crate) fn enforce_size_ceiling_for_validated_mime(
    policy: &EffectivePolicy,
    declared_mime: &str,
    validated_mime: &str,
    backend_max_bytes: Option<u64>,
    actual_size: i64,
) -> Result<(), DomainError> {
    if validated_mime == declared_mime {
        return Ok(());
    }
    let effective_max =
        PolicyResolver::compute_effective_max_bytes(policy, validated_mime, backend_max_bytes);
    if let Some(limit) = effective_max
        && actual_size > 0
        && actual_size.cast_unsigned() > limit
    {
        return Err(DomainError::policy_size_exceeded(
            limit,
            "policy size limit (true content type)",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "mime_tests.rs"]
mod mime_tests;
