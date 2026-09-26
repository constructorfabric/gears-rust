//! Local filesystem storage backend
//! (`cpt-cf-file-storage-fr-backend-abstraction`).
//!
//! Blobs are stored at `<root>/<sanitized-path>`. The opaque path
//! (`/{file_id}/{version_id}`) is sanitized to prevent traversal outside root.
//!
//! `put_stream` never writes directly to the target path. Instead it: (1)
//! streams the bytes into a sibling temp file (`<target>.tmp.<uuid>`) in the
//! same directory as `target`, so the final rename below is on the same
//! filesystem; (2) fsyncs the temp file's data + metadata before the handle
//! is dropped; (3) atomically renames the temp file onto `target` (a
//! same-filesystem POSIX rename never exposes a torn/partial file to a
//! concurrent reader); (4) best-effort fsyncs the parent directory so the
//! rename's directory entry itself is durable (needed on some filesystems,
//! e.g. ext4/xfs, to survive a crash). Step (4) is best-effort: if directory
//! fsync is unsupported or fails, a warning is logged and `put_stream` still
//! returns `Ok`, since the blob itself is already durably in place after the
//! rename.
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

use super::{BackendCapabilities, PublishOutcome, StorageBackend, check_read_prefix_budget};

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
    /// each successful `put_stream`'s rename. Defaults to `true`.
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

    /// Classifies by `e.kind()` via the shared
    /// [`super::is_transient_io_error`]: a stalled read/write, a
    /// signal-interrupted syscall, a would-block on a non-blocking fd, or a
    /// dropped connection are transient (retrying the same operation is
    /// expected to make progress); anything else (a missing path, permission
    /// denied, disk full, ...) is a permanent fault.
    fn io_err(&self, e: &std::io::Error) -> DomainError {
        let msg = e.to_string();
        if super::is_transient_io_error(e.kind()) {
            DomainError::backend_unavailable(&self.id, msg)
        } else {
            DomainError::backend(&self.id, msg)
        }
    }

    /// Best-effort directory fsync: opening a directory for read and calling
    /// `sync_all` flushes its directory-entry metadata (e.g. a rename) to
    /// durable storage on platforms/filesystems that support it.
    async fn fsync_dir(&self, dir: &std::path::Path) -> std::io::Result<()> {
        let dir_handle = tokio::fs::File::open(dir).await?;
        dir_handle.sync_all().await
    }

    /// Resolve `path` to its target file, ensuring the parent directory
    /// exists. Shared setup step for `put_stream`'s chunked write, which
    /// writes into a sibling temp file under the same parent before
    /// converging on `publish_tmp`.
    async fn prepare_target(&self, path: &str) -> Result<(PathBuf, Option<PathBuf>), DomainError> {
        let target = self.resolve(path)?;
        let parent = target.parent().map(Path::to_path_buf);
        if let Some(parent) = &parent {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| self.io_err(&e))?;
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
    /// `put_stream` and `publish_exclusive`'s successful write — see module
    /// docs for the full durability rationale.
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
            .map_err(|e| self.io_err(&e))?;

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
    /// `target` **iff `target` does not already exist** (create-exclusive;
    /// see [`StorageBackend::publish_exclusive`]'s doc comment for why this
    /// must not silently replace an existing object like
    /// [`Self::publish_tmp`] does).
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
            Err(e) => Err(self.io_err(&e)),
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
            .map_err(|e| self.io_err(&e))?;
        let mut hasher = hash::Hasher::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| self.io_err(&e))?;
            file.write_all(&chunk).await.map_err(|e| self.io_err(&e))?;
            hasher.update(&chunk);
            if max_size.is_some_and(|m| hasher.len() > m) {
                return Err(DomainError::validation("size", "exceeds max_size"));
            }
        }
        file.sync_all().await.map_err(|e| self.io_err(&e))?;
        let bytes_written = hasher.len();
        let digest = hash::digest_to_array(hasher.finalize());
        Ok((bytes_written, digest))
    }

    /// Fixed-size chunk length shared by [`Self::chunked_file_stream`]'s two
    /// callers (`get_stream`/`get_range_stream`) — matches the chunk size
    /// `write_stream_to_tmp`'s write side effectively uses via its
    /// caller-supplied stream, and is small enough that a single in-flight
    /// chunk is never a meaningful memory concern.
    const READ_CHUNK_SIZE: usize = 64 * 1024;

    /// Shared chunked-read core for `get_stream` (whole-object) and
    /// `get_range_stream` (a resolved byte range): pulls up to
    /// [`Self::READ_CHUNK_SIZE`] bytes at a time from `file` — already
    /// positioned via `seek` by the caller when a range read starts mid-file
    /// — stopping at EOF (`remaining: None`, the whole-file case) or once
    /// exactly `remaining` bytes have been yielded (a resolved range's
    /// length, so a range read never reads even one byte past its own end).
    ///
    /// Returns `BoxStream<'static, _>` rather than borrowing `&self`: `file`
    /// is moved wholesale into the `unfold` state and nothing here ever
    /// touches `self` again, so the stream owns everything it needs outright
    /// — see [`StorageBackend::get_stream`]'s doc comment for why that
    /// `'static` bound matters (it is what lets the sidecar hand the stream
    /// straight to `axum::body::Body::from_stream`).
    ///
    /// `state` in the `unfold` closure is `None` once a read has errored or
    /// the file/range is exhausted, so the stream terminates cleanly rather
    /// than re-polling a file handle that already reported an error.
    ///
    /// Reaching EOF (`read` returns `Ok(0)`) while `remaining` still
    /// names an outstanding byte count (i.e. the file turned out shorter
    /// than the length the caller committed to -- `get_range_stream`'s
    /// resolved range length, or `get_stream`'s `expected_len`) is a genuine
    /// short-read: the caller already told an HTTP client how many bytes to
    /// expect, so silently ending the stream here would just hand back a
    /// body shorter than its own `Content-Length` with no diagnostic. That
    /// case yields an `UnexpectedEof` error instead of ending the stream
    /// cleanly. This is the *second* line of defense against a file changing
    /// underneath a read: `get_stream`/`get_range_stream` already refuse a
    /// length mismatch observed at open time before returning any stream at
    /// all (see their own doc comments); this bound additionally catches a
    /// change landing *after* that check but before (or during) the read
    /// loop itself.
    ///
    /// Both of this module's callers pass a length: `get_range_stream` its
    /// resolved (and already-verified-equal-to-`expected_len`) range length,
    /// `get_stream` its own `expected_len` directly. `remaining: None`
    /// therefore means only "read to EOF, no length was ever promised" --
    /// kept for callers that legitimately do not know the size up front,
    /// where an `Ok(0)` simply ends the stream.
    fn chunked_file_stream(
        file: tokio::fs::File,
        remaining: Option<u64>,
    ) -> BoxStream<'static, std::io::Result<Bytes>> {
        let stream =
            futures::stream::unfold((Some(file), remaining), |(state, remaining)| async move {
                let mut file = state?;
                if remaining == Some(0) {
                    return None;
                }
                let want = remaining.map_or(Self::READ_CHUNK_SIZE, |r| {
                    usize::try_from(r)
                        .unwrap_or(usize::MAX)
                        .min(Self::READ_CHUNK_SIZE)
                });
                let mut buf = vec![0u8; want];
                match file.read(&mut buf).await {
                    // `remaining == Some(0)` is already handled above, so
                    // reaching an EOF read with `remaining: Some(r)` here
                    // means `r > 0`: the file ended before yielding the
                    // bytes the caller expected.
                    Ok(0) => remaining.map(|r| {
                        (
                            Err(std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                format!("file ended {r} byte(s) short of the expected read length"),
                            )),
                            (None, None),
                        )
                    }),
                    Ok(n) => {
                        buf.truncate(n);
                        let next_remaining = remaining.map(|r| r - n as u64);
                        Some((Ok(Bytes::from(buf)), (Some(file), next_remaining)))
                    }
                    Err(e) => Some((Err(e), (None, None))),
                }
            });
        Box::pin(stream)
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

    /// Stream a blob into `path` without ever buffering the whole body in
    /// memory: chunks are written + hashed as they arrive, and the running
    /// byte count is checked against `max_size` after every chunk so an
    /// oversized upload is aborted mid-stream (the moment the limit is
    /// crossed) rather than after the full body has been received. The
    /// partial temp file is removed on any failure path (oversized, I/O
    /// error, or a stream error).
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

    /// Create-exclusive publish. Writes + hashes the stream into a temp file
    /// exactly like `put_stream`, but publishes it via
    /// [`Self::publish_tmp_exclusive`]
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

    /// Read up to `max_bytes` from the start of `path` without reading past
    /// it: opens the file and reads at most `max_bytes`, stopping at EOF if
    /// the object is shorter. `NotFound` maps to `Ok(None)`, mirroring
    /// `stat`'s presence contract; nothing here ever reads the file's own
    /// metadata/length first, since the bound is on `max_bytes` alone.
    async fn read_prefix(&self, path: &str, max_bytes: u64) -> Result<Option<Bytes>, DomainError> {
        check_read_prefix_budget(max_bytes)?;
        let target = self.resolve(path)?;
        let mut file = match tokio::fs::File::open(&target).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(self.io_err(&e)),
        };
        let want = usize::try_from(max_bytes).unwrap_or(usize::MAX);
        let mut buf = vec![0u8; want];
        let mut filled = 0usize;
        while filled < want {
            let n = file
                .read(&mut buf[filled..])
                .await
                .map_err(|e| self.io_err(&e))?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        buf.truncate(filled);
        Ok(Some(Bytes::from(buf)))
    }

    /// Stream the blob at `path` from disk in fixed-size chunks, so a
    /// read-back (e.g. finalize's) or a whole-object download never
    /// materializes more than one chunk of the file in memory regardless of
    /// its size. Delegates to [`Self::chunked_file_stream`], bounded by
    /// `expected_len` — see that method's doc comment for the shared chunking
    /// mechanics it and [`Self::get_range_stream`] both build on.
    ///
    /// `expected_len` is the length the caller already committed to
    /// elsewhere (e.g. the sidecar's `Content-Length`, set from a `stat`
    /// taken *before* this call). This method's own `file.metadata()` call is
    /// a second, independent observation of the same file — a write landing
    /// strictly between the caller's `stat` and this call's `open` would
    /// otherwise go undetected, since the two observations never get
    /// compared. Checking `metadata().len() == expected_len` here, before any
    /// byte is ever read, closes that gap: a file that changed size in that
    /// window is refused up front rather than silently streamed at its new
    /// (wrong) length. A change happening *after* this check — inside the
    /// read loop itself — is still caught by `chunked_file_stream`'s own
    /// short-read detection, bounded by this same `expected_len`.
    async fn get_stream(
        &self,
        path: &str,
        expected_len: u64,
    ) -> Result<BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        let target = self.resolve(path)?;
        let file = tokio::fs::File::open(&target)
            .await
            .map_err(|e| self.io_err(&e))?;
        let len = file.metadata().await.map_err(|e| self.io_err(&e))?.len();
        if len != expected_len {
            return Err(DomainError::conflict(format!(
                "object at '{path}' changed size before it could be read: expected {expected_len} byte(s), found {len}"
            )));
        }
        Ok(Self::chunked_file_stream(file, Some(expected_len)))
    }

    /// Native streaming range read: resolve the range against the file's
    /// real length, seek once, then hand off to the same
    /// [`Self::chunked_file_stream`] core `get_stream` uses — bounded this
    /// time by the range's length — so a `Range` request (including
    /// `bytes=0-`, which resolves to the whole object and is the very first
    /// request many media players issue) never allocates a `len`-sized
    /// buffer up front.
    ///
    /// `expected_len` is the resolved range length the caller already
    /// committed to (the sidecar resolves `range` itself first to build
    /// `Content-Range`/`Content-Length`, then calls this with the same
    /// `range`). This method re-resolves `range` against its own, independent
    /// `file.metadata()` observation — a file that grew or shrank strictly
    /// between the caller's resolution and this one can make that
    /// re-resolution land on a different length for the *same* `range` value
    /// (e.g. `OpenEnded { start: 0 }` against a bigger or smaller `total`).
    /// Comparing the freshly resolved length to `expected_len` before seeking
    /// or reading a single byte catches that: a mismatch is refused up front
    /// rather than silently streamed at the wrong length.
    async fn get_range_stream(
        &self,
        path: &str,
        range: ByteRange,
        expected_len: u64,
    ) -> Result<BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        let target = self.resolve(path)?;
        let mut file = tokio::fs::File::open(&target)
            .await
            .map_err(|e| self.io_err(&e))?;
        let total = file.metadata().await.map_err(|e| self.io_err(&e))?.len();
        let Some((start, end)) = range.resolve(total) else {
            return Err(DomainError::validation("range", "unsatisfiable byte range"));
        };
        // `resolve` yields an inclusive end; clamp defensively against `total`.
        let end = end.min(total.saturating_sub(1));
        let len = end - start + 1;
        if len != expected_len {
            return Err(DomainError::conflict(format!(
                "object at '{path}' range changed before it could be read: expected {expected_len} byte(s), resolved {len}"
            )));
        }
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|e| self.io_err(&e))?;
        Ok(Self::chunked_file_stream(file, Some(expected_len)))
    }

    /// Cheap stat: reads only the file's metadata, never its content, so
    /// range-aware callers can resolve a `Range` request without paying for
    /// a full read first.
    async fn size(&self, path: &str) -> Result<u64, DomainError> {
        let target = self.resolve(path)?;
        let meta = tokio::fs::metadata(&target)
            .await
            .map_err(|e| self.io_err(&e))?;
        Ok(meta.len())
    }

    async fn delete(&self, path: &str) -> Result<(), DomainError> {
        let target = self.resolve(path)?;
        match tokio::fs::remove_file(&target).await {
            Ok(()) => Ok(()),
            // Idempotent: a missing blob is a successful delete.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(self.io_err(&e)),
        }
    }

    async fn exists(&self, path: &str) -> Result<bool, DomainError> {
        let target = self.resolve(path)?;
        // Only a genuine "not found" means absent; permission/IO errors are real
        // failures and must not be silently reported as a missing blob.
        match tokio::fs::metadata(&target).await {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(self.io_err(&e)),
        }
    }

    /// Native combined stat: a single `stat(2)` distinguishes "not
    /// found" (`Ok(None)`) from "present, this many bytes" (`Ok(Some(len))`)
    /// from a genuine I/O fault (`Err`), where `exists` followed by `size`
    /// would need two separate syscalls.
    async fn stat(&self, path: &str) -> Result<Option<u64>, DomainError> {
        let target = self.resolve(path)?;
        match tokio::fs::metadata(&target).await {
            Ok(meta) => Ok(Some(meta.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(self.io_err(&e)),
        }
    }

    /// Walk the root directory recursively and return all file paths as
    /// backend-relative paths in the form `"/{component}/{component}"`.
    ///
    /// Non-existent root (fresh install with no uploads yet) returns an empty
    /// vec rather than an error.
    async fn list_paths(&self) -> Result<Vec<String>, DomainError> {
        // If the root does not exist yet (no blobs written), return empty.
        match tokio::fs::metadata(&self.root).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(self.io_err(&e)),
        }

        let mut paths = Vec::new();
        let mut stack = vec![self.root.clone()];

        while let Some(dir) = stack.pop() {
            let mut entries = tokio::fs::read_dir(&dir)
                .await
                .map_err(|e| self.io_err(&e))?;

            while let Some(entry) = entries.next_entry().await.map_err(|e| self.io_err(&e))? {
                let ft = entry.file_type().await.map_err(|e| self.io_err(&e))?;
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
            .map_err(|e| self.io_err(&e))?;
        if meta.is_dir() {
            Ok(())
        } else {
            Err(DomainError::backend(&self.id, "root is not a directory"))
        }
    }
}
