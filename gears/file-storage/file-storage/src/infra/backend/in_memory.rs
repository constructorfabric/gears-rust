//! In-memory storage backend — a real backend *type* for tests and ephemeral
//! deployments. Content lives in a `Mutex<HashMap>` keyed by path.
//!
//! P2-M3: implements multipart upload natively (`multipart_native: true`).

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;
use bytes::Bytes;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::infra::content::hash;

use super::{BackendCapabilities, StorageBackend};

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
            ..BackendCapabilities::default()
        }
    }

    async fn put(&self, path: &str, bytes: Bytes) -> Result<(), DomainError> {
        self.lock_blobs()?.insert(path.to_owned(), bytes);
        Ok(())
    }

    async fn get(&self, path: &str) -> Result<Bytes, DomainError> {
        self.lock_blobs()?
            .get(path)
            .cloned()
            .ok_or_else(|| DomainError::backend(&self.id, format!("blob not found: {path}")))
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
        _parts: &[(u32, String)],
    ) -> Result<Vec<u8>, DomainError> {
        let (final_path, parts_map) = {
            let mut mp = self.lock_multipart()?;
            mp.remove(upload_handle).ok_or_else(|| {
                DomainError::backend(
                    &self.id,
                    format!("multipart handle not found: {upload_handle}"),
                )
            })?
        };
        // Assemble parts in ascending part_number order (BTreeMap iterates sorted).
        let mut assembled = Vec::new();
        for (_, part_data) in parts_map {
            assembled.extend_from_slice(&part_data);
        }
        // Hash the assembled object so the caller stores the digest of the bytes
        // actually persisted (consistent with `get` + integrity recomputes).
        let digest = hash::sha256(&assembled);
        self.lock_blobs()?
            .insert(final_path, Bytes::from(assembled));
        Ok(digest)
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
