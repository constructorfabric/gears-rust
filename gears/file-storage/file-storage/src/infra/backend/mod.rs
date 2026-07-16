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

mod in_memory;
mod local_fs;
mod s3;

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
    // @cpt-begin:cpt-cf-file-storage-algo-content-hash-modes-build-manifest:p1:inst-buildmanifest-sort
    entries.sort_by_key(|e| e.offset);
    // @cpt-end:cpt-cf-file-storage-algo-content-hash-modes-build-manifest:p1:inst-buildmanifest-sort
    let manifest = Manifest::new(entries)?;
    let root = manifest.root();
    // @cpt-begin:cpt-cf-file-storage-algo-content-hash-modes-build-manifest:p1:inst-buildmanifest-return
    Ok((manifest, root))
    // @cpt-end:cpt-cf-file-storage-algo-content-hash-modes-build-manifest:p1:inst-buildmanifest-return
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

    /// Write a blob at `path`. Overwrites are allowed (each version is a fresh
    /// path, so callers do not rely on write-once semantics here).
    async fn put(&self, path: &str, bytes: Bytes) -> Result<(), DomainError>;

    /// Stream a blob into `path`, hashing incrementally and enforcing
    /// `max_size` as bytes arrive rather than buffering the whole body first
    /// (`cpt-cf-file-storage-fr-backend-abstraction`, memory-DoS fix). Returns
    /// `(bytes_written, sha256_digest)`.
    ///
    /// The default implementation falls back to buffering the entire stream
    /// in memory (still enforcing `max_size` as chunks arrive, so an
    /// oversized upload is still rejected — just not memory-bounded) before
    /// delegating to `put`. This keeps every backend that hasn't been
    /// upgraded to a true streaming write correct; backends for which
    /// unbounded memory use during upload is a real concern (e.g.
    /// `LocalFsBackend`) should override this method.
    async fn put_stream(
        &self,
        path: &str,
        stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<(u64, [u8; 32]), DomainError> {
        use futures::StreamExt;

        let mut buf = Vec::new();
        let mut stream = stream;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| DomainError::backend(self.id(), e.to_string()))?;
            buf.extend_from_slice(&chunk);
            if max_size.is_some_and(|m| buf.len() as u64 > m) {
                return Err(DomainError::validation("size", "exceeds max_size"));
            }
        }
        let bytes_written = buf.len() as u64;
        let digest =
            crate::infra::content::hash::digest_to_array(crate::infra::content::hash::sha256(&buf));
        self.put(path, Bytes::from(buf)).await?;
        Ok((bytes_written, digest))
    }

    /// Publish a blob at `path`, but **only if nothing is stored there yet**
    /// (create-exclusive semantics) — unlike [`Self::put_stream`], which is
    /// documented as overwrite-allowed and stays that way for its existing
    /// callers (per-part multipart writes, which are deliberately
    /// overwrite-safe for resume; `migrate_backend`'s write to a fresh
    /// backend). This method exists for exactly one call site: the sidecar's
    /// single-shot upload handler, publishing a version's *final*, canonical
    /// object at `/{file_id}/{version_id}`.
    ///
    /// # Why this must not just overwrite (P2 remediation — replay-`PUT` fix)
    /// A `PUT` token's signature covers `op`/`file_id`/`version_id`/size/hash
    /// *constraints*, never the body bytes (DESIGN.md, ADR-0003), and stays
    /// valid until `exp`. Once a version has been finalized and bound as a
    /// file's live content, a holder of that same still-unexpired token could
    /// otherwise re-`PUT` different bytes to the same backend path and
    /// silently replace the live, already-served content out from under the
    /// recorded size/hash/`ETag`/MIME — a HIGH-severity immutability break,
    /// not the merely-orphans-a-version consequence the design previously
    /// assumed. Making this call create-exclusive closes that: only the
    /// *first* write to a given path ever lands; every subsequent attempt
    /// observes `created: false` and the existing bytes are provably
    /// untouched.
    ///
    /// # This default implementation is explicitly non-atomic (TOCTOU) — read before relying on it
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
    /// **`S3Backend` is the one backend still left on this non-atomic
    /// default**, and that is a deliberate, currently-accepted gap, not an
    /// oversight: S3 support is opt-in (`s3_backends` config) and
    /// release-gated by [ADR-0005](../../../docs/ADR/0005-cpt-cf-file-storage-adr-s3-client-selection.md)
    /// (also see [ADR-0003](../../../docs/ADR/0003-cpt-cf-file-storage-adr-sidecar-data-plane.md)'s
    /// "Known gap" paragraph). Before S3 leaves that release gate for
    /// general availability, this default TOCTOU fallback **MUST** be
    /// replaced with a real atomic conditional-PUT override on `S3Backend`
    /// (e.g. an `If-None-Match: *` conditional write on the signed `PUT`,
    /// mapping the resulting `412 Precondition Failed` to the same
    /// `created: false` outcome `LocalFsBackend`/`InMemoryBackend` already
    /// produce) — shipping S3 without that override means the exclusive-
    /// publish race this method exists to close is still open for S3-backed
    /// uploads.
    async fn publish_exclusive(
        &self,
        path: &str,
        stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<PublishOutcome, DomainError> {
        use futures::StreamExt;

        let mut buf = Vec::new();
        let mut stream = stream;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| DomainError::backend(self.id(), e.to_string()))?;
            buf.extend_from_slice(&chunk);
            if max_size.is_some_and(|m| buf.len() as u64 > m) {
                return Err(DomainError::validation("size", "exceeds max_size"));
            }
        }
        let bytes_written = buf.len() as u64;
        let digest =
            crate::infra::content::hash::digest_to_array(crate::infra::content::hash::sha256(&buf));

        // Non-atomic check-then-act: see this method's doc comment for the
        // race this fallback accepts as the price of a backend-agnostic
        // default.
        if self.exists(path).await? {
            return Ok(PublishOutcome {
                bytes_written,
                digest,
                created: false,
            });
        }
        self.put(path, Bytes::from(buf)).await?;
        Ok(PublishOutcome {
            bytes_written,
            digest,
            created: true,
        })
    }

    /// Read the whole blob at `path`.
    async fn get(&self, path: &str) -> Result<Bytes, DomainError>;

    /// Stream the blob at `path` in chunks, without necessarily buffering the
    /// whole object in memory at once. Used by `finalize_upload`'s read-back
    /// verification (`cpt-cf-file-storage-fr-backend-abstraction`,
    /// memory-safety fix mirroring `put_stream`'s streaming-write bound) to
    /// recompute the actual size/hash/MIME-sniff-prefix from the real stored
    /// bytes without re-inflating a potentially huge object into memory.
    ///
    /// The default implementation falls back to `get`, yielding the whole
    /// blob as a single chunk (`futures::stream::once`) — still correct, just
    /// not memory-bounded — so every backend that hasn't been upgraded to a
    /// true streaming read stays correct; backends for which unbounded memory
    /// use during a read-back is a real concern (e.g. `LocalFsBackend`,
    /// `S3Backend`) should override this method.
    async fn get_stream(
        &self,
        path: &str,
    ) -> Result<futures::stream::BoxStream<'_, std::io::Result<Bytes>>, DomainError> {
        let bytes = self.get(path).await?;
        let stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>> =
            Box::pin(futures::stream::once(async move { Ok(bytes) }));
        Ok(stream)
    }

    /// Read a byte range of the blob at `path`. Default impl reads the whole
    /// blob then slices; range-native backends should override.
    async fn get_range(&self, path: &str, range: ByteRange) -> Result<Bytes, DomainError> {
        let full = self.get(path).await?;
        let total = full.len() as u64;
        match range.resolve(total) {
            Some((start, end)) => {
                let s = usize::try_from(start).unwrap_or(usize::MAX);
                let e = usize::try_from(end).unwrap_or(usize::MAX);
                Ok(full.slice(s..=e.min(full.len().saturating_sub(1))))
            }
            None => Err(DomainError::validation("range", "unsatisfiable byte range")),
        }
    }

    /// The total length in bytes of the blob at `path`, without necessarily
    /// reading its content. Range-aware callers (e.g. the sidecar's
    /// `download` handler, P2 1.11) use this to resolve `Range` requests
    /// against the actual blob length and to build a correct `Content-Range`
    /// header, without materializing the whole blob first.
    ///
    /// The default implementation falls back to `get`, so only backends with
    /// a cheaper standalone stat (e.g. `LocalFsBackend`'s filesystem
    /// metadata) need to override it.
    async fn size(&self, path: &str) -> Result<u64, DomainError> {
        Ok(self.get(path).await?.len() as u64)
    }

    /// Delete the blob at `path`. Missing blobs are treated as success
    /// (idempotent delete).
    async fn delete(&self, path: &str) -> Result<(), DomainError>;

    /// Whether a blob exists at `path`.
    async fn exists(&self, path: &str) -> Result<bool, DomainError>;

    /// Initiate a multipart upload for `path`. Returns an opaque backend handle.
    /// Default returns an error — backends must opt-in by overriding this method
    /// and setting `multipart_native: true` in their capabilities.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    async fn initiate_multipart(&self, _path: &str) -> Result<String, DomainError> {
        Err(DomainError::multipart_not_supported(self.id()))
    }

    /// Upload one part. Returns `(backend_etag, part_hash_bytes)`.
    ///
    /// `part_offset` is the part's start byte offset within the assembled
    /// object (ADR-0006). It is not used to hash the part — `part_hash` is a
    /// flat `sha256(data)` exactly as before — but is threaded through so the
    /// backend can build the offset-manifest at `complete` time without
    /// re-deriving it from a plan it may not retain.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    async fn upload_part(
        &self,
        _path: &str,
        _upload_handle: &str,
        _part_number: u32,
        _part_offset: u64,
        _data: Bytes,
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
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    async fn complete_multipart(
        &self,
        _path: &str,
        _upload_handle: &str,
        _parts: &[MultipartCompletionPart],
    ) -> Result<(Manifest, [u8; 32]), DomainError> {
        Err(DomainError::multipart_not_supported(self.id()))
    }

    /// Abort a multipart upload, discarding all uploaded parts.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    async fn abort_multipart(&self, _path: &str, _upload_handle: &str) -> Result<(), DomainError> {
        Err(DomainError::multipart_not_supported(self.id()))
    }

    /// Enumerate all object paths stored by this backend (for orphan
    /// reconciliation). Returns paths in the same format they are stored in
    /// `file_versions.backend_path` (e.g. `"/{file_id}/{version_id}"`).
    ///
    /// The default implementation returns an empty vec — backends that cannot
    /// enumerate their contents are treated conservatively (unknown = skip).
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    async fn list_paths(&self) -> Result<Vec<String>, DomainError> {
        Ok(vec![])
    }

    /// Cheap readiness probe (P2 1.6): confirms the backend can actually
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
    /// sidecar's `/readyz` probe (P2 1.6), which polls every backend's
    /// [`StorageBackend::is_ready`] rather than just the default one.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Arc<dyn StorageBackend>)> {
        self.backends.iter().map(|(id, b)| (id.as_str(), b))
    }
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod backend_tests;
