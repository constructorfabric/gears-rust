//! In-memory storage backend — a real backend *type* for tests and ephemeral
//! deployments. Content lives in a `Mutex<HashMap>` keyed by path.
//!
//! Implements multipart upload natively (`multipart_native: true`).

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;
use bytes::Bytes;
use file_storage_sdk::ByteRange;
use futures::StreamExt;
use futures::stream::BoxStream;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::infra::content::hash;
use crate::infra::content::hash_mode::Manifest;

use super::{
    BackendCapabilities, MultipartCompletionPart, PublishOutcome, StorageBackend,
    build_manifest_and_root, check_read_prefix_budget,
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

    /// Private whole-object read helper. `get`/`put` no longer exist on
    /// `StorageBackend` (production backends must not read/write whole
    /// objects), but this backend is explicitly non-durable, in-process
    /// storage for tests/dev deployments, not a memory-DoS surface worth
    /// hardening: the whole object already lives in memory as one `Bytes`
    /// under `self.blobs`, so this helper "buffers" nothing that isn't
    /// already sitting there.
    fn get_whole(&self, path: &str) -> Result<Bytes, DomainError> {
        self.lock_blobs()?
            .get(path)
            .cloned()
            .ok_or_else(|| DomainError::backend(&self.id, format!("blob not found: {path}")))
    }

    /// Private whole-object range-slice helper, built on [`Self::get_whole`]
    /// for the same non-hardening reason.
    fn get_range_whole(&self, path: &str, range: ByteRange) -> Result<Bytes, DomainError> {
        let full = self.get_whole(path)?;
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

    /// Create-exclusive publish. The existence check and the insert happen
    /// under the same `blobs` lock guard, so no concurrent
    /// `publish_exclusive`/`put` call can interleave between them — unlike
    /// the trait's default TOCTOU fallback. See
    /// [`StorageBackend::publish_exclusive`] for the full contract.
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

    /// Read up to `max_bytes` from the start of the stored blob, if any.
    /// This backend already holds the whole object in memory as one
    /// `Bytes`, so slicing its prefix costs nothing extra beyond what
    /// `get_whole` itself already holds -- see its own doc comment for why
    /// that's an acceptable non-hardened shortcut for this specific backend.
    async fn read_prefix(&self, path: &str, max_bytes: u64) -> Result<Option<Bytes>, DomainError> {
        check_read_prefix_budget(max_bytes)?;
        let blobs = self.lock_blobs()?;
        Ok(blobs.get(path).map(|b| {
            let n = usize::try_from(max_bytes)
                .unwrap_or(usize::MAX)
                .min(b.len());
            b.slice(0..n)
        }))
    }

    /// Yields the stored `Bytes` as a single chunk: this backend is
    /// explicitly non-durable, in-process storage for tests/dev deployments,
    /// not a memory-DoS surface worth hardening — the override exists so the
    /// shared backend contract tests can run identically against every
    /// backend, not just `LocalFsBackend`/`S3Backend`. Still verifies
    /// `expected_len` against the blob it actually reads under the lock, the
    /// same contract every other backend enforces — see
    /// [`StorageBackend::get_stream`]'s doc comment.
    async fn get_stream(
        &self,
        path: &str,
        expected_len: u64,
    ) -> Result<BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        let bytes = self.get_whole(path)?;
        let actual_len = bytes.len() as u64;
        if actual_len != expected_len {
            return Err(DomainError::conflict(format!(
                "object at '{path}' changed size before it could be read: expected {expected_len} byte(s), found {actual_len}"
            )));
        }
        Ok(Box::pin(futures::stream::once(async move { Ok(bytes) })))
    }

    /// Yields the resolved range as a single chunk — same non-hardening
    /// rationale as `get_stream`'s override: this backend is explicitly
    /// non-durable, in-process storage for tests/dev deployments, so a
    /// one-chunk stream (via the trait's own `get_range` for the actual
    /// slicing) is enough to let the shared backend contract tests exercise
    /// `get_range_stream` against every backend, not just
    /// `LocalFsBackend`/`S3Backend`. Still verifies `expected_len` against
    /// the resolved range, mirroring `get_stream`'s check above.
    async fn get_range_stream(
        &self,
        path: &str,
        range: ByteRange,
        expected_len: u64,
    ) -> Result<BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        let bytes = self.get_range_whole(path, range)?;
        let actual_len = bytes.len() as u64;
        if actual_len != expected_len {
            return Err(DomainError::conflict(format!(
                "object at '{path}' range changed before it could be read: expected {expected_len} byte(s), resolved {actual_len}"
            )));
        }
        Ok(Box::pin(futures::stream::once(async move { Ok(bytes) })))
    }

    async fn delete(&self, path: &str) -> Result<(), DomainError> {
        self.lock_blobs()?.remove(path);
        Ok(())
    }

    async fn exists(&self, path: &str) -> Result<bool, DomainError> {
        Ok(self.lock_blobs()?.contains_key(path))
    }

    async fn size(&self, path: &str) -> Result<u64, DomainError> {
        self.lock_blobs()?
            .get(path)
            .map(|b| b.len() as u64)
            .ok_or_else(|| DomainError::backend(&self.id, format!("blob not found: {path}")))
    }

    /// Native combined stat: one lock acquisition instead of the
    /// default's `exists` (lock + lookup) followed by `size` (another lock +
    /// `get`, which for this backend would otherwise clone the whole blob
    /// just to measure it).
    async fn stat(&self, path: &str) -> Result<Option<u64>, DomainError> {
        Ok(self.lock_blobs()?.get(path).map(|b| b.len() as u64))
    }

    async fn initiate_multipart(&self, path: &str) -> Result<String, DomainError> {
        let handle = format!("{}-{}", path, Uuid::now_v7());
        self.lock_multipart()?
            .insert(handle.clone(), (path.to_owned(), BTreeMap::new()));
        Ok(handle)
    }

    /// Collects the stream into a single buffer before storing it: this
    /// backend is explicitly non-durable, in-process storage for tests/dev
    /// deployments, not a memory-DoS surface worth hardening (same rationale
    /// as this backend's `put_stream`/`publish_exclusive` overrides above).
    /// Still enforces the trait's exact-length contract on `len` — an
    /// implementation is required to, regardless of durability — so tests
    /// exercising a short/long part stream get the same rejection shape a
    /// real backend (`S3Backend`) would give.
    async fn upload_part_stream(
        &self,
        _path: &str,
        upload_handle: &str,
        part_number: u32,
        _part_offset: u64,
        mut stream: BoxStream<'static, std::io::Result<Bytes>>,
        len: u64,
    ) -> Result<(String, Vec<u8>), DomainError> {
        let mut buf = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| DomainError::backend(&self.id, e.to_string()))?;
            buf.extend_from_slice(&chunk);
            if buf.len() as u64 > len {
                return Err(DomainError::validation(
                    "size",
                    format!("part stream exceeded the declared length {len}"),
                ));
            }
        }
        if buf.len() as u64 != len {
            return Err(DomainError::validation(
                "size",
                format!(
                    "part stream yielded {} byte(s), expected exactly {len}",
                    buf.len()
                ),
            ));
        }
        let data = Bytes::from(buf);
        let hash_bytes = hash::sha256(&data);
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

        build_manifest_and_root(parts)
    }

    async fn abort_multipart(&self, _path: &str, upload_handle: &str) -> Result<(), DomainError> {
        self.lock_multipart()?.remove(upload_handle);
        Ok(())
    }

    /// Returns all blob paths currently in the store.
    async fn list_paths(&self) -> Result<Vec<String>, DomainError> {
        let paths = self.lock_blobs()?.keys().cloned().collect();
        Ok(paths)
    }
}
