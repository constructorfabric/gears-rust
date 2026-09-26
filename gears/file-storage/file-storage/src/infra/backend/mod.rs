//! Pluggable storage-backend abstraction
//! (`cpt-cf-file-storage-component-backend-abstraction`,
//! `cpt-cf-file-storage-fr-backend-abstraction`).
//!
//! A backend stores immutable content blobs keyed by an opaque path
//! (`/{file_id}/{version_id}` by convention). Clients never address a backend
//! directly — content moves only through the sidecar (backend opacity).
//!
//! P1 ships two backend *types* (`cpt-cf-file-storage-fr-backend-capabilities`
//! target "≥2 backends"): a local filesystem backend and an in-memory backend.
//! P2 adds an `S3Backend` (ADR-0005 `cpt-cf-file-storage-adr-s3-client-selection`)
//! on top of those two. ADR-0005 remains `status: proposed` until a team
//! security review runs (its new external HTTP-signing/XML-parsing
//! dependencies are the trigger) — the code is safe to build and test on a
//! branch regardless, but merging it to `main` is gated on that review.
//! GCS/etc. remain deferred beyond that.

mod hashing_length_guard;
mod in_memory;
mod length_guard;
mod local_fs;
mod s3;

pub(crate) use hashing_length_guard::hashing_length_guard;
pub(crate) use length_guard::length_guard;

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use file_storage_sdk::ByteRange;

use crate::domain::error::DomainError;
use crate::infra::content::hash_mode::Manifest;

pub use in_memory::InMemoryBackend;
pub use local_fs::LocalFsBackend;
pub use s3::S3Backend;

use crate::infra::content::hash_mode::ManifestEntry;

/// Hard ceiling on [`StorageBackend::read_prefix`]'s `max_bytes` argument.
/// Every real caller only ever needs a small, fixed-size leading slice — a
/// MIME-sniff prefix (`MIME_SNIFF_PREFIX_BYTES`, 8 KiB) is the largest one —
/// never an arbitrary range (that is streamed only through
/// [`StorageBackend::get_range_stream`]) and never a whole-object read.
/// Set comfortably above every real caller's actual need so it never
/// constrains legitimate use, while still making a whole-object
/// `read_prefix` call impossible by construction: exceeding it is a caller
/// bug, rejected as a validation error rather than silently clamped.
pub(crate) const MAX_READ_PREFIX_BYTES: u64 = 64 * 1024;

/// Whether a backend I/O error is transient (a retry is expected to
/// succeed) as opposed to a persistent fault. Shared by every backend so a
/// stalled read/write, a signal-interrupted syscall, a would-block on a
/// non-blocking fd, or a dropped/reset connection are classified identically
/// regardless of which backend observed them; a missing path, permission
/// denied, disk full, or corrupt data is never transient — retrying changes
/// nothing about any of those.
///
/// `UnexpectedEof` is included: every backend's chunked read (e.g.
/// `LocalFsBackend::chunked_file_stream`) raises exactly this kind when a
/// stream ends short of the length the caller already committed to, which is
/// itself either a concurrent write racing the read or a dropped connection
/// truncating the body — both retry-worthy, never a reason to report a
/// permanent fault.
pub(crate) fn is_transient_io_error(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::UnexpectedEof
    )
}

/// Classify a mid-stream read failure (a chunk error from a `get_stream`-
/// style `BoxStream` that already opened successfully) into a `DomainError`:
/// [`is_transient_io_error`] picks `backend_unavailable` (retryable, REST
/// `503` + `Retry-After`) over `backend` (permanent, `500`). `context`
/// identifies which read this was (e.g. "read-back stream read failed") so
/// the message stays specific without the caller hand-rolling it.
pub(crate) fn classify_stream_io_error(
    backend_id: &str,
    context: &str,
    e: &std::io::Error,
) -> DomainError {
    let msg = format!("{context}: {e}");
    if is_transient_io_error(e.kind()) {
        DomainError::backend_unavailable(backend_id, msg)
    } else {
        DomainError::backend(backend_id, msg)
    }
}

/// Shared budget check every [`StorageBackend::read_prefix`] implementation
/// runs before touching its backend, so the ceiling is enforced identically
/// (and the error shape is identical) regardless of which backend answers.
pub(crate) fn check_read_prefix_budget(max_bytes: u64) -> Result<(), DomainError> {
    if max_bytes > MAX_READ_PREFIX_BYTES {
        return Err(DomainError::validation(
            "max_bytes",
            format!(
                "read_prefix max_bytes {max_bytes} exceeds the {MAX_READ_PREFIX_BYTES}-byte ceiling"
            ),
        ));
    }
    Ok(())
}

/// One part of a multipart completion, as handed to `complete_multipart`:
/// `(part_number, offset, part_hash, backend_etag)` (ADR-0006). Named to keep
/// the `StorageBackend` trait signature readable and shared verbatim across
/// backends.
pub type MultipartCompletionPart = (u32, u64, [u8; 32], String);

/// Build the ADR-0006 offset-manifest + `root` from the
/// [`MultipartCompletionPart`] tuples a `complete_multipart` call receives.
/// Shared by every multipart-capable backend so the canonical wire format is
/// produced in exactly one place (via [`Manifest::to_wire_string`]) — never
/// hand-rolled per backend, where a subtle divergence would silently yield a
/// different `root`.
///
/// Entries are sorted by ascending offset (identical to ascending part-number
/// order for any valid plan) before the manifest is assembled, so a caller
/// that passes parts out of order still produces the canonical manifest.
pub(crate) fn build_manifest_and_root(
    parts: &[MultipartCompletionPart],
) -> Result<(Manifest, [u8; 32]), DomainError> {
    let mut entries: Vec<ManifestEntry> = parts
        .iter()
        .map(|(_, offset, digest, _)| ManifestEntry {
            offset: *offset,
            digest: *digest,
        })
        .collect();
    entries.sort_by_key(|e| e.offset);
    let manifest = Manifest::new(entries)?;
    let root = manifest.root();
    Ok((manifest, root))
}

/// Result of [`StorageBackend::publish_exclusive`]: the measured size/digest
/// of the stream that was just read, plus whether the write actually landed.
///
/// `created: true` means `path` held nothing before this call and now holds
/// exactly the streamed bytes. `created: false` means `path` already held a
/// blob and this call left it untouched — the destination was **never**
/// overwritten. `bytes_written`/`digest` are always populated (describing
/// *this* attempt's bytes) even when `created` is `false`, so a caller can
/// still run a server-side idempotency check (e.g. the sidecar's finalize
/// callback) without a second read of the backend.
#[derive(Debug, Clone, Copy)]
pub struct PublishOutcome {
    pub bytes_written: u64,
    pub digest: [u8; 32],
    pub created: bool,
}

/// Optional features a backend may declare
/// (`cpt-cf-file-storage-fr-backend-capabilities`). Versioning is **not** here —
/// it is implemented at the `FileStorage` level on every backend.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BackendCapabilities {
    /// Native chunked upload with server-side assembly (P2 multipart).
    pub multipart_native: bool,
    /// Server-side encryption at rest (P3).
    pub encryption_native: bool,
    /// Native byte-range reads (otherwise `FileStorage` slices after a full read).
    pub range_native: bool,
    /// Internal-only presigned URLs (backend-to-backend tooling); never exposed.
    pub presigned_url_internal: bool,
    /// Maximum blob size the backend accepts in bytes. `None` = unbounded.
    pub max_size_bytes: Option<u64>,
    /// Whether content written to this backend survives process restarts /
    /// crashes (e.g. `local-fs`, S3). `false` for volatile backends (e.g. the
    /// in-memory dev/test backend) — `migrate_backend` gates moves onto a
    /// non-durable backend behind an elevated authorization scope.
    pub durable: bool,
}

/// A storage backend: moves immutable content blobs. All methods are keyed by an
/// opaque backend path.
#[async_trait]
pub trait StorageBackend: Send + Sync {
    /// Stable backend identifier (matches `file_versions.backend_id`).
    fn id(&self) -> &str;

    /// The capabilities this backend advertises.
    fn capabilities(&self) -> BackendCapabilities;

    /// Stream a blob into `path`, hashing incrementally and enforcing
    /// `max_size` as bytes arrive rather than buffering the whole body first
    /// (`cpt-cf-file-storage-fr-backend-abstraction`, memory-DoS fix). Returns
    /// `(bytes_written, sha256_digest)`.
    ///
    /// No default: a buffering fallback here would defeat the whole point of
    /// this trait having no whole-object `put` to fall back to. Every real
    /// backend (`LocalFsBackend`, `S3Backend`, `InMemoryBackend`) implements
    /// this natively.
    async fn put_stream(
        &self,
        path: &str,
        stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<(u64, [u8; 32]), DomainError>;

    /// Publish a blob at `path`, but **only if nothing is stored there yet**
    /// (create-exclusive semantics) — unlike [`Self::put_stream`], which is
    /// documented as overwrite-allowed and stays that way for its existing
    /// callers (per-part multipart writes, which are deliberately
    /// overwrite-safe for resume; `migrate_backend`'s write to a fresh
    /// backend). This method exists for exactly one call site: the sidecar's
    /// single-shot upload handler, publishing a version's *final*, canonical
    /// object at `/{file_id}/{version_id}`.
    ///
    /// # Why this must not just overwrite
    /// A `PUT` token's signature covers `op`/`file_id`/`version_id`/size/hash
    /// *constraints*, never the body bytes (DESIGN.md, ADR-0003), and stays
    /// valid until `exp`. Once a version has been finalized and bound as a
    /// file's live content, a holder of that same still-unexpired token could
    /// otherwise re-`PUT` different bytes to the same backend path and
    /// silently replace the live, already-served content out from under the
    /// recorded size/hash/`ETag`/MIME — a HIGH-severity immutability break.
    /// Making this call create-exclusive closes that: only the
    /// *first* write to a given path ever lands; every subsequent attempt
    /// observes `created: false` and the existing bytes are provably
    /// untouched.
    ///
    /// # This default implementation is non-atomic (TOCTOU) — read before relying on it
    /// It is an `exists` check followed by a separate `put`, with a real race
    /// window in between: two concurrent callers can both observe "nothing
    /// there yet" and both proceed to `put`, so two racing publishes to the
    /// same `path` can each report `created: true` and the second `put`'s
    /// bytes silently win, defeating the create-exclusive guarantee this
    /// method exists to provide. It is only "good enough" as a
    /// backend-agnostic fallback for a backend that cannot yet do better —
    /// not a substitute for a real atomic primitive.
    ///
    /// [`LocalFsBackend`](super::LocalFsBackend) (`std::fs::hard_link`, which
    /// atomically fails with `AlreadyExists` if the target already has a
    /// directory entry) and [`InMemoryBackend`](super::InMemoryBackend) (a
    /// single mutex guards both the check and the insert) both override this
    /// with a truly atomic implementation, closing the race for those two
    /// backends completely.
    ///
    /// [`S3Backend`](super::backend::S3Backend) implements it with an atomic
    /// conditional-write (`If-None-Match: *` on the terminal
    /// `PutObject`/`CompleteMultipartUpload`, mapping the resulting `412
    /// Precondition Failed` to `created: false` — the same outcome
    /// `LocalFsBackend`/`InMemoryBackend` produce). That guarantee is
    /// provider-dependent: it requires an endpoint that honours S3
    /// conditional writes (native AWS S3 since 2024-08, and S3-compatible
    /// stores that implement it). S3 support is opt-in (`s3_backends`
    /// config) and release-gated by
    /// [ADR-0005](../../../docs/ADR/0005-cpt-cf-file-storage-adr-s3-client-selection.md)
    /// (also see [ADR-0003](../../../docs/ADR/0003-cpt-cf-file-storage-adr-sidecar-data-plane.md)'s
    /// "Known gap" paragraph); validating a specific target deployment's
    /// conditional-write support is part of that gate.
    ///
    /// No default: a backend-agnostic `exists`-then-write fallback would
    /// necessarily be non-atomic (TOCTOU), reintroducing the exact race this
    /// method exists to close. Every backend wired today (`local-fs`,
    /// `in-memory`, `s3`) provides a genuinely atomic implementation.
    async fn publish_exclusive(
        &self,
        path: &str,
        stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<PublishOutcome, DomainError>;

    /// Stream the blob at `path` in chunks, without necessarily buffering the
    /// whole object in memory at once. Used by `finalize_upload`'s read-back
    /// verification (`cpt-cf-file-storage-fr-backend-abstraction`,
    /// memory-safety fix mirroring `put_stream`'s streaming-write bound) to
    /// recompute the actual size/hash/MIME-sniff-prefix from the real stored
    /// bytes without re-inflating a potentially huge object into memory, and
    /// by the sidecar's whole-object `download` handler (P2 download-memory
    /// fix) so a `GET` without a `Range` header streams straight into the
    /// HTTP response body instead of first landing in a `Bytes` buffer.
    ///
    /// `expected_len` is the length the caller already committed to
    /// elsewhere (the sidecar's already-set `Content-Length`, from an earlier
    /// `stat`; `finalize_upload`'s own immediately-preceding `stat`) before
    /// this call was ever made. Every implementation must verify the object
    /// it actually opens still has this exact length **before yielding any
    /// byte**, and fail rather than stream a body that would disagree with
    /// what the caller already promised — the caller's earlier observation
    /// and this call's own independent read of the object are two separate
    /// observations a concurrent write can invalidate in between (see
    /// [`LocalFsBackend::get_stream`] for the concrete race this closes).
    ///
    /// Declared `BoxStream<'static, _>` rather than borrowing `&self`'s
    /// lifetime: every implementation below moves fully-owned data into the
    /// returned stream (an owned file handle, an owned `reqwest::Response`,
    /// an owned `Bytes` — never a reference back into `self`), which is
    /// exactly what lets a caller hand the stream straight to
    /// `axum::body::Body::from_stream`, which requires a genuinely `'static`
    /// stream — a `BoxStream<'_, _>` tied to a short-lived `&Arc<dyn
    /// StorageBackend>` borrow could never satisfy that without an unsound
    /// lifetime cast, and this crate forbids `unsafe` outright
    /// (`unsafe_code = "forbid"` at the workspace level).
    ///
    /// No default: a single-chunk buffering fallback would defeat the point
    /// of removing the whole-object `get` this used to fall back to. Every
    /// real backend (`LocalFsBackend`, `S3Backend`, `InMemoryBackend`)
    /// implements this natively.
    async fn get_stream(
        &self,
        path: &str,
        expected_len: u64,
    ) -> Result<futures::stream::BoxStream<'static, std::io::Result<Bytes>>, DomainError>;

    /// Read up to `max_bytes` from the **start** of the blob at `path`,
    /// without reading (or buffering) anything beyond that leading prefix —
    /// hard-capped at [`MAX_READ_PREFIX_BYTES`] (every implementation must
    /// enforce this via [`check_read_prefix_budget`] before touching its
    /// backend). This — plus [`Self::stat`] for a pure existence check — is
    /// the only way the *production control-plane* API can see any of an
    /// object's bytes; an arbitrary range or a whole object is streamed only
    /// through [`Self::get_range_stream`]/[`Self::get_stream`], which the
    /// sidecar's data-plane download handlers use, never the control plane.
    ///
    /// `Ok(None)` mirrors [`Self::stat`]'s `Ok(None)`: nothing is stored at
    /// `path`. `Ok(Some(bytes))` gives `bytes.len() == min(max_bytes, object
    /// length)` — a caller asking for more than the object holds gets the
    /// whole (short) object back, not an error.
    async fn read_prefix(&self, path: &str, max_bytes: u64) -> Result<Option<Bytes>, DomainError>;

    /// Stream a byte range of the blob at `path`, without necessarily
    /// buffering the whole resolved range in memory at once — the
    /// range-request mirror of [`Self::get_stream`]'s relationship to
    /// [`Self::get`]. Used by the sidecar's `download` handler for a `Range`-
    /// qualified `GET` (P2 download-memory fix): `Range: bytes=0-` is the
    /// very first request many media players issue, and
    /// `ByteRange::OpenEnded::resolve` turns that into a range spanning the
    /// *entire* object, so an unbounded-media-length upload combined with a
    /// naive `Vec::with_capacity(len)` read (the old `get_range` default's
    /// shape) was not a hypothetical — see the e2e video demo under
    /// `testing/e2e/gears/file_storage/web_ui/`.
    ///
    /// Same `'static` rationale as [`Self::get_stream`]: every override below
    /// moves fully-owned data into the returned stream, never a borrow of
    /// `self`, so declaring it `'static` costs nothing and is what lets the
    /// sidecar hand it straight to `axum::body::Body::from_stream`.
    ///
    /// `expected_len` mirrors [`Self::get_stream`]'s own parameter, but for a
    /// range read it is the resolved *range's* length — what the caller has
    /// already put in its `Content-Length` (the sidecar resolves `range`
    /// itself first to build `Content-Range`/`Content-Length`, then passes
    /// this call the range it already committed to). Every implementation
    /// must verify that re-resolving `range` against the object it actually
    /// opens still yields a range of this exact length before yielding any
    /// byte, and fail rather than stream a body whose length would disagree
    /// with what the caller already promised — see
    /// [`LocalFsBackend::get_range_stream`] for the concrete race this closes.
    ///
    /// No default: this used to fall back to `get_range` (whole resolved
    /// range as a single chunk over a whole-object `get`), which no longer
    /// exists. `local-fs`, `s3`, and `in-memory` all implement this natively
    /// (see their own doc comments), each still enforcing the `expected_len`
    /// contract above: a mismatch is a [`DomainError::conflict`], surfaced
    /// before the single chunk is ever handed back.
    async fn get_range_stream(
        &self,
        path: &str,
        range: ByteRange,
        expected_len: u64,
    ) -> Result<futures::stream::BoxStream<'static, std::io::Result<Bytes>>, DomainError>;

    /// The total length in bytes of the blob at `path`, without necessarily
    /// reading its content. Range-aware callers (e.g. the sidecar's
    /// `download` handler) use this to resolve `Range` requests
    /// against the actual blob length and to build a correct `Content-Range`
    /// header, without materializing the whole blob first.
    ///
    /// No default: this used to fall back to `get`, which no longer exists.
    /// Every real backend implements a cheap standalone stat (e.g.
    /// `LocalFsBackend`'s filesystem metadata, `S3Backend`'s `HeadObject`).
    async fn size(&self, path: &str) -> Result<u64, DomainError>;

    /// Delete the blob at `path`. Missing blobs are treated as success
    /// (idempotent delete).
    async fn delete(&self, path: &str) -> Result<(), DomainError>;

    /// Whether a blob exists at `path`.
    async fn exists(&self, path: &str) -> Result<bool, DomainError>;

    /// Combined existence-check-plus-size stat: `Ok(None)` when
    /// nothing is stored at `path` (mirrors `exists`'s `Ok(false)`),
    /// `Ok(Some(len))` when a blob is present with byte length `len`
    /// (mirrors `size`'s `Ok(n)`), and `Err` for a genuine backend fault
    /// distinct from either -- the same three-way split `exists` already
    /// documents, just resolved by one round-trip instead of two.
    ///
    /// Without this, a caller needing both facts -- e.g. the sidecar's
    /// download handlers, to answer `404` distinctly from a backend fault
    /// and to size the response / resolve a `Range` -- would need a separate
    /// `exists` and `size` call, two `HeadObject`s against `S3Backend` for
    /// every `GET`/`HEAD`.
    ///
    /// The default implementation composes `exists` then `size`, so a
    /// backend that hasn't been upgraded to a single combined stat still
    /// behaves correctly; `local-fs`, `s3`, and `in-memory` all override this
    /// with a single native stat.
    async fn stat(&self, path: &str) -> Result<Option<u64>, DomainError> {
        if !self.exists(path).await? {
            return Ok(None);
        }
        Ok(Some(self.size(path).await?))
    }

    /// Initiate a multipart upload for `path`. Returns an opaque backend handle.
    /// Default returns an error — backends must opt-in by overriding this method
    /// and setting `multipart_native: true` in their capabilities.
    async fn initiate_multipart(&self, _path: &str) -> Result<String, DomainError> {
        Err(DomainError::multipart_not_supported(self.id()))
    }

    /// Upload one part, streamed rather than buffered whole. Returns
    /// `(backend_etag, part_hash_bytes)`.
    ///
    /// `part_offset` is the part's start byte offset within the assembled
    /// object (ADR-0006). It is not used to hash the part — `part_hash` is a
    /// flat `sha256` over the streamed bytes exactly as before — but is
    /// threaded through so the backend can build the offset-manifest at
    /// `complete` time without re-deriving it from a plan it may not retain.
    ///
    /// `len` is the part's exact, authoritative expected size — for the
    /// native multipart write path this is always the server-minted token
    /// claim (`claims.multipart.size`), never a client-supplied header, since
    /// a backend that needs the length up front to sign/send a single
    /// request (S3's `UploadPart`) cannot treat it as a mere ceiling the way
    /// [`Self::put_stream`]'s `max_size` is. An implementation MUST verify
    /// `stream` yields exactly `len` bytes before treating the part as
    /// uploaded — fewer or more is an error, and the part must not be
    /// considered stored (see [`S3Backend`](super::S3Backend)'s
    /// implementation, which enforces this on the same pass that computes
    /// `part_hash`, via `hashing_length_guard`).
    ///
    /// Declared `BoxStream<'static, _>` for the same reason
    /// [`Self::get_stream`] is: every real caller (the sidecar's
    /// `write_multipart_part_native`, this trait's own S3 `put_stream`/
    /// `publish_exclusive` multipart chunking) already owns fully-detached
    /// data with no borrow back into a short-lived caller frame, and
    /// `'static` is what lets an implementation hand the stream straight to
    /// an HTTP client body (e.g. `reqwest::Body::wrap_stream`) without an
    /// intermediate buffering copy.
    async fn upload_part_stream(
        &self,
        _path: &str,
        _upload_handle: &str,
        _part_number: u32,
        _part_offset: u64,
        _stream: futures::stream::BoxStream<'static, std::io::Result<Bytes>>,
        _len: u64,
    ) -> Result<(String, Vec<u8>), DomainError> {
        Err(DomainError::multipart_not_supported(self.id()))
    }

    /// Complete a multipart upload, assembling all uploaded parts in order.
    ///
    /// `parts` are `(part_number, offset, part_hash, backend_etag)` tuples the
    /// control plane already collected during upload — the backend MUST build
    /// the offset-manifest and its `root` from these (ADR-0006 mode 2) rather
    /// than re-reading the assembled object. The backend still performs its
    /// own native completion (S3 `CompleteMultipartUpload`, in-memory
    /// assembly) but never re-`GetObject`s the object just to hash it.
    ///
    /// Returns `(manifest, root)` where `root = sha256(manifest.to_wire_string())`
    /// — the control plane stores `root` as the version's `hash_value` and the
    /// manifest text in `version_hash_manifest`.
    async fn complete_multipart(
        &self,
        _path: &str,
        _upload_handle: &str,
        _parts: &[MultipartCompletionPart],
    ) -> Result<(Manifest, [u8; 32]), DomainError> {
        Err(DomainError::multipart_not_supported(self.id()))
    }

    /// Abort a multipart upload, discarding all uploaded parts.
    async fn abort_multipart(&self, _path: &str, _upload_handle: &str) -> Result<(), DomainError> {
        Err(DomainError::multipart_not_supported(self.id()))
    }

    /// Enumerate all object paths stored by this backend (for orphan
    /// reconciliation). Returns paths in the same format they are stored in
    /// `file_versions.backend_path` (e.g. `"/{file_id}/{version_id}"`).
    ///
    /// The default implementation returns an empty vec — backends that cannot
    /// enumerate their contents are treated conservatively (unknown = skip).
    async fn list_paths(&self) -> Result<Vec<String>, DomainError> {
        Ok(vec![])
    }

    /// Cheap readiness probe: confirms the backend can actually
    /// serve requests right now (e.g. its local-fs root is mounted, its S3
    /// endpoint is reachable and its credentials are valid), without moving
    /// any real content. Used by the sidecar's `/readyz` route for k8s
    /// readiness probing.
    ///
    /// The default implementation is always ready — correct for backends
    /// with no external dependency to probe (e.g. `InMemoryBackend`).
    async fn is_ready(&self) -> Result<(), DomainError> {
        Ok(())
    }
}

/// Registry of configured backends, with one designated default for new uploads.
#[derive(Clone)]
pub struct BackendRegistry {
    backends: BTreeMap<String, Arc<dyn StorageBackend>>,
    default_id: String,
}

impl BackendRegistry {
    /// Build a registry from configured backends; `default_id` must be present.
    pub fn new(
        backends: Vec<Arc<dyn StorageBackend>>,
        default_id: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let default_id = default_id.into();
        // Fail fast on a duplicated backend id rather than silently keeping the
        // last one (which would drop a backend invisibly and make resolution
        // order-dependent).
        let mut map: BTreeMap<String, Arc<dyn StorageBackend>> = BTreeMap::new();
        for b in backends {
            let id = b.id().to_owned();
            if map.insert(id.clone(), b).is_some() {
                return Err(DomainError::backend(id, "duplicate backend id"));
            }
        }
        if !map.contains_key(&default_id) {
            return Err(DomainError::backend(
                default_id,
                "default backend id is not among the configured backends",
            ));
        }
        Ok(Self {
            backends: map,
            default_id,
        })
    }

    /// The backend new uploads are written to.
    #[must_use]
    pub fn default_backend(&self) -> Arc<dyn StorageBackend> {
        // Safe: constructor guarantees the default id is present.
        Arc::clone(&self.backends[&self.default_id])
    }

    /// The id of the default backend.
    #[must_use]
    pub fn default_id(&self) -> &str {
        &self.default_id
    }

    /// Look up a backend by id.
    pub fn get(&self, id: &str) -> Result<Arc<dyn StorageBackend>, DomainError> {
        self.backends
            .get(id)
            .cloned()
            .ok_or_else(|| DomainError::unknown_backend(id))
    }

    /// All configured backends with their capabilities (for `GET /storages`).
    #[must_use]
    pub fn list(&self) -> Vec<(String, BackendCapabilities)> {
        self.backends
            .values()
            .map(|b| (b.id().to_owned(), b.capabilities()))
            .collect()
    }

    /// Iterate all configured backends as `(id, backend)` pairs. Used by the
    /// sidecar's `/readyz` probe, which polls every backend's
    /// [`StorageBackend::is_ready`] rather than just the default one.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Arc<dyn StorageBackend>)> {
        self.backends.iter().map(|(id, b)| (id.as_str(), b))
    }
}

#[cfg(test)]
mod classify_io_error_tests {
    use std::io::ErrorKind;

    use super::{classify_stream_io_error, is_transient_io_error};
    use crate::domain::error::DomainError;

    #[test]
    fn classifies_timed_out_as_transient() {
        assert!(is_transient_io_error(ErrorKind::TimedOut));
    }

    #[test]
    fn classifies_interrupted_and_would_block_as_transient() {
        assert!(is_transient_io_error(ErrorKind::Interrupted));
        assert!(is_transient_io_error(ErrorKind::WouldBlock));
    }

    #[test]
    fn classifies_dropped_connection_kinds_as_transient() {
        assert!(is_transient_io_error(ErrorKind::ConnectionReset));
        assert!(is_transient_io_error(ErrorKind::ConnectionAborted));
        assert!(is_transient_io_error(ErrorKind::BrokenPipe));
    }

    #[test]
    fn classifies_unexpected_eof_as_transient() {
        assert!(is_transient_io_error(ErrorKind::UnexpectedEof));
    }

    #[test]
    fn classifies_not_found_and_permission_denied_as_permanent() {
        assert!(!is_transient_io_error(ErrorKind::NotFound));
        assert!(!is_transient_io_error(ErrorKind::PermissionDenied));
    }

    #[test]
    fn classifies_invalid_data_and_other_as_permanent() {
        assert!(!is_transient_io_error(ErrorKind::InvalidData));
        assert!(!is_transient_io_error(ErrorKind::Other));
    }

    #[test]
    fn classify_stream_io_error_maps_transient_kind_to_backend_unavailable() {
        let e = std::io::Error::new(ErrorKind::TimedOut, "connection timed out");
        let err = classify_stream_io_error("s3-primary", "read-back stream read failed", &e);
        assert!(
            matches!(err, DomainError::BackendUnavailable { .. }),
            "expected BackendUnavailable, got {err:?}"
        );
    }

    #[test]
    fn classify_stream_io_error_maps_permanent_kind_to_backend() {
        let e = std::io::Error::new(ErrorKind::InvalidData, "corrupt chunk");
        let err = classify_stream_io_error("s3-primary", "read-back stream read failed", &e);
        assert!(
            matches!(err, DomainError::Backend { .. }),
            "expected Backend, got {err:?}"
        );
    }
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod backend_tests;
