//! Local filesystem storage backend
//! (`cpt-cf-file-storage-fr-backend-abstraction`).
//!
//! Blobs are stored at `<root>/<sanitized-path>`. The opaque path
//! (`/{file_id}/{version_id}`) is sanitized to prevent traversal outside root.

use std::path::PathBuf;

use async_trait::async_trait;
use bytes::Bytes;
use file_storage_sdk::ByteRange;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::domain::error::DomainError;

use super::{BackendCapabilities, StorageBackend};

/// Filesystem-backed blob store rooted at a configured directory.
pub struct LocalFsBackend {
    id: String,
    root: PathBuf,
}

impl LocalFsBackend {
    #[must_use]
    pub fn new(id: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            root: root.into(),
        }
    }

    /// Map an opaque backend path to a concrete file path under `root`, rejecting
    /// any component that could escape the root (`..`, absolute, etc.).
    fn resolve(&self, path: &str) -> Result<PathBuf, DomainError> {
        let mut out = self.root.clone();
        for comp in path.split('/').filter(|c| !c.is_empty()) {
            if comp == ".." || comp == "." || comp.contains('\\') {
                return Err(DomainError::backend(&self.id, "illegal path component"));
            }
            out.push(comp);
        }
        // The resolved path must still be under root.
        if !out.starts_with(&self.root) {
            return Err(DomainError::backend(&self.id, "path escapes backend root"));
        }
        Ok(out)
    }

    fn io_err(&self, e: impl std::fmt::Display) -> DomainError {
        DomainError::backend(&self.id, e.to_string())
    }
}

#[async_trait]
impl StorageBackend for LocalFsBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            range_native: true,
            ..BackendCapabilities::default()
        }
    }

    async fn put(&self, path: &str, bytes: Bytes) -> Result<(), DomainError> {
        let target = self.resolve(path)?;
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| self.io_err(e))?;
        }
        tokio::fs::write(&target, &bytes)
            .await
            .map_err(|e| self.io_err(e))
    }

    async fn get(&self, path: &str) -> Result<Bytes, DomainError> {
        let target = self.resolve(path)?;
        let data = tokio::fs::read(&target).await.map_err(|e| self.io_err(e))?;
        Ok(Bytes::from(data))
    }

    /// Native range read: seek to the requested offset and read only the
    /// requested bytes, never materializing the whole blob.
    async fn get_range(&self, path: &str, range: ByteRange) -> Result<Bytes, DomainError> {
        let target = self.resolve(path)?;
        let mut file = tokio::fs::File::open(&target)
            .await
            .map_err(|e| self.io_err(e))?;
        let total = file.metadata().await.map_err(|e| self.io_err(e))?.len();
        let Some((start, end)) = range.resolve(total) else {
            return Err(DomainError::validation("range", "unsatisfiable byte range"));
        };
        // `resolve` yields an inclusive end; clamp defensively against `total`.
        let end = end.min(total.saturating_sub(1));
        // Fail cleanly on an oversized range instead of asking the allocator for
        // `usize::MAX` (which would turn it into an OOM/panic path).
        let len = usize::try_from(end - start + 1)
            .map_err(|_| DomainError::validation("range", "requested byte range is too large"))?;
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|e| self.io_err(e))?;
        let mut buf = vec![0u8; len];
        file.read_exact(&mut buf)
            .await
            .map_err(|e| self.io_err(e))?;
        Ok(Bytes::from(buf))
    }

    async fn delete(&self, path: &str) -> Result<(), DomainError> {
        let target = self.resolve(path)?;
        match tokio::fs::remove_file(&target).await {
            Ok(()) => Ok(()),
            // Idempotent: a missing blob is a successful delete.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(self.io_err(e)),
        }
    }

    async fn exists(&self, path: &str) -> Result<bool, DomainError> {
        let target = self.resolve(path)?;
        // Only a genuine "not found" means absent; permission/IO errors are real
        // failures and must not be silently reported as a missing blob.
        match tokio::fs::metadata(&target).await {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(self.io_err(e)),
        }
    }

    /// Walk the root directory recursively and return all file paths as
    /// backend-relative paths in the form `"/{component}/{component}"`.
    ///
    /// Non-existent root (fresh install with no uploads yet) returns an empty
    /// vec rather than an error.
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    async fn list_paths(&self) -> Result<Vec<String>, DomainError> {
        // If the root does not exist yet (no blobs written), return empty.
        match tokio::fs::metadata(&self.root).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(self.io_err(e)),
        }

        let mut paths = Vec::new();
        let mut stack = vec![self.root.clone()];

        while let Some(dir) = stack.pop() {
            let mut entries = tokio::fs::read_dir(&dir)
                .await
                .map_err(|e| self.io_err(e))?;

            while let Some(entry) = entries.next_entry().await.map_err(|e| self.io_err(e))? {
                let ft = entry.file_type().await.map_err(|e| self.io_err(e))?;
                if ft.is_dir() {
                    stack.push(entry.path());
                } else if ft.is_file() {
                    // Strip the root prefix and convert OS separator to '/'.
                    let abs = entry.path();
                    if let Ok(rel) = abs.strip_prefix(&self.root) {
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        paths.push(format!("/{rel_str}"));
                    }
                }
            }
        }

        Ok(paths)
    }
}
