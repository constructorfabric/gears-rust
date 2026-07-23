//! Domain types for multipart upload sessions and parts.
//!
//! @cpt-cf-file-storage-fr-multipart-upload

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
    /// A `complete` call holds the completion lease and is assembling on the
    /// backend (upload-flow redesign). Entered/left by single conditional
    /// UPDATEs — no DB transaction is ever held across the backend I/O. A
    /// crashed completer leaves the state here until `lease_until` passes,
    /// after which the next `complete` takes the lease over.
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
    /// Whether `complete` binds the finalized version itself (upload-flow
    /// redesign; set by the merged `POST /files` create+plan path with
    /// `bind: "auto"`). `false` = staged behaviour (client binds manually).
    pub auto_bind: bool,
    /// Completion-lease expiry while `state == Completing`; a later
    /// `complete` may take the lease over once this passes.
    pub lease_until: Option<OffsetDateTime>,
    /// Persisted JSON of the successful complete response
    /// ([`StoredCompleteResult`]) once `state == Completed` — the idempotent
    /// re-complete replays it verbatim.
    pub complete_result: Option<String>,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

/// Outcome of `complete_multipart_upload` (upload-flow redesign): either the
/// finished result, or "someone else currently holds the completion lease" —
/// the caller answers `202` and the client polls by re-issuing the same
/// (idempotent) `complete`.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone)]
pub enum MultipartCompleteOutcome {
    Completed(CompletedMultipartUpload),
    Completing { retry_after_secs: u64 },
}

impl MultipartCompleteOutcome {
    /// Unwrap the `Completed` variant; panics on `Completing`. Intended for
    /// tests and single-completer contexts where a 202 is impossible.
    ///
    /// # Panics
    /// Panics when the outcome is [`Self::Completing`].
    #[must_use]
    pub fn unwrap_completed(self) -> CompletedMultipartUpload {
        match self {
            Self::Completed(c) => c,
            Self::Completing { .. } => {
                panic!("complete is still in progress (completion lease held elsewhere)")
            }
        }
    }
}

/// Serializable snapshot of a successful complete response, persisted on the
/// session row (`multipart_uploads.complete_result`) in the same transaction
/// that flips the state to `completed` — the source of truth every
/// idempotent re-complete replays.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredCompleteResult {
    pub version_id: Uuid,
    pub size: i64,
    /// Hex-encoded content hash.
    pub content_hash: String,
    /// `HashMode::as_str` spelling.
    pub hash_mode: String,
    pub part_count: i32,
    pub manifest: Option<String>,
    /// `BindState::as_str` spelling.
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
            manifest: c.manifest.clone(),
            bind_state: c.bind_state.as_str().to_owned(),
            etag: c.etag.clone(),
            current_etag: c.current_etag.clone(),
        }
    }

    /// Rebuild the response object; `None` when the stored JSON predates the
    /// current schema (caller falls back to rebuilding from the version row).
    #[must_use]
    pub fn into_completed(self) -> Option<CompletedMultipartUpload> {
        let hash_mode = HashMode::parse(&self.hash_mode)?;
        let bind_state = match self.bind_state.as_str() {
            "bound" => BindState::Bound,
            "conflict" => BindState::Conflict,
            "manual" => BindState::Manual,
            _ => return None,
        };
        Some(CompletedMultipartUpload {
            version_id: self.version_id,
            size: self.size,
            hash_algorithm: crate::infra::content::hash::ALGORITHM,
            content_hash: hex::decode(&self.content_hash).ok()?,
            hash_mode,
            part_count: self.part_count,
            manifest: self.manifest,
            bind_state,
            etag: self.etag,
            current_etag: self.current_etag,
        })
    }
}

/// Result of a successful `complete_multipart_upload` (item 3.3): everything
/// the ADR-0006 assembly step already computes, returned to the caller
/// instead of being discarded behind a `204 No Content`.
///
/// `manifest` is included so a client can independently re-verify the
/// composite hash (`docs/features/content-hash-modes.md` §"Client-Side
/// Manifest Re-Verification") without a second round-trip. At ~90 bytes per
/// part this is ~1 MiB at the [`MAX_PART_COUNT`] ceiling, which
/// [`compute_plan`] now actually enforces (independent of backend) —
/// acceptable for a one-shot response.
#[domain_model]
#[derive(Debug, Clone)]
pub struct CompletedMultipartUpload {
    pub version_id: Uuid,
    pub size: i64,
    /// Always `"SHA-256"` — the only hash algorithm used by either ADR-0006
    /// hash mode.
    pub hash_algorithm: &'static str,
    /// The ADR-0006 composite root `sha256(manifest)` — or, for a **one-part
    /// plan** (ADR-0006 single-part amendment), plain `sha256(object bytes)`
    /// (identical to the single part's streaming digest).
    pub content_hash: Vec<u8>,
    /// [`HashMode::MultipartCompositeSha256`] for a plan of two or more
    /// parts; [`HashMode::WholeSha256`] for the degenerate one-part plan
    /// (ADR-0006 single-part amendment — no composite wrapping, no manifest).
    pub hash_mode: HashMode,
    pub part_count: i32,
    /// Wire-format manifest text (`Manifest::to_wire_string`); `None` for a
    /// one-part plan, whose version is `whole-sha256` and has no manifest.
    pub manifest: Option<String>,
    /// Bind outcome (upload-flow redesign) — see [`BindState`].
    pub bind_state: BindState,
    /// The file's content ETag after a successful bind
    /// (`bind_state == Bound` only).
    pub etag: Option<String>,
    /// The file's CURRENT content ETag when the bind CAS was lost
    /// (`bind_state == Conflict` only) — the value a manual rebind's
    /// `If-Match` needs, no re-upload required.
    pub current_etag: Option<String>,
}

/// Bind outcome state — ONE state model shared by both upload paths
/// (upload-flow redesign): the multipart `complete` response's `bind_state`
/// field and the single-part `PUT` response's `X-FS-Bound` header carry the
/// same three values.
///
/// * `Bound` — the upload bound its version as the file's current content
///   (CAS won); the new content ETag accompanies it.
/// * `Conflict` — an auto-bind was requested but the CAS lost (content moved
///   concurrently). The upload itself SUCCEEDED: the version is `available`
///   and a manual `bind` (with the accompanying current ETag as `If-Match`)
///   makes it live without re-uploading a byte.
/// * `Manual` — no bind was requested (`bind: "manual"` / the standalone
///   initiate path); the client binds explicitly, as before the redesign.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindState {
    Bound,
    Conflict,
    Manual,
}

impl BindState {
    /// Wire spelling (`bind_state` field / `X-FS-Bound` header value —
    /// except `Bound`, which the header spells `"true"` for ergonomic
    /// boolean-ish checks; see api.md).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bound => "bound",
            Self::Conflict => "conflict",
            Self::Manual => "manual",
        }
    }
}

/// Result of `GET /files/{id}/multipart/{upload_id}` (item 3.4): the
/// session's current state plus the received/missing parts, with fresh
/// resume URLs for any part not yet uploaded.
///
/// `upload_url` on each [`MissingPart`] is only populated while the session
/// is still `in_progress` and unexpired -- a terminal (`completed`/`aborted`)
/// or expired session reports state and part accounting only, no resume
/// URLs (there is nothing left to resume). Each minted resume URL's `exp` is
/// capped at `min(session.expires_at, now + url_ttl_secs)`: it never outlives
/// the session, but it also never gets a longer TTL than any freshly-minted
/// URL just because the session itself is long-lived.
///
/// @cpt-cf-file-storage-fr-multipart-upload
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
    /// Parts already reported (via the sidecar's report-part callback), in
    /// ascending `part_number` order (mirrors `list_multipart_parts`).
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

/// One part not yet uploaded, with its planned bounds recomputed from the
/// session's `(declared_size, part_size)` columns and, when resumable, a
/// freshly-minted signed upload URL.
#[domain_model]
#[derive(Debug, Clone)]
pub struct MissingPart {
    pub part_number: u32,
    pub offset: u64,
    pub size: u64,
    /// `Some` only for a live, unexpired `in_progress` session; its token
    /// `exp` is `min(session.expires_at, now + url_ttl_secs)` -- the same
    /// short URL TTL as any freshly-minted part URL, additionally capped so
    /// it never outlives the session it resumes.
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
/// 5 MiB is the S3 minimum for all parts except the last. This value also
/// doubles as the lower bound of the sane range that a client-supplied
/// `preferred_part_size` is validated against at the service boundary
/// (`MultipartService::initiate_multipart_upload`, P2 remediation 2.11).
pub const DEFAULT_MIN_PART_SIZE: u64 = 5 * 1024 * 1024;

/// Maximum accepted `preferred_part_size` client hint (P2 remediation 2.11).
///
/// 5 GiB is S3's absolute maximum part size. Values above this cannot be a
/// legitimate part-size preference; they are rejected at the service
/// boundary before ever reaching [`compute_plan`]. The checked arithmetic in
/// [`compute_plan`]/[`round_up_to`] below is kept regardless, as
/// defense-in-depth for callers that bypass that boundary.
pub const MAX_PART_SIZE: u64 = 5 * 1024 * 1024 * 1024;

/// Hard ceiling on the number of parts a single multipart plan may contain,
/// enforced by [`compute_plan`] independently of any one backend's own
/// native limit (ADR-0006 §"Manifest storage", `docs/features/
/// content-hash-modes.md` §12 risk 1: "a backend without such a native limit
/// cannot silently produce an unbounded manifest").
///
/// `10_000` matches S3's own hard multipart-part limit (`S3Backend::upload_part`
/// rejects `part_number > 10_000`), so this ceiling never makes an S3-backed
/// plan any more restrictive than the backend already is — it only closes the
/// gap for backends (e.g. the in-memory dev/test backend) that impose no such
/// limit of their own, and bounds the manifest size (~800 KB worst case at
/// ~90 bytes/entry) and the per-initiate signed-URL minting cost.
pub const MAX_PART_COUNT: u64 = 10_000;

/// Compute the server-chosen `part_size` and generate the plan skeleton
/// (without URLs — those are injected by `MultipartService`).
///
/// Rules (FEATURE §3):
/// - `part_size = max(preferred, backend_min)` rounded up to the nearest
///   multiple of `DEFAULT_MIN_PART_SIZE` (BLAKE3-friendly alignment deferred,
///   SHA-256 is used in P2).
/// - `parts = ceil(declared_size / part_size)`.
/// - The last part's `size` is `declared_size - (parts - 1) * part_size`.
/// - If that part count would exceed [`MAX_PART_COUNT`], `part_size` is
///   **widened** (never exceeding [`MAX_PART_SIZE`]) just enough to bring the
///   plan back under the ceiling; if even [`MAX_PART_SIZE`] cannot fit
///   `declared_size` within [`MAX_PART_COUNT`] parts, the plan is rejected
///   rather than minted (see "Errors" below).
///
/// One raw part entry from `compute_plan`: `(part_number, offset, size)`.
pub type RawPartEntry = (u32, u64, u64);

/// Returns `(part_size, parts_count)` ready to be used by the caller.
///
/// # Errors
/// Returns [`DomainError::Validation`] if:
/// - the part-size arithmetic would overflow `u64`. Callers are expected to
///   have already validated `preferred_part_size` against a sane range (P2
///   remediation 2.11); this is a defense-in-depth guard against a huge/
///   adversarial value reaching this function by another path, rather than
///   panicking or silently wrapping.
/// - `declared_size` is so large that even the maximum permissible part size
///   ([`MAX_PART_SIZE`]) would require more than [`MAX_PART_COUNT`] parts —
///   there is no part size this backend/plan can use that keeps the upload
///   within the enforced part-count ceiling.
///
/// This check (and any part-size widening it triggers) runs *before* the
/// plan `Vec` is allocated, so an attacker-controlled `declared_size` (up to
/// and including `u64::MAX`) is rejected without ever allocating memory
/// proportional to it.
pub fn compute_plan(
    declared_size: u64,
    preferred_part_size: Option<u64>,
    backend_min_part_size: Option<u64>,
) -> Result<(u64, Vec<RawPartEntry>), DomainError> {
    let min = backend_min_part_size.unwrap_or(DEFAULT_MIN_PART_SIZE);
    let preferred = preferred_part_size.unwrap_or(min);
    // Part size = max(preferred, backend_min), rounded up to the nearest `min`.
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

    // Enforce the MAX_PART_COUNT ceiling *before* computing/allocating the
    // parts vector: a declared_size that would blow past the ceiling at the
    // chosen part_size must either widen part_size to fit, or be rejected
    // outright -- never silently produce (or attempt to allocate space for)
    // an unbounded number of parts.
    if declared_size.div_ceil(part_size) > MAX_PART_COUNT {
        // Smallest part_size that keeps this declared_size within
        // MAX_PART_COUNT parts.
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
        // Widen part_size just enough to fit; `minimal_required <= MAX_PART_SIZE`
        // was just checked, and `minimal_required > part_size` follows from the
        // `div_ceil` check above, so this can only increase part_size.
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

/// Round `value` up to the next multiple of `align` (≥ 1).
///
/// Uses checked arithmetic: returns `None` on overflow instead of
/// panicking (under overflow-checks) or silently wrapping to a tiny value
/// (P2 remediation 2.11).
fn round_up_to(value: u64, align: u64) -> Option<u64> {
    if align == 0 {
        return Some(value);
    }
    value.div_ceil(align).checked_mul(align)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P2 remediation 2.11: a near-`u64::MAX` value must not panic (under
    /// overflow-checks) or silently wrap to a tiny `part_size` — it must be
    /// reported as `None` so the caller can turn it into a domain error.
    /// `round_up_to` is private, so this is a same-module unit test rather
    /// than an integration test in `tests/multipart_test.rs`.
    #[test]
    fn round_up_to_does_not_overflow_on_max_input() {
        assert_eq!(round_up_to(u64::MAX, DEFAULT_MIN_PART_SIZE), None);
        assert_eq!(round_up_to(u64::MAX, u64::MAX), Some(u64::MAX));
        assert_eq!(round_up_to(1, u64::MAX), Some(u64::MAX));
        // Sanity: ordinary inputs still round up correctly.
        assert_eq!(round_up_to(7, 5), Some(10));
        assert_eq!(round_up_to(10, 5), Some(10));
    }

    /// `compute_plan` must surface the overflow as a domain error instead of
    /// panicking, even when called directly with an adversarial
    /// `preferred_part_size` that bypasses the service-boundary validation.
    #[test]
    fn compute_plan_returns_validation_error_on_overflowing_preferred_part_size() {
        let err = compute_plan(u64::MAX, Some(u64::MAX), None).unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }

    /// An absurd `declared_size` (`u64::MAX`, with no `preferred_part_size`
    /// hint, i.e. the P2 remediation 2.11 boundary check on the client hint
    /// never fires) must be rejected quickly as a domain error instead of
    /// driving a `Vec::with_capacity` allocation proportional to
    /// `declared_size / DEFAULT_MIN_PART_SIZE` (~3.5e12 entries, ~84 TB).
    #[test]
    fn compute_plan_rejects_absurd_declared_size_without_allocating() {
        let err = compute_plan(u64::MAX, None, None).unwrap_err();
        assert!(
            matches!(err, DomainError::Validation { .. }),
            "expected Validation, got {err:?}"
        );
    }

    /// A `declared_size` that would need more than `MAX_PART_COUNT` parts at
    /// the caller's preferred part size must not be rejected outright --
    /// `compute_plan` should first try widening `part_size` (never beyond
    /// `MAX_PART_SIZE`) to bring the plan back within the ceiling.
    #[test]
    fn compute_plan_widens_part_size_to_stay_within_max_part_count() {
        // 15,000 parts at DEFAULT_MIN_PART_SIZE would exceed MAX_PART_COUNT.
        let declared_size = 15_000 * DEFAULT_MIN_PART_SIZE;
        let (part_size, parts) = compute_plan(declared_size, Some(DEFAULT_MIN_PART_SIZE), None)
            .expect("must widen instead of rejecting");

        assert!(
            part_size > DEFAULT_MIN_PART_SIZE,
            "part_size must have been widened above the caller's preferred value, got {part_size}"
        );
        assert!(
            part_size <= MAX_PART_SIZE,
            "widened part_size must never exceed MAX_PART_SIZE, got {part_size}"
        );
        assert!(
            (parts.len() as u64) <= MAX_PART_COUNT,
            "plan must fit within MAX_PART_COUNT parts, got {}",
            parts.len()
        );
        let total: u64 = parts.iter().map(|(_, _, size)| *size).sum();
        assert_eq!(
            total, declared_size,
            "sum of part sizes must equal declared_size"
        );
    }

    /// A `declared_size` too large to fit within `MAX_PART_COUNT` parts even
    /// at `MAX_PART_SIZE` (the backend's absolute max part size) cannot be
    /// widened away -- it must be rejected with a clear domain error rather
    /// than minting a plan that would exceed the ceiling.
    #[test]
    fn compute_plan_rejects_declared_size_beyond_max_part_size_times_max_part_count() {
        let declared_size = MAX_PART_SIZE * MAX_PART_COUNT + 1;
        let err = compute_plan(declared_size, None, None).unwrap_err();
        assert!(
            matches!(err, DomainError::Validation { .. }),
            "expected Validation, got {err:?}"
        );
    }

    /// Exactly at the `MAX_PART_SIZE * MAX_PART_COUNT` boundary, the plan
    /// must still be accepted (widened to exactly `MAX_PART_SIZE`).
    #[test]
    fn compute_plan_accepts_declared_size_exactly_at_the_boundary() {
        let declared_size = MAX_PART_SIZE * MAX_PART_COUNT;
        let (part_size, parts) =
            compute_plan(declared_size, None, None).expect("boundary value must be accepted");
        assert_eq!(part_size, MAX_PART_SIZE);
        assert_eq!(parts.len() as u64, MAX_PART_COUNT);
    }
}
