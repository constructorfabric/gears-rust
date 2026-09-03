//! Content-type validation against the actual bytes (magic-byte / signature
//! sniffing), `cpt-cf-file-storage-fr-content-type-validation`.

use crate::domain::error::DomainError;
use crate::domain::policy::{EffectivePolicy, PolicyResolver};

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

/// Cap on how many leading bytes of the read-back blob are captured for MIME
/// sniffing (`cpt-cf-file-storage-fr-content-type-validation`). The vendored
/// `infer` crate's deepest matcher (a legacy RAR-archive signature) inspects
/// byte offset 261; every other matcher looks at far fewer bytes. 8 KiB is
/// comfortably more than any matcher needs, so truncating the read-back to
/// this prefix can never change a sniff result.
///
/// Shared by both finalize paths that sniff a read-back prefix: the
/// single-part `finalize_upload`/`finalize_upload_by_token`
/// (`src/domain/service/write.rs`) and the multipart-complete path
/// (`src/domain/multipart_service.rs`, P2 remediation item 1.10).
pub(crate) const MIME_SNIFF_PREFIX_BYTES: usize = 8 * 1024;

/// Validate the read-back blob's actual bytes against the version's declared
/// MIME type, reusing [`validate`]'s magic-byte sniffing (the same logic the
/// in-process data plane runs at ingress) rather than re-implementing it.
///
/// Returns the MIME type that should be persisted: the sniffed/canonical type
/// when the bytes carry a recognizable signature, otherwise `declared_mime`
/// unchanged (unrecognized content — e.g. plain text/CSV/custom binary — is
/// not evidence of a mismatch, so it is accepted as declared).
///
/// A version with no declared MIME type (`declared_mime` empty) is passed
/// through untouched: there is nothing to validate against, and unrestricted
/// uploads must keep working exactly as before.
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

/// Re-enforce the per-MIME size ceiling against the **validated** type. The
/// declared-type check runs earlier (before the blob is even read back); this
/// second check closes the gap where a declared type with a generous — or
/// unrestricted — ceiling would otherwise let bytes of a more tightly
/// restricted true type slip through under it.
///
/// A no-op when `validated_mime` is the same string the earlier check already
/// used (nothing new to enforce).
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
