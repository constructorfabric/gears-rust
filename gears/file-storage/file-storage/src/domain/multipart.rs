//! Domain types for multipart upload sessions and parts.

use time::OffsetDateTime;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::infra::content::hash_mode::HashMode;

/// State of a multipart upload session.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MultipartUploadState {
    InProgress,
    Completed,
    Aborted,
}

impl MultipartUploadState {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Aborted => "aborted",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "in_progress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            "aborted" => Some(Self::Aborted),
            _ => None,
        }
    }
}

/// An in-flight multipart upload session.
#[domain_model]
#[derive(Debug, Clone)]
pub struct MultipartUploadSession {
    pub upload_id: Uuid,
    pub file_id: Uuid,
    pub version_id: Uuid,
    pub backend_upload_handle: String,
    pub state: MultipartUploadState,
    pub declared_mime: String,
    pub mime_validated: bool,
    /// Total file size declared at initiate time (bytes).
    pub declared_size: u64,
    /// Server-chosen plan unit (bytes, uniform except the final part).
    pub part_size: u64,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

/// Result of a successful `complete_multipart_upload`.
///
/// `manifest` lets a client re-verify the composite hash without a second round-trip
/// (~90 bytes per part, ~1 MiB at the 10k-part ceiling).
#[domain_model]
#[derive(Debug, Clone)]
pub struct CompletedMultipartUpload {
    pub version_id: Uuid,
    pub size: i64,
    /// Always `"SHA-256"`.
    pub hash_algorithm: &'static str,
    /// Composite root: `sha256(manifest)`.
    pub content_hash: Vec<u8>,
    /// Always [`HashMode::MultipartCompositeSha256`] for this completion path.
    pub hash_mode: HashMode,
    pub part_count: i32,
    /// Wire-format manifest text (`Manifest::to_wire_string`).
    pub manifest: String,
}

/// Result of `GET /files/{id}/multipart/{upload_id}`: session state plus received and
/// missing parts.
///
/// `upload_url` on each [`MissingPart`] is populated only while the session is
/// `in_progress` and unexpired; terminal or expired sessions get no resume URLs.
#[domain_model]
#[derive(Debug, Clone)]
pub struct MultipartUploadStatus {
    pub upload_id: Uuid,
    pub version_id: Uuid,
    pub state: MultipartUploadState,
    pub declared_mime: String,
    pub declared_size: u64,
    pub part_size: u64,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
    /// Parts already reported by the sidecar, in ascending `part_number` order.
    pub received: Vec<ReceivedPart>,
    /// Parts not yet reported, in ascending `part_number` order.
    pub missing: Vec<MissingPart>,
}

/// One already-uploaded part, as reported by the sidecar.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ReceivedPart {
    pub part_number: u32,
    pub size: i64,
    pub uploaded_at: OffsetDateTime,
}

/// One part not yet uploaded: planned bounds recomputed from the session's
/// `(declared_size, part_size)` and, when resumable, a fresh signed upload URL.
#[domain_model]
#[derive(Debug, Clone)]
pub struct MissingPart {
    pub part_number: u32,
    pub offset: u64,
    pub size: u64,
    /// `Some` only for a live `in_progress` session; token expiry is capped at the
    /// session's `expires_at`, so a resume URL never outlives its session.
    pub upload_url: Option<String>,
}

/// One uploaded part of a multipart session.
#[domain_model]
#[derive(Debug, Clone)]
pub struct MultipartPart {
    pub upload_id: Uuid,
    pub part_number: u32,
    pub backend_etag: String,
    pub part_hash: Vec<u8>,
    pub size: i64,
    pub uploaded_at: OffsetDateTime,
}

/// One planned part returned in the initiate response. The client must `PUT` exactly
/// `size` bytes to the signed `upload_url` (which carries the `size` claim).
#[domain_model]
#[derive(Debug, Clone)]
pub struct MultipartPartPlan {
    /// 1-based part number (S3 convention).
    pub part_number: u32,
    /// Byte offset of this part within the final assembled object.
    pub offset: u64,
    /// Exact byte length of this part.
    pub size: u64,
    /// Sidecar signed URL the client `PUT`s this part's bytes to.
    pub upload_url: String,
}

/// The server-authoritative parts plan returned by `POST /files/{id}/multipart`.
#[domain_model]
#[derive(Debug, Clone)]
pub struct MultipartPlan {
    pub upload_id: Uuid,
    pub version_id: Uuid,
    /// The hash algorithm used for per-part hashes (`"SHA-256"`).
    pub part_hash_algorithm: String,
    /// Uniform part size (bytes); the final part may be smaller.
    pub part_size: u64,
    /// One entry per part, in ascending `part_number` order.
    pub parts: Vec<MultipartPartPlan>,
    /// Token expiry; all per-part URLs share this expiry.
    pub expires_at: OffsetDateTime,
}

/// Minimum part size when the backend declares none: the S3 minimum for all parts but the
/// last. Also the lower bound for a client-supplied `preferred_part_size`.
pub const DEFAULT_MIN_PART_SIZE: u64 = 5 * 1024 * 1024;

/// Maximum accepted `preferred_part_size` hint: S3's absolute maximum part size. Larger
/// values are rejected at the service boundary; `compute_plan` still uses checked
/// arithmetic as defense-in-depth.
pub const MAX_PART_SIZE: u64 = 5 * 1024 * 1024 * 1024;

/// One raw part entry from `compute_plan`: `(part_number, offset, size)`.
pub type RawPartEntry = (u32, u64, u64);

/// Compute the server-chosen `part_size` and the plan skeleton (URLs are injected by
/// `MultipartService`). Returns `(part_size, parts)`.
///
/// `part_size = max(preferred, backend_min)` rounded up to a multiple of the minimum;
/// `parts = ceil(declared_size / part_size)`; the last part holds the remainder.
///
/// # Errors
/// Returns [`DomainError::Validation`] if the part-size arithmetic overflows `u64`
/// (defense-in-depth; callers validate `preferred_part_size` first).
pub fn compute_plan(
    declared_size: u64,
    preferred_part_size: Option<u64>,
    backend_min_part_size: Option<u64>,
) -> Result<(u64, Vec<RawPartEntry>), DomainError> {
    let min = backend_min_part_size.unwrap_or(DEFAULT_MIN_PART_SIZE);
    let preferred = preferred_part_size.unwrap_or(min);
    let raw = preferred.max(min);
    let part_size = round_up_to(raw, min).ok_or_else(|| {
        DomainError::validation(
            "preferred_part_size",
            format!("part-size computation overflowed for preferred={preferred}, min={min}"),
        )
    })?;

    if declared_size == 0 {
        return Ok((part_size, vec![(1, 0, 0)]));
    }

    let n_parts = declared_size.div_ceil(part_size);
    let capacity = usize::try_from(n_parts).unwrap_or(usize::MAX);
    let mut parts = Vec::with_capacity(capacity);
    for i in 0..n_parts {
        let offset = i.checked_mul(part_size).ok_or_else(|| {
            DomainError::validation(
                "preferred_part_size",
                format!("part offset overflowed at part {}", i + 1),
            )
        })?;
        let size = if i + 1 == n_parts {
            declared_size - offset
        } else {
            part_size
        };
        let part_number = u32::try_from(i + 1).unwrap_or(u32::MAX);
        parts.push((part_number, offset, size));
    }
    Ok((part_size, parts))
}

/// Round `value` up to the next multiple of `align`; `None` on overflow.
fn round_up_to(value: u64, align: u64) -> Option<u64> {
    if align == 0 {
        return Some(value);
    }
    value.div_ceil(align).checked_mul(align)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_up_to_does_not_overflow_on_max_input() {
        assert_eq!(round_up_to(u64::MAX, DEFAULT_MIN_PART_SIZE), None);
        assert_eq!(round_up_to(u64::MAX, u64::MAX), Some(u64::MAX));
        assert_eq!(round_up_to(1, u64::MAX), Some(u64::MAX));
        assert_eq!(round_up_to(7, 5), Some(10));
        assert_eq!(round_up_to(10, 5), Some(10));
    }

    #[test]
    fn compute_plan_returns_validation_error_on_overflowing_preferred_part_size() {
        let err = compute_plan(u64::MAX, Some(u64::MAX), None).unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }
}
