//! In-memory storage backend — a real backend *type* for tests and ephemeral
//! deployments. Content lives in a `Mutex<HashMap>` keyed by path.
//!
//! P2-M3: implements multipart upload natively (`multipart_native: true`).

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::infra::content::hash;
use crate::infra::content::hash_mode::Manifest;

use super::{
    BackendCapabilities, MultipartCompletionPart, PublishOutcome, StorageBackend,
    build_manifest_and_root,
};

/// In-progress multipart state per handle: (blob path, ordered parts).
type MultipartMap = HashMap<String, (String, BTreeMap<u32, Bytes>)>;

/// In-memory blob store with multipart upload support.
pub struct InMemoryBackend {
    id: String,
    blobs: Mutex<HashMap<String, Bytes>>,
    /// In-progress multipart state: handle → (path, parts in order)
    multipart: Mutex<MultipartMap>,
}

impl InMemoryBackend {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            blobs: Mutex::new(HashMap::new()),
            multipart: Mutex::new(HashMap::new()),
        }
    }

    fn lock_blobs(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, Bytes>>, DomainError> {
        self.blobs
            .lock()
            .map_err(|_| DomainError::backend("in-memory", "poisoned lock (blobs)"))
    }

    fn lock_multipart(&self) -> Result<std::sync::MutexGuard<'_, MultipartMap>, DomainError> {
        self.multipart
            .lock()
            .map_err(|_| DomainError::backend("in-memory", "poisoned lock (multipart)"))
    }
}

#[async_trait]
impl StorageBackend for InMemoryBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            multipart_native: true,
            range_native: false,
            // Intentionally left on the `BackendCapabilities::default()`
            // value of `false`: content lives only in process memory and is
            // lost on restart/crash, so `migrate_backend` must treat this as
            // non-durable.
            ..BackendCapabilities::default()
        }
    }

    async fn put(&self, path: &str, bytes: Bytes) -> Result<(), DomainError> {
        self.lock_blobs()?.insert(path.to_owned(), bytes);
        Ok(())
    }

    /// Collecting into a `Bytes` buffer is acceptable here: this backend is
    /// explicitly non-durable, in-process storage for tests/dev deployments,
    /// not a memory-DoS surface worth hardening. The override exists so the
    /// shared backend contract tests (`local_fs_put_stream_*` and friends)
    /// can run identically against every backend, not just `LocalFsBackend`.
    async fn put_stream(
        &self,
        path: &str,
        mut stream: BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<(u64, [u8; 32]), DomainError> {
        let mut buf = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| DomainError::backend(&self.id, e.to_string()))?;
            buf.extend_from_slice(&chunk);
            if max_size.is_some_and(|m| buf.len() as u64 > m) {
                return Err(DomainError::validation("size", "exceeds max_size"));
            }
        }
        let bytes_written = buf.len() as u64;
        let digest = hash::digest_to_array(hash::sha256(&buf));
        self.lock_blobs()?.insert(path.to_owned(), Bytes::from(buf));
        Ok((bytes_written, digest))
    }

    /// Create-exclusive publish (P2 remediation — replay-`PUT` overwrite
    /// fix). The existence check and the insert happen under the same
    /// `blobs` lock guard, so no concurrent `publish_exclusive`/`put` call
    /// can interleave between them — unlike the trait's default TOCTOU
    /// fallback. See [`StorageBackend::publish_exclusive`] for the full
    /// contract.
    async fn publish_exclusive(
        &self,
        path: &str,
        mut stream: BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<PublishOutcome, DomainError> {
        let mut buf = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| DomainError::backend(&self.id, e.to_string()))?;
            buf.extend_from_slice(&chunk);
            if max_size.is_some_and(|m| buf.len() as u64 > m) {
                return Err(DomainError::validation("size", "exceeds max_size"));
            }
        }
        let bytes_written = buf.len() as u64;
        let digest = hash::digest_to_array(hash::sha256(&buf));

        let mut blobs = self.lock_blobs()?;
        if blobs.contains_key(path) {
            return Ok(PublishOutcome {
                bytes_written,
                digest,
                created: false,
            });
        }
        blobs.insert(path.to_owned(), Bytes::from(buf));
        Ok(PublishOutcome {
            bytes_written,
            digest,
            created: true,
        })
    }

    async fn get(&self, path: &str) -> Result<Bytes, DomainError> {
        self.lock_blobs()?
            .get(path)
            .cloned()
            .ok_or_else(|| DomainError::backend(&self.id, format!("blob not found: {path}")))
    }

    /// Yields the stored `Bytes` as a single chunk: this backend is
    /// explicitly non-durable, in-process storage for tests/dev deployments,
    /// not a memory-DoS surface worth hardening — the override exists so the
    /// shared backend contract tests can run identically against every
    /// backend, not just `LocalFsBackend`/`S3Backend`.
    async fn get_stream(
        &self,
        path: &str,
    ) -> Result<BoxStream<'_, std::io::Result<Bytes>>, DomainError> {
        let bytes = self.get(path).await?;
        Ok(Box::pin(futures::stream::once(async move { Ok(bytes) })))
    }

    async fn delete(&self, path: &str) -> Result<(), DomainError> {
        self.lock_blobs()?.remove(path);
        Ok(())
    }

    async fn exists(&self, path: &str) -> Result<bool, DomainError> {
        Ok(self.lock_blobs()?.contains_key(path))
    }

    async fn initiate_multipart(&self, path: &str) -> Result<String, DomainError> {
        let handle = format!("{}-{}", path, Uuid::now_v7());
        self.lock_multipart()?
            .insert(handle.clone(), (path.to_owned(), BTreeMap::new()));
        Ok(handle)
    }

    async fn upload_part(
        &self,
        _path: &str,
        upload_handle: &str,
        part_number: u32,
        _part_offset: u64,
        data: Bytes,
    ) -> Result<(String, Vec<u8>), DomainError> {
        // @cpt-begin:cpt-cf-file-storage-flow-multipart-upload-part:p1:inst-part-hash
        let hash_bytes = hash::sha256(&data);
        // @cpt-end:cpt-cf-file-storage-flow-multipart-upload-part:p1:inst-part-hash
        let etag = hex::encode(&hash_bytes);

        let mut mp = self.lock_multipart()?;
        let entry = mp.get_mut(upload_handle).ok_or_else(|| {
            DomainError::backend(
                &self.id,
                format!("multipart handle not found: {upload_handle}"),
            )
        })?;
        entry.1.insert(part_number, data);
        Ok((etag, hash_bytes))
    }

    async fn complete_multipart(
        &self,
        _path: &str,
        upload_handle: &str,
        parts: &[MultipartCompletionPart],
    ) -> Result<(Manifest, [u8; 32]), DomainError> {
        let (final_path, parts_map) = {
            let mut mp = self.lock_multipart()?;
            mp.remove(upload_handle).ok_or_else(|| {
                DomainError::backend(
                    &self.id,
                    format!("multipart handle not found: {upload_handle}"),
                )
            })?
        };
        // Assemble parts in ascending part_number order (BTreeMap iterates
        // sorted) into the blob store so `get` returns the whole object. The
        // stored **hash** is no longer derived from these assembled bytes —
        // per ADR-0006 mode 2 it is the offset-manifest root built from the
        // per-part digests the caller already collected, so completing a
        // multipart upload never rehashes the assembled object.
        let mut assembled = Vec::new();
        for (_, part_data) in parts_map {
            assembled.extend_from_slice(&part_data);
        }
        self.lock_blobs()?
            .insert(final_path, Bytes::from(assembled));

        // @cpt-cf-file-storage-algo-content-hash-modes-build-manifest
        build_manifest_and_root(parts)
    }

    async fn abort_multipart(&self, _path: &str, upload_handle: &str) -> Result<(), DomainError> {
        self.lock_multipart()?.remove(upload_handle);
        Ok(())
    }

    /// Returns all blob paths currently in the store.
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    async fn list_paths(&self) -> Result<Vec<String>, DomainError> {
        let paths = self.lock_blobs()?.keys().cloned().collect();
        Ok(paths)
    }
}
