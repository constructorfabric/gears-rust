//! Pluggable storage-backend abstraction.
//!
//! A backend stores immutable content blobs keyed by an opaque path
//! (`/{file_id}/{version_id}` by convention). Clients never address a backend
//! directly; content moves only through the sidecar. Shipped backends: local
//! filesystem, in-memory and S3.

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

/// One part of a multipart completion: `(part_number, offset, part_hash, backend_etag)`.
pub type MultipartCompletionPart = (u32, u64, [u8; 32], String);

/// Build the offset-manifest and its `root` from `complete_multipart` parts (ADR-0006).
///
/// Shared by all multipart backends so the canonical wire format is produced in one place.
/// Entries are sorted by ascending offset, so out-of-order parts still yield the canonical
/// manifest.
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

/// Optional features a backend may declare. Versioning is not here: it is implemented at
/// the `FileStorage` level on every backend.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BackendCapabilities {
    /// Native chunked upload with server-side assembly.
    pub multipart_native: bool,
    /// Server-side encryption at rest.
    pub encryption_native: bool,
    /// Native byte-range reads (every backend implements them).
    pub range_native: bool,
    /// Internal-only presigned URLs (backend-to-backend tooling); never exposed.
    pub presigned_url_internal: bool,
    /// Maximum blob size the backend accepts in bytes. `None` = unbounded.
    pub max_size_bytes: Option<u64>,
    /// Whether content survives process restarts (`false` for the in-memory backend).
    /// `migrate_backend` gates moves onto a non-durable backend behind an elevated scope.
    pub durable: bool,
}

/// A storage backend: moves immutable content blobs, keyed by an opaque backend path.
///
/// Paths are produced by the gear, never by clients; implementations must still keep every
/// access inside their own root/bucket (no `..` or absolute-path escapes).
/// `get_range` and `size` are required (no whole-blob fallbacks).
#[async_trait]
pub trait StorageBackend: Send + Sync {
    /// Stable backend identifier (matches `file_versions.backend_id`).
    fn id(&self) -> &str;

    /// The capabilities this backend advertises.
    fn capabilities(&self) -> BackendCapabilities;

    /// Write a blob at `path`. Overwrites are allowed (each version uses a fresh path).
    async fn put(&self, path: &str, bytes: Bytes) -> Result<(), DomainError>;

    /// Stream a blob into `path`, hashing incrementally and enforcing `max_size` as bytes
    /// arrive. Returns `(bytes_written, sha256_digest)`.
    ///
    /// The default buffers the whole stream in memory (still enforcing `max_size`) and
    /// delegates to `put`; backends where that matters (e.g. `LocalFsBackend`) override it.
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

    /// Stream the blob at `path` in chunks.
    ///
    /// The default falls back to `get` and yields one chunk; backends with large objects
    /// (e.g. `LocalFsBackend`, `S3Backend`) override it to avoid buffering.
    async fn get_stream(
        &self,
        path: &str,
    ) -> Result<futures::stream::BoxStream<'_, std::io::Result<Bytes>>, DomainError> {
        let bytes = self.get(path).await?;
        let stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>> =
            Box::pin(futures::stream::once(async move { Ok(bytes) }));
        Ok(stream)
    }

    /// Read a byte range of the blob at `path` without materializing the whole blob.
    /// An unsatisfiable range is a validation error.
    async fn get_range(&self, path: &str, range: ByteRange) -> Result<Bytes, DomainError>;

    /// Total length in bytes of the blob at `path` (a metadata-only HEAD, no content read).
    /// Used to resolve `Range` requests, build `Content-Range`, and check the size at finalize.
    async fn size(&self, path: &str) -> Result<u64, DomainError>;

    /// Delete the blob at `path`; a missing blob is success (idempotent).
    async fn delete(&self, path: &str) -> Result<(), DomainError>;

    /// Whether a blob exists at `path`.
    async fn exists(&self, path: &str) -> Result<bool, DomainError>;

    /// Initiate a multipart upload for `path`, returning an opaque backend handle.
    /// Default is an error; backends opt in and set `multipart_native: true`.
    async fn initiate_multipart(&self, _path: &str) -> Result<String, DomainError> {
        Err(DomainError::multipart_not_supported(self.id()))
    }

    /// Upload one part. Returns `(backend_etag, part_hash_bytes)`.
    ///
    /// `part_offset` is the part's start offset in the assembled object. The part hash is a
    /// flat `sha256(data)`; the offset is passed through for the offset-manifest (ADR-0006).
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
    /// The backend MUST build the manifest and `root` from `parts` (ADR-0006) rather than
    /// re-reading the assembled object.
    ///
    /// Returns `(manifest, root)` with `root = sha256(manifest.to_wire_string())`; the
    /// control plane stores `root` as the version's `hash_value`.
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

    /// Enumerate all stored paths in the `file_versions.backend_path` format (for orphan
    /// reconciliation). The default is empty: backends that cannot enumerate are skipped.
    async fn list_paths(&self) -> Result<Vec<String>, DomainError> {
        Ok(vec![])
    }

    /// Cheap readiness probe for the sidecar's `/readyz` (e.g. root mounted, S3 reachable),
    /// without moving content. The default is always ready.
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
        // Fail fast on a duplicate id instead of silently dropping a backend.
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

    /// Iterate all configured backends as `(id, backend)` pairs (used by `/readyz`).
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Arc<dyn StorageBackend>)> {
        self.backends.iter().map(|(id, b)| (id.as_str(), b))
    }
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod backend_tests;
