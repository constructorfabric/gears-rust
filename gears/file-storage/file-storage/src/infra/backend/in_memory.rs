//! In-memory (non-durable) storage backend for tests and ephemeral deployments, with native
//! multipart upload.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use uuid::Uuid;

use file_storage_sdk::ByteRange;

use crate::domain::error::DomainError;
use crate::infra::content::hash;
use crate::infra::content::hash_mode::Manifest;

use super::{
    BackendCapabilities, MultipartCompletionPart, StorageBackend, build_manifest_and_root,
};

/// In-progress multipart state per handle: (blob path, ordered parts).
type MultipartMap = HashMap<String, (String, BTreeMap<u32, Bytes>)>;

/// In-memory blob store with multipart upload support.
pub struct InMemoryBackend {
    id: String,
    blobs: Mutex<HashMap<String, Bytes>>,
    /// In-progress multipart state: handle -> (path, parts in order).
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
            range_native: true,
            // `durable` stays `false`: content is lost on restart.
            ..BackendCapabilities::default()
        }
    }

    async fn put(&self, path: &str, bytes: Bytes) -> Result<(), DomainError> {
        self.lock_blobs()?.insert(path.to_owned(), bytes);
        Ok(())
    }

    /// Buffers the stream: acceptable for non-durable test/dev storage.
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

    async fn get(&self, path: &str) -> Result<Bytes, DomainError> {
        self.lock_blobs()?
            .get(path)
            .cloned()
            .ok_or_else(|| DomainError::backend(&self.id, format!("blob not found: {path}")))
    }

    /// Yields the stored `Bytes` as a single chunk.
    async fn get_stream(
        &self,
        path: &str,
    ) -> Result<BoxStream<'_, std::io::Result<Bytes>>, DomainError> {
        let bytes = self.get(path).await?;
        Ok(Box::pin(futures::stream::once(async move { Ok(bytes) })))
    }

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

    async fn size(&self, path: &str) -> Result<u64, DomainError> {
        Ok(self.get(path).await?.len() as u64)
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
        // Assemble in ascending part_number order (BTreeMap). The hash is the manifest root
        // built from the caller's per-part digests, not rehashed from these bytes.
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

    async fn list_paths(&self) -> Result<Vec<String>, DomainError> {
        let paths = self.lock_blobs()?.keys().cloned().collect();
        Ok(paths)
    }
}
