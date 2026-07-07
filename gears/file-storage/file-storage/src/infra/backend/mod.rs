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

pub use in_memory::InMemoryBackend;
pub use local_fs::LocalFsBackend;
pub use s3::S3Backend;

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

    /// Read the whole blob at `path`.
    async fn get(&self, path: &str) -> Result<Bytes, DomainError>;

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
    /// @cpt-cf-file-storage-fr-multipart-upload
    async fn upload_part(
        &self,
        _path: &str,
        _upload_handle: &str,
        _part_number: u32,
        _data: Bytes,
    ) -> Result<(String, Vec<u8>), DomainError> {
        Err(DomainError::multipart_not_supported(self.id()))
    }

    /// Complete a multipart upload, assembling all uploaded parts in order.
    ///
    /// Returns the SHA-256 digest of the fully assembled object, so the control
    /// plane can persist the hash of the *actual* stored bytes (matching what
    /// `get`/`migrate_backend` would recompute) rather than a hash derived from
    /// the individual part digests.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    async fn complete_multipart(
        &self,
        _path: &str,
        _upload_handle: &str,
        _parts: &[(u32, String)],
    ) -> Result<Vec<u8>, DomainError> {
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
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod backend_tests;
