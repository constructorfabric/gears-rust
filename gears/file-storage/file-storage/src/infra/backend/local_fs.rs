//! Local filesystem storage backend
//! (`cpt-cf-file-storage-fr-backend-abstraction`).
//!
//! Blobs are stored at `<root>/<sanitized-path>`. The opaque path
//! (`/{file_id}/{version_id}`) is sanitized to prevent traversal outside root.
//!
//! `put` never writes directly to the target path. Instead it: (1) writes the
//! bytes to a sibling temp file (`<target>.tmp.<uuid>`) in the same directory
//! as `target`, so the final rename below is on the same filesystem; (2)
//! fsyncs the temp file's data + metadata before the handle is dropped; (3)
//! atomically renames the temp file onto `target` (a same-filesystem POSIX
//! rename never exposes a torn/partial file to a concurrent reader); (4)
//! best-effort fsyncs the parent directory so the rename's directory entry
//! itself is durable (needed on some filesystems, e.g. ext4/xfs, to survive a
//! crash). Step (4) is best-effort: if directory fsync is unsupported or
//! fails, a warning is logged and `put` still returns `Ok`, since the blob
//! itself is already durably in place after the rename.
//!
//! `publish_exclusive` (the sidecar's single-shot upload path) follows the
//! same write-to-temp-then-publish shape, but its publish step is a
//! `std::fs::hard_link` instead of a `rename` — see
//! [`StorageBackend::publish_exclusive`] for why a `PUT` token replay must
//! not be allowed to overwrite an already-published object.

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

use super::{BackendCapabilities, PublishOutcome, StorageBackend};

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

    /// Enable/disable the best-effort parent-directory fsync performed after
    /// each successful `put`'s rename. Defaults to `true`.
    #[must_use]
    pub fn with_fsync_parent_dir(mut self, enabled: bool) -> Self {
        self.fsync_parent_dir = enabled;
        self
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

    /// Best-effort directory fsync: opening a directory for read and calling
    /// `sync_all` flushes its directory-entry metadata (e.g. a rename) to
    /// durable storage on platforms/filesystems that support it.
    async fn fsync_dir(&self, dir: &std::path::Path) -> std::io::Result<()> {
        let dir_handle = tokio::fs::File::open(dir).await?;
        dir_handle.sync_all().await
    }

    /// Resolve `path` to its target file, ensuring the parent directory
    /// exists. Shared setup step for both `put` (whole-buffer write) and
    /// `put_stream` (chunked write) — both write into a sibling temp file
    /// under the same parent before converging on `publish_tmp`.
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

    /// A sibling temp-file path for `target`, unique per call.
    fn tmp_path_for(target: &Path) -> PathBuf {
        PathBuf::from(format!("{}.tmp.{}", target.display(), Uuid::now_v7()))
    }

    /// Atomically publish an already-written-and-fsynced temp file at
    /// `target`: rename it into place, then best-effort fsync the parent
    /// directory so the rename's directory entry is durable. Shared tail of
    /// `put` and `put_stream` — see module docs for the full durability
    /// rationale.
    async fn publish_tmp(
        &self,
        tmp: &Path,
        target: &Path,
        parent: Option<&Path>,
    ) -> Result<(), DomainError> {
        // Atomic same-filesystem replace: a concurrent reader either sees the
        // old file or the fully-written new one, never a torn mix.
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

    /// Atomically publish an already-written-and-fsynced temp file at
    /// `target` **iff `target` does not already exist** (create-exclusive —
    /// P2 remediation, replay-`PUT` overwrite fix; see
    /// [`StorageBackend::publish_exclusive`]'s doc comment for why this must
    /// not silently replace an existing object like [`Self::publish_tmp`]
    /// does).
    ///
    /// `std::fs::hard_link` creates a second directory entry pointing at
    /// `tmp`'s inode and, per POSIX `link(2)`, atomically fails with
    /// `AlreadyExists` if `target` already names something — unlike
    /// `rename`, which would silently replace it. On success, `tmp`'s own
    /// directory entry is then removed (the data now lives solely under
    /// `target`; the underlying inode is not freed since a link still refers
    /// to it) and the parent directory is best-effort fsynced exactly like
    /// `publish_tmp`.
    ///
    /// Returns `Ok(true)` if this call created `target`, `Ok(false)` if
    /// `target` already existed — the caller is responsible for cleaning up
    /// `tmp` in that case (this method never touches `tmp` on the
    /// already-exists path, so a caller that wants to inspect it first
    /// still can).
    async fn publish_tmp_exclusive(
        &self,
        tmp: &Path,
        target: &Path,
        parent: Option<&Path>,
    ) -> Result<bool, DomainError> {
        match tokio::fs::hard_link(tmp, target).await {
            Ok(()) => {
                self.finish_exclusive_publish(tmp, parent).await;
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(self.io_err(e)),
        }
    }

    /// Best-effort cleanup + durability tail of a successful hard-link
    /// publish: remove the temp file's own directory entry (the data now
    /// lives solely under `target`, via the hard link) and fsync the parent
    /// directory, mirroring `publish_tmp`'s own post-rename tail. Both steps
    /// are best-effort and only logged on failure, never fatal — split out
    /// of `publish_tmp_exclusive` to keep its own cognitive complexity down.
    async fn finish_exclusive_publish(&self, tmp: &Path, parent: Option<&Path>) {
        // Best-effort: a failure here just leaves a harmless extra link,
        // cleaned up like any other stray `*.tmp.*` by the
        // orphan-reconciliation sweep.
        if let Err(e) = tokio::fs::remove_file(tmp).await {
            tracing::warn!(
                error = ?e,
                "failed to remove temp file's directory entry after hard-link publish"
            );
        }

        if self.fsync_parent_dir
            && let Some(parent) = parent
            && let Err(e) = self.fsync_dir(parent).await
        {
            tracing::warn!(
                error = ?e,
                "parent-dir fsync failed or unsupported by this filesystem, continuing"
            );
        }
    }

    /// Stream `stream`'s chunks into `tmp`, hashing incrementally and
    /// aborting (without waiting for the rest of the stream) the moment the
    /// running byte count exceeds `max_size`. Returns `(bytes_written,
    /// digest)` on success. Caller is responsible for cleaning up `tmp` on
    /// error and for the final `sync_all`/rename/parent-fsync sequence.
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
            // Local filesystem writes survive process restarts.
            durable: true,
            ..BackendCapabilities::default()
        }
    }

    async fn put(&self, path: &str, bytes: Bytes) -> Result<(), DomainError> {
        let (target, parent) = self.prepare_target(path).await?;
        let tmp = Self::tmp_path_for(&target);

        // Write + fsync the temp file before it is ever visible at `target`.
        let write_result = async {
            let mut file = tokio::fs::File::create(&tmp)
                .await
                .map_err(|e| self.io_err(e))?;
            file.write_all(&bytes).await.map_err(|e| self.io_err(e))?;
            file.sync_all().await.map_err(|e| self.io_err(e))
        }
        .await;

        if let Err(e) = write_result {
            // Best-effort cleanup: never leave an orphaned `*.tmp.*` behind.
            drop(tokio::fs::remove_file(&tmp).await);
            return Err(e);
        }

        self.publish_tmp(&tmp, &target, parent.as_deref()).await
    }

    /// Stream a blob into `path` without ever buffering the whole body in
    /// memory: chunks are written + hashed as they arrive, and the running
    /// byte count is checked against `max_size` after every chunk so an
    /// oversized upload is aborted mid-stream (the moment the limit is
    /// crossed) rather than after the full body has been received. The
    /// partial temp file is removed on any failure path (oversized, I/O
    /// error, or a stream error), exactly like `put`'s cleanup-on-failure
    /// behavior.
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
                // Best-effort cleanup: never leave a partial `*.tmp.*` behind,
                // whether the failure was an oversized stream or an I/O error.
                drop(tokio::fs::remove_file(&tmp).await);
                return Err(e);
            }
        };

        self.publish_tmp(&tmp, &target, parent.as_deref()).await?;
        Ok((bytes_written, digest))
    }

    /// Create-exclusive publish (P2 remediation — replay-`PUT` overwrite
    /// fix). Writes + hashes the stream into a temp file exactly like
    /// `put_stream`, but publishes it via [`Self::publish_tmp_exclusive`]
    /// (hard-link, atomically fails on an existing target) instead of
    /// `publish_tmp`'s unconditional rename. See
    /// [`StorageBackend::publish_exclusive`] for the full contract.
    async fn publish_exclusive(
        &self,
        path: &str,
        stream: BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<PublishOutcome, DomainError> {
        let (target, parent) = self.prepare_target(path).await?;
        let tmp = Self::tmp_path_for(&target);

        let write_result = self.write_stream_to_tmp(&tmp, stream, max_size).await;
        let (bytes_written, digest) = match write_result {
            Ok(v) => v,
            Err(e) => {
                drop(tokio::fs::remove_file(&tmp).await);
                return Err(e);
            }
        };

        match self
            .publish_tmp_exclusive(&tmp, &target, parent.as_deref())
            .await
        {
            Ok(true) => Ok(PublishOutcome {
                bytes_written,
                digest,
                created: true,
            }),
            Ok(false) => {
                // The destination already existed: it was never touched.
                // `tmp` still holds this attempt's freshly-written (but
                // never linked) bytes and must be cleaned up like any other
                // rejected upload.
                drop(tokio::fs::remove_file(&tmp).await);
                Ok(PublishOutcome {
                    bytes_written,
                    digest,
                    created: false,
                })
            }
            Err(e) => {
                drop(tokio::fs::remove_file(&tmp).await);
                Err(e)
            }
        }
    }

    async fn get(&self, path: &str) -> Result<Bytes, DomainError> {
        let target = self.resolve(path)?;
        let data = tokio::fs::read(&target).await.map_err(|e| self.io_err(e))?;
        Ok(Bytes::from(data))
    }

    /// Stream the blob at `path` from disk in fixed-size chunks via manual
    /// `AsyncReadExt` reads, so a read-back (e.g. finalize's) never
    /// materializes more than one chunk of the file in memory regardless of
    /// its size. This crate does not otherwise depend on `tokio-util`, so
    /// this deliberately avoids `ReaderStream` rather than pulling in a new
    /// dependency for a single call site.
    async fn get_stream(
        &self,
        path: &str,
    ) -> Result<BoxStream<'_, std::io::Result<Bytes>>, DomainError> {
        const CHUNK_SIZE: usize = 64 * 1024;

        let target = self.resolve(path)?;
        let file = tokio::fs::File::open(&target)
            .await
            .map_err(|e| self.io_err(e))?;

        // `state` is `None` once a read has errored or the file is exhausted,
        // so the stream terminates cleanly rather than re-polling a file
        // handle that already reported an error.
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

    /// Cheap stat: reads only the file's metadata, never its content, so
    /// range-aware callers (P2 1.11) can resolve a `Range` request without
    /// paying for a full read first.
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

    /// Readiness probe: confirms `root` exists and is a directory. Catches
    /// an unmounted volume or a misconfigured root before a real request
    /// tries to read/write through it. Never touches file content.
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
