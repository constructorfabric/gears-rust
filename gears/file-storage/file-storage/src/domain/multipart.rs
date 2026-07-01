//! Domain types for multipart upload sessions and parts.
//!
//! @cpt-cf-file-storage-fr-multipart-upload

use time::OffsetDateTime;
use toolkit_macros::domain_model;
use uuid::Uuid;

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
///
/// `declared_size` and `part_size` were added by the
/// `multipart-coordinator` server-authoritative feature (§6).
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

// ── Server-authoritative parts plan (multipart-coordinator feature) ────────────

/// One planned part as returned to the client in the initiate response.
///
/// The `upload_url` is a sidecar signed URL containing the exact `size` claim.
/// The client must `PUT` exactly `size` bytes to `upload_url`.
///
/// @cpt-cf-file-storage-fr-multipart-upload
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
///
/// @cpt-cf-file-storage-fr-multipart-upload
#[domain_model]
#[derive(Debug, Clone)]
pub struct MultipartPlan {
    pub upload_id: Uuid,
    pub version_id: Uuid,
    /// The hash algorithm used for per-part hashes (`"SHA-256"` in P2).
    pub part_hash_algorithm: String,
    /// Uniform part size (bytes); the final part may be smaller.
    pub part_size: u64,
    /// One entry per part, in ascending `part_number` order.
    pub parts: Vec<MultipartPartPlan>,
    /// Token expiry; all per-part URLs share this expiry.
    pub expires_at: OffsetDateTime,
}

/// Minimum part size used when the backend does not declare a minimum.
///
/// 5 MiB is the S3 minimum for all parts except the last.
pub const DEFAULT_MIN_PART_SIZE: u64 = 5 * 1024 * 1024;

/// Compute the server-chosen `part_size` and generate the plan skeleton
/// (without URLs — those are injected by `MultipartService`).
///
/// Rules (FEATURE §3):
/// - `part_size = max(preferred, backend_min)` rounded up to the nearest
///   multiple of `DEFAULT_MIN_PART_SIZE` (BLAKE3-friendly alignment deferred,
///   SHA-256 is used in P2).
/// - `parts = ceil(declared_size / part_size)`.
/// - The last part's `size` is `declared_size - (parts - 1) * part_size`.
///
/// One raw part entry from `compute_plan`: `(part_number, offset, size)`.
pub type RawPartEntry = (u32, u64, u64);

/// Returns `(part_size, parts_count)` ready to be used by the caller.
#[must_use]
pub fn compute_plan(
    declared_size: u64,
    preferred_part_size: Option<u64>,
    backend_min_part_size: Option<u64>,
) -> (u64, Vec<RawPartEntry>) {
    let min = backend_min_part_size.unwrap_or(DEFAULT_MIN_PART_SIZE);
    let preferred = preferred_part_size.unwrap_or(min);
    // Part size = max(preferred, backend_min), rounded up to the nearest `min`.
    let raw = preferred.max(min);
    let part_size = round_up_to(raw, min);

    if declared_size == 0 {
        return (part_size, vec![(1, 0, 0)]);
    }

    let n_parts = declared_size.div_ceil(part_size);
    let capacity = usize::try_from(n_parts).unwrap_or(usize::MAX);
    let mut parts = Vec::with_capacity(capacity);
    for i in 0..n_parts {
        let offset = i * part_size;
        let size = if i + 1 == n_parts {
            declared_size - offset
        } else {
            part_size
        };
        let part_number = u32::try_from(i + 1).unwrap_or(u32::MAX);
        parts.push((part_number, offset, size));
    }
    (part_size, parts)
}

/// Round `value` up to the next multiple of `align` (≥ 1).
fn round_up_to(value: u64, align: u64) -> u64 {
    if align == 0 {
        return value;
    }
    value.div_ceil(align) * align
}
