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
    /// A `complete` holds the lease and is assembling; after `lease_until` the next `complete` takes over.
    Completing,
    Completed,
    Aborted,
}

impl MultipartUploadState {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::Completing => "completing",
            Self::Completed => "completed",
            Self::Aborted => "aborted",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "in_progress" => Some(Self::InProgress),
            "completing" => Some(Self::Completing),
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
    pub auto_bind: bool,
    pub lease_until: Option<OffsetDateTime>,
    /// Persisted `StoredCompleteResult` JSON once `Completed`; a re-complete replays it.
    pub complete_result: Option<String>,
    pub backend_id: String,
    /// Backend object path; lets expired-session cleanup work after the `file_versions` row is gone.
    pub backend_path: String,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

/// Outcome of `complete_multipart_upload`: finished, or another caller holds the lease (`202`).
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone)]
pub enum MultipartCompleteOutcome {
    Completed(CompletedMultipartUpload),
    Completing { retry_after_secs: u64 },
}

/// Serializable snapshot of a successful complete response, persisted in the same transaction
/// that flips the session to `completed`; carries no `manifest` (re-read from `version_hash_manifest`).
#[domain_model]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredCompleteResult {
    pub version_id: Uuid,
    pub size: i64,
    pub content_hash: String,
    pub hash_mode: String,
    pub part_count: i32,
    pub bind_state: String,
    pub etag: Option<String>,
    pub current_etag: Option<String>,
}

impl StoredCompleteResult {
    #[must_use]
    pub fn from_completed(c: &CompletedMultipartUpload) -> Self {
        Self {
            version_id: c.version_id,
            size: c.size,
            content_hash: hex::encode(&c.content_hash),
            hash_mode: c.hash_mode.as_str().to_owned(),
            part_count: c.part_count,
            bind_state: c.bind_state.as_str().to_owned(),
            etag: c.etag.clone(),
            current_etag: c.current_etag.clone(),
        }
    }

    /// Rebuild the response (`manifest` supplied by the caller). `None` for an unparsable or
    /// inconsistent snapshot; the caller rebuilds from the version row.
    #[must_use]
    pub fn into_completed(self, manifest: Option<String>) -> Option<CompletedMultipartUpload> {
        let hash_mode = HashMode::parse(&self.hash_mode)?;
        let bind_state = match self.bind_state.as_str() {
            "bound" => BindState::Bound,
            "conflict" => BindState::Conflict,
            "manual" => BindState::Manual,
            _ => return None,
        };
        // An inconsistent snapshot is untrustworthy, so `None`. One part is `whole-sha256` with no
        // manifest (ADR-0006); 2+ parts are `multipart-composite-sha256` with one; anything else is corruption.
        match hash_mode {
            HashMode::WholeSha256 if self.part_count != 1 || manifest.is_some() => return None,
            HashMode::MultipartCompositeSha256 if self.part_count < 2 || manifest.is_none() => {
                return None;
            }
            _ => {}
        }
        let consistent = match bind_state {
            BindState::Bound => self.etag.is_some() && self.current_etag.is_none(),
            BindState::Conflict => self.etag.is_none() && self.current_etag.is_some(),
            BindState::Manual => self.etag.is_none() && self.current_etag.is_none(),
        };
        if !consistent {
            return None;
        }
        let content_hash = hex::decode(&self.content_hash)
            .ok()
            .filter(|d| d.len() == 32)?;
        Some(CompletedMultipartUpload {
            version_id: self.version_id,
            size: self.size,
            hash_algorithm: crate::infra::content::hash::ALGORITHM,
            content_hash,
            hash_mode,
            part_count: self.part_count,
            manifest,
            bind_state,
            etag: self.etag,
            current_etag: self.current_etag,
        })
    }
}

/// Result of a successful `complete_multipart_upload`.
///
/// `manifest` lets a client re-verify the composite hash without a second round-trip
/// (~90 bytes per part, ~1 MiB at the [`MAX_PART_COUNT`] ceiling).
#[domain_model]
#[derive(Debug, Clone)]
pub struct CompletedMultipartUpload {
    pub version_id: Uuid,
    pub size: i64,
    /// Always `"SHA-256"`.
    pub hash_algorithm: &'static str,
    pub content_hash: Vec<u8>,
    pub hash_mode: HashMode,
    pub part_count: i32,
    pub manifest: Option<String>,
    pub bind_state: BindState,
    pub etag: Option<String>,
    /// Current content `ETag` when the bind CAS was lost (`Conflict` only); the `If-Match` for a manual rebind.
    pub current_etag: Option<String>,
}

/// Bind outcome shared by multipart `complete` (`bind_state`) and single-part `PUT` (`X-FS-Bound`).
/// `Conflict` means the auto-bind CAS lost but the upload still SUCCEEDED; a manual `bind` makes it live.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindState {
    Bound,
    Conflict,
    Manual,
}

impl BindState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bound => "bound",
            Self::Conflict => "conflict",
            Self::Manual => "manual",
        }
    }
}

/// How a bind attempt came out, as the caller decided it; [`BindDecision::outcome`] is the one
/// mapping to the reported `(bind_state, etag, current_etag)` for first calls and replays alike.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindDecision {
    /// This version is the file's content: `Bound` with this version's `ETag`.
    Bound,
    /// Auto-bind lost the CAS: `Conflict` with the `ETag` of the pointer that won.
    Conflict { current_etag: Option<String> },
    /// The upload never asked to bind: no bind outcome.
    Manual,
}

impl BindDecision {
    #[must_use]
    pub fn outcome(
        self,
        file_id: Uuid,
        version_id: Uuid,
    ) -> (BindState, Option<String>, Option<String>) {
        match self {
            Self::Bound => (
                BindState::Bound,
                Some(crate::domain::etag::content_etag(file_id, version_id)),
                None,
            ),
            Self::Conflict { current_etag } => (BindState::Conflict, None, current_etag),
            Self::Manual => (BindState::Manual, None, None),
        }
    }
}

/// Bind outcome a replay reports (a single-part finalize retry or a multipart complete rebuilt
/// from the version row): a bind won at finalize replays as `Bound` from the persisted
/// `bound_on_finalize` flag (a retry must not turn `Bound` into `Conflict`), a manual upload
/// never reports a bind outcome, and an auto-bind upload that lost the CAS reads the live pointer
/// (`Bound` if the file now points at this version, else `Conflict` with the current `ETag`).
#[must_use]
pub fn replay_bind_state(
    file_id: Uuid,
    content_id: Option<Uuid>,
    version_id: Uuid,
    bound_on_finalize: bool,
    auto_bind: bool,
) -> (BindState, Option<String>, Option<String>) {
    let decision = if bound_on_finalize {
        BindDecision::Bound
    } else if !auto_bind {
        BindDecision::Manual
    } else if content_id == Some(version_id) {
        BindDecision::Bound
    } else {
        BindDecision::Conflict {
            current_etag: content_id.map(|cid| crate::domain::etag::content_etag(file_id, cid)),
        }
    };
    decision.outcome(file_id, version_id)
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

/// Hard ceiling on the parts of one plan, enforced by `compute_plan` (ADR-0006: unbounded
/// manifests must not be possible). `10_000` matches S3's limit.
pub const MAX_PART_COUNT: u64 = 10_000;

/// One raw part entry from `compute_plan`: `(part_number, offset, size)`.
pub type RawPartEntry = (u32, u64, u64);

/// Compute the server-chosen `part_size` and the plan skeleton (URLs are injected by
/// `MultipartService`). Returns `(part_size, parts)`.
///
/// `part_size = max(preferred, backend_min)` rounded up to a multiple of the minimum;
/// `parts = ceil(declared_size / part_size)`, widening `part_size` (up to [`MAX_PART_SIZE`]) to
/// stay within [`MAX_PART_COUNT`]. Runs before allocating the plan, so an attacker-controlled
/// `declared_size` cannot drive allocation.
///
/// # Errors
/// Returns [`DomainError::Validation`] on `u64` overflow or if even [`MAX_PART_SIZE`] cannot fit
/// `declared_size` within [`MAX_PART_COUNT`] parts.
pub fn compute_plan(
    declared_size: u64,
    preferred_part_size: Option<u64>,
    backend_min_part_size: Option<u64>,
) -> Result<(u64, Vec<RawPartEntry>), DomainError> {
    let min = backend_min_part_size.unwrap_or(DEFAULT_MIN_PART_SIZE);
    let preferred = preferred_part_size.unwrap_or(min);
    let raw = preferred.max(min);
    let mut part_size = round_up_to(raw, min).ok_or_else(|| {
        DomainError::validation(
            "preferred_part_size",
            format!("part-size computation overflowed for preferred={preferred}, min={min}"),
        )
    })?;

    if declared_size == 0 {
        return Ok((part_size, vec![(1, 0, 0)]));
    }

    if declared_size.div_ceil(part_size) > MAX_PART_COUNT {
        let minimal_required = declared_size.div_ceil(MAX_PART_COUNT);
        if minimal_required > MAX_PART_SIZE {
            return Err(DomainError::validation(
                "declared_size",
                format!(
                    "declared_size {declared_size} bytes is too large for multipart upload on \
                     this backend: even at the maximum part size of {MAX_PART_SIZE} bytes it \
                     would require more than {MAX_PART_COUNT} parts"
                ),
            ));
        }
        part_size = minimal_required.max(part_size).min(MAX_PART_SIZE);
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
