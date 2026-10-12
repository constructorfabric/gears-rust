//! Local filesystem storage backend.
//!
//! Blobs live at `<root>/<sanitized-path>`; the opaque path is sanitized to prevent traversal
//! outside root.
//!
//! Writes never touch the target directly: bytes go to a sibling temp file
//! (`<target>.tmp.<uuid>`, same filesystem), which is fsynced and then atomically renamed
//! onto the target, so readers never see a partial file. The parent directory is then fsynced
//! best-effort (needed on ext4/xfs for the rename to survive a crash); if that fails, a
//! warning is logged and the write still succeeds.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytes::Bytes;
use file_storage_sdk::ByteRange;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::infra::content::hash;

use super::{BackendCapabilities, StorageBackend};

/// Filesystem-backed blob store rooted at a configured directory.
pub struct LocalFsBackend {
    id: String,
    root: PathBuf,
    fsync_parent_dir: bool,
}

impl LocalFsBackend {
    #[must_use]
    pub fn new(id: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            root: root.into(),
            fsync_parent_dir: true,
        }
    }

    /// Enable/disable the best-effort parent-directory fsync after rename (default `true`).
    #[must_use]
    pub fn with_fsync_parent_dir(mut self, enabled: bool) -> Self {
        self.fsync_parent_dir = enabled;
        self
    }

    /// Map an opaque backend path to a file path under `root`, rejecting components that
    /// could escape it (`..`, `.`, backslashes).
    fn resolve(&self, path: &str) -> Result<PathBuf, DomainError> {
        let mut out = self.root.clone();
        for comp in path.split('/').filter(|c| !c.is_empty()) {
            if comp == ".." || comp == "." || comp.contains('\\') {
                return Err(DomainError::backend(&self.id, "illegal path component"));
            }
            out.push(comp);
        }
        if !out.starts_with(&self.root) {
            return Err(DomainError::backend(&self.id, "path escapes backend root"));
        }
        Ok(out)
    }

    fn io_err(&self, e: impl std::fmt::Display) -> DomainError {
        DomainError::backend(&self.id, e.to_string())
    }

    /// Best-effort directory fsync (flushes directory entries such as a rename).
    async fn fsync_dir(&self, dir: &std::path::Path) -> std::io::Result<()> {
        let dir_handle = tokio::fs::File::open(dir).await?;
        dir_handle.sync_all().await
    }

    /// Resolve `path` to its target file and create the parent directory.
    async fn prepare_target(&self, path: &str) -> Result<(PathBuf, Option<PathBuf>), DomainError> {
        let target = self.resolve(path)?;
        let parent = target.parent().map(Path::to_path_buf);
        if let Some(parent) = &parent {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| self.io_err(e))?;
        }
        Ok((target, parent))
    }

    /// A unique sibling temp-file path for `target`.
    fn tmp_path_for(target: &Path) -> PathBuf {
        PathBuf::from(format!("{}.tmp.{}", target.display(), Uuid::now_v7()))
    }

    /// Rename a written and fsynced temp file onto `target`, then fsync the parent
    /// best-effort (see module docs).
    async fn publish_tmp(
        &self,
        tmp: &Path,
        target: &Path,
        parent: Option<&Path>,
    ) -> Result<(), DomainError> {
        tokio::fs::rename(tmp, target)
            .await
            .map_err(|e| self.io_err(e))?;

        if self.fsync_parent_dir
            && let Some(parent) = parent
            && let Err(e) = self.fsync_dir(parent).await
        {
            tracing::warn!(
                error = ?e,
                "parent-dir fsync failed or unsupported by this filesystem, continuing"
            );
        }

        Ok(())
    }

    /// Stream chunks into `tmp`, hashing incrementally and aborting as soon as the byte count
    /// exceeds `max_size`. The caller removes `tmp` on error and publishes it on success.
    async fn write_stream_to_tmp(
        &self,
        tmp: &Path,
        mut stream: BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<(u64, [u8; 32]), DomainError> {
        let mut file = tokio::fs::File::create(tmp)
            .await
            .map_err(|e| self.io_err(e))?;
        let mut hasher = hash::Hasher::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| self.io_err(e))?;
            file.write_all(&chunk).await.map_err(|e| self.io_err(e))?;
            hasher.update(&chunk);
            if max_size.is_some_and(|m| hasher.len() > m) {
                return Err(DomainError::validation("size", "exceeds max_size"));
            }
        }
        file.sync_all().await.map_err(|e| self.io_err(e))?;
        let bytes_written = hasher.len();
        let digest = hash::digest_to_array(hasher.finalize());
        Ok((bytes_written, digest))
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
            durable: true,
            ..BackendCapabilities::default()
        }
    }

    async fn put(&self, path: &str, bytes: Bytes) -> Result<(), DomainError> {
        let (target, parent) = self.prepare_target(path).await?;
        let tmp = Self::tmp_path_for(&target);

        let write_result = async {
            let mut file = tokio::fs::File::create(&tmp)
                .await
                .map_err(|e| self.io_err(e))?;
            file.write_all(&bytes).await.map_err(|e| self.io_err(e))?;
            file.sync_all().await.map_err(|e| self.io_err(e))
        }
        .await;

        if let Err(e) = write_result {
            // Best-effort cleanup of the temp file.
            drop(tokio::fs::remove_file(&tmp).await);
            return Err(e);
        }

        self.publish_tmp(&tmp, &target, parent.as_deref()).await
    }

    /// Writes and hashes chunks as they arrive without buffering the body; an oversized
    /// upload is aborted mid-stream. The partial temp file is removed on any failure.
    async fn put_stream(
        &self,
        path: &str,
        stream: BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<(u64, [u8; 32]), DomainError> {
        let (target, parent) = self.prepare_target(path).await?;
        let tmp = Self::tmp_path_for(&target);

        let write_result = self.write_stream_to_tmp(&tmp, stream, max_size).await;

        let (bytes_written, digest) = match write_result {
            Ok(v) => v,
            Err(e) => {
                // Best-effort cleanup of the partial temp file.
                drop(tokio::fs::remove_file(&tmp).await);
                return Err(e);
            }
        };

        self.publish_tmp(&tmp, &target, parent.as_deref()).await?;
        Ok((bytes_written, digest))
    }

    async fn get(&self, path: &str) -> Result<Bytes, DomainError> {
        let target = self.resolve(path)?;
        let data = tokio::fs::read(&target).await.map_err(|e| self.io_err(e))?;
        Ok(Bytes::from(data))
    }

    /// Streams the file in fixed-size chunks, so at most one chunk is in memory. Manual
    /// reads avoid pulling in `tokio-util` for `ReaderStream`.
    async fn get_stream(
        &self,
        path: &str,
    ) -> Result<BoxStream<'_, std::io::Result<Bytes>>, DomainError> {
        const CHUNK_SIZE: usize = 64 * 1024;

        let target = self.resolve(path)?;
        let file = tokio::fs::File::open(&target)
            .await
            .map_err(|e| self.io_err(e))?;

        // `state` is `None` after an error or EOF so the stream ends instead of re-polling.
        let stream = futures::stream::unfold(Some(file), |state| async move {
            let mut file = state?;
            let mut buf = vec![0u8; CHUNK_SIZE];
            match file.read(&mut buf).await {
                Ok(0) => None,
                Ok(n) => {
                    buf.truncate(n);
                    Some((Ok(Bytes::from(buf)), Some(file)))
                }
                Err(e) => Some((Err(e), None)),
            }
        });
        Ok(Box::pin(stream))
    }

    /// Seeks to the offset and reads only the requested bytes.
    async fn get_range(&self, path: &str, range: ByteRange) -> Result<Bytes, DomainError> {
        let target = self.resolve(path)?;
        let mut file = tokio::fs::File::open(&target)
            .await
            .map_err(|e| self.io_err(e))?;
        let total = file.metadata().await.map_err(|e| self.io_err(e))?.len();
        let Some((start, end)) = range.resolve(total) else {
            return Err(DomainError::validation("range", "unsatisfiable byte range"));
        };
        // `resolve` yields an inclusive end; clamp defensively.
        let end = end.min(total.saturating_sub(1));
        // Reject an oversized range rather than risk an OOM allocation.
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

    /// Reads only the file's metadata.
    async fn size(&self, path: &str) -> Result<u64, DomainError> {
        let target = self.resolve(path)?;
        let meta = tokio::fs::metadata(&target)
            .await
            .map_err(|e| self.io_err(e))?;
        Ok(meta.len())
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

    /// Walks `root` recursively, returning backend-relative paths (`"/{a}/{b}"`).
    /// A missing root (no uploads yet) yields an empty list.
    async fn list_paths(&self) -> Result<Vec<String>, DomainError> {
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

    /// Readiness probe: `root` exists and is a directory (catches an unmounted volume).
    async fn is_ready(&self) -> Result<(), DomainError> {
        let meta = tokio::fs::metadata(&self.root)
            .await
            .map_err(|e| self.io_err(e))?;
        if meta.is_dir() {
            Ok(())
        } else {
            Err(DomainError::backend(&self.id, "root is not a directory"))
        }
    }
}
