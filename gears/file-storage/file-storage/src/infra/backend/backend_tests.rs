use std::sync::Arc;

use bytes::Bytes;
use file_storage_sdk::ByteRange;
use futures::StreamExt;
use futures::stream::{self, BoxStream};

use crate::domain::error::DomainError;
use crate::infra::content::hash;

use super::*;

fn unique_root() -> std::path::PathBuf {
    // No Math.random in scripts, but tests can use uuid + temp_dir.
    let mut p = std::env::temp_dir();
    p.push(format!("cf-fs-test-{}", uuid::Uuid::now_v7()));
    p
}

/// Assert that `backend.get_stream(path)`'s concatenated chunks are
/// byte-for-byte equal to `backend.get(path)`'s result — the `get_stream`
/// contract every `StorageBackend` implementation (default-fallback or a true
/// chunked override) must satisfy. Factored out of `assert_backend_contract`
/// to keep that function's own cognitive complexity down.
async fn assert_get_stream_matches_get(backend: &dyn StorageBackend, path: &str, expected: &[u8]) {
    let mut stream = backend.get_stream(path).await.unwrap();
    let mut streamed = Vec::new();
    while let Some(chunk) = stream.next().await {
        streamed.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(streamed, expected);
}

/// Assert that `backend.get_range_stream(path, range)`'s concatenated chunks
/// are byte-for-byte equal to `expected` -- the `get_range_stream` contract
/// every `StorageBackend` implementation (default-fallback or a true
/// native override) must satisfy, mirroring `assert_get_stream_matches_get`
/// for the ranged case.
async fn assert_get_range_stream_matches_slice(
    backend: &dyn StorageBackend,
    path: &str,
    range: ByteRange,
    expected: &[u8],
) {
    let mut stream = backend.get_range_stream(path, range).await.unwrap();
    let mut streamed = Vec::new();
    while let Some(chunk) = stream.next().await {
        streamed.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(streamed, expected);
}

/// Shared behavioral contract every `StorageBackend` implementation must
/// satisfy, factored out of what used to be per-backend hand-written
/// `put/get/delete/exists/get_range` assertions (`in_memory_*`/`local_fs_*`
/// duplicated the same checks). Covers: put -> get round trip, `get_stream`
/// (streamed chunks reassemble to the same bytes `get` returns), `get_range`
/// correctness for both the `Inclusive` and `Suffix` variants (mirroring the
/// former `default_get_range_slices_content`/`get_range_suffix_returns_tail`
/// assertions), idempotent `delete`, and `exists` distinguishing
/// present/missing. Backend-specific behavior (atomicity, tmp-file cleanup,
/// path-traversal rejection, etc.) stays in each backend's own tests.
pub async fn assert_backend_contract(backend: &dyn StorageBackend) {
    // put -> get round trip, and exists() reports present.
    backend
        .put("contract/put-get", Bytes::from_static(b"hello, contract"))
        .await
        .unwrap();
    assert_eq!(
        backend.get("contract/put-get").await.unwrap(),
        Bytes::from_static(b"hello, contract")
    );
    assert!(backend.exists("contract/put-get").await.unwrap());

    // get_stream: concatenated chunks must equal get()'s bytes, for every
    // backend regardless of whether it overrides the default single-chunk
    // fallback with a true chunked read.
    assert_get_stream_matches_get(backend, "contract/put-get", b"hello, contract").await;

    // The remaining groups live in their own functions so each stays under
    // the crate's cognitive-complexity ceiling; all of them run for every
    // backend the contract is asserted against.
    assert_range_and_delete_contract(backend).await;
    assert_stat_contract(backend).await;
    assert_range_stream_contract(backend).await;
}

/// The buffered-`get_range`, `delete` and `exists` groups of
/// [`assert_backend_contract`].
async fn assert_range_and_delete_contract(backend: &dyn StorageBackend) {
    // get_range: Inclusive and Suffix variants.
    backend
        .put("contract/range", Bytes::from_static(b"0123456789"))
        .await
        .unwrap();
    let slice = backend
        .get_range("contract/range", ByteRange::Inclusive { start: 2, end: 4 })
        .await
        .unwrap();
    assert_eq!(slice, Bytes::from_static(b"234"));
    let tail = backend
        .get_range("contract/range", ByteRange::Suffix { length: 3 })
        .await
        .unwrap();
    assert_eq!(tail, Bytes::from_static(b"789"));

    // delete is idempotent.
    backend
        .put("contract/delete", Bytes::from_static(b"x"))
        .await
        .unwrap();
    backend.delete("contract/delete").await.unwrap();
    backend.delete("contract/delete").await.unwrap();
    assert!(!backend.exists("contract/delete").await.unwrap());

    // exists distinguishes present from missing.
    assert!(!backend.exists("contract/never-existed").await.unwrap());
}

/// The `stat` and `get_range_stream` half of [`assert_backend_contract`].
///
/// Kept separate purely for readability and lint budget -- callers should use
/// `assert_backend_contract`, which runs both halves.
/// The `stat` half of [`assert_backend_contract`]: `Some(len)` for a present
/// object (including an empty one), `Ok(None)` for an absent one, and
/// agreement with the `exists` + `size` composition the trait's default
/// implementation is built from.
async fn assert_stat_contract(backend: &dyn StorageBackend) {
    // stat: an existing non-empty object returns Some(len) matching the
    // actual byte length written.
    backend
        .put(
            "contract/stat-nonempty",
            Bytes::from_static(b"twelve bytes"),
        )
        .await
        .unwrap();
    assert_eq!(
        backend.stat("contract/stat-nonempty").await.unwrap(),
        Some(12),
        "stat of an existing object must report its real byte length"
    );

    // stat: an existing EMPTY object still returns Some(0), never None --
    // "present with zero bytes" and "absent" must not collapse together.
    backend
        .put("contract/stat-empty", Bytes::new())
        .await
        .unwrap();
    assert_eq!(
        backend.stat("contract/stat-empty").await.unwrap(),
        Some(0),
        "stat of an empty object must report Some(0), not None"
    );

    // stat: a missing object returns Ok(None), never an Err.
    assert_eq!(
        backend.stat("contract/stat-never-existed").await.unwrap(),
        None,
        "stat of a missing object must be Ok(None), not an error"
    );

    // stat must agree with the exists()+size() combination for both a
    // present and an absent path -- the exact contract the default
    // (exists-then-size) implementation defines and every native override
    // must preserve.
    for path in [
        "contract/stat-nonempty",
        "contract/stat-empty",
        "contract/stat-never-existed",
    ] {
        let composed = if backend.exists(path).await.unwrap() {
            Some(backend.size(path).await.unwrap())
        } else {
            None
        };
        assert_eq!(
            backend.stat(path).await.unwrap(),
            composed,
            "stat must agree with exists()+size() for path {path}"
        );
    }
}

/// The `get_range_stream` half of [`assert_backend_contract`]: every
/// `ByteRange` variant streams exactly the requested slice, and an
/// unsatisfiable range is refused rather than silently truncated.
async fn assert_range_stream_contract(backend: &dyn StorageBackend) {
    // get_range_stream: Inclusive (narrow), OpenEnded (tail), and Suffix
    // variants must each stream exactly the bytes get_range resolves for the
    // same ByteRange.
    backend
        .put("contract/range-stream", Bytes::from_static(b"0123456789"))
        .await
        .unwrap();
    assert_get_range_stream_matches_slice(
        backend,
        "contract/range-stream",
        ByteRange::Inclusive { start: 2, end: 4 },
        b"234",
    )
    .await;
    assert_get_range_stream_matches_slice(
        backend,
        "contract/range-stream",
        ByteRange::OpenEnded { start: 7 },
        b"789",
    )
    .await;
    assert_get_range_stream_matches_slice(
        backend,
        "contract/range-stream",
        ByteRange::Suffix { length: 4 },
        b"6789",
    )
    .await;

    // An unsatisfiable range must surface a validation error, not panic or
    // silently truncate.
    // `expect_err` is not available here: the `Ok` variant is a boxed stream,
    // which does not implement `Debug`. Match the result instead.
    let range_result = backend
        .get_range_stream(
            "contract/range-stream",
            ByteRange::Inclusive { start: 20, end: 30 },
        )
        .await;
    match range_result {
        Ok(_) => panic!("an out-of-bounds range must be rejected, not silently resolved"),
        Err(err) => assert!(
            matches!(err, DomainError::Validation { .. }),
            "an unsatisfiable range must produce a Validation error, got {err:?}"
        ),
    }
}

#[tokio::test]
async fn in_memory_satisfies_backend_contract() {
    let b = InMemoryBackend::new("mem");
    assert_backend_contract(&b).await;
}

#[tokio::test]
async fn in_memory_get_missing_errors() {
    let b = InMemoryBackend::new("mem");
    assert!(b.get("nope").await.is_err());
    assert!(!b.exists("nope").await.unwrap());
}

#[tokio::test]
async fn local_fs_satisfies_backend_contract() {
    let root = unique_root();
    let b = LocalFsBackend::new("fs", &root);
    assert_backend_contract(&b).await;
    drop(tokio::fs::remove_dir_all(&root).await);
}

#[tokio::test]
async fn local_fs_rejects_path_traversal() {
    let root = unique_root();
    let b = LocalFsBackend::new("fs", &root);
    let res = b.put("../escape", Bytes::from_static(b"x")).await;
    assert!(res.is_err(), "path traversal must be rejected");
}

#[tokio::test]
async fn local_fs_put_is_atomic_under_concurrent_writers() {
    const WRITERS: u8 = 8;
    const SIZE: usize = 64 * 1024;

    let root = unique_root();
    let backend = Arc::new(LocalFsBackend::new("fs", &root));

    // N distinct full-size payloads, each filled with its own byte pattern so
    // a torn/mixed result is trivially detectable.
    let payloads: Vec<Bytes> = (0..WRITERS).map(|i| Bytes::from(vec![i; SIZE])).collect();

    let handles: Vec<_> = payloads
        .iter()
        .cloned()
        .map(|payload| {
            let backend = Arc::clone(&backend);
            tokio::spawn(async move { backend.put("fid/vid", payload).await })
        })
        .collect();

    for handle in handles {
        handle.await.unwrap().unwrap();
    }

    let got = backend.get("fid/vid").await.unwrap();
    assert_eq!(got.len(), SIZE, "result must be a full, untorn write");
    assert!(
        payloads.iter().any(|p| p == &got),
        "result must equal exactly one of the concurrent payloads in full, never a torn mix"
    );

    drop(tokio::fs::remove_dir_all(&root).await);
}

#[tokio::test]
async fn local_fs_put_leaves_no_tmp_file_after_success() {
    let root = unique_root();
    let b = LocalFsBackend::new("fs", &root);
    b.put("fid/vid", Bytes::from_static(b"hello"))
        .await
        .unwrap();

    let parent = root.join("fid");
    let mut entries = tokio::fs::read_dir(&parent).await.unwrap();
    while let Some(entry) = entries.next_entry().await.unwrap() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        assert!(!name.contains(".tmp."), "found leftover tmp file: {name}");
    }

    drop(tokio::fs::remove_dir_all(&root).await);
}

#[cfg(unix)]
#[tokio::test]
async fn local_fs_put_cleans_up_tmp_file_on_write_failure() {
    use std::os::unix::fs::PermissionsExt;

    let root = unique_root();
    let b = LocalFsBackend::new("fs", &root);

    // Pre-create the target's parent directory, then strip its write bit:
    // `create_dir_all` still succeeds (dir already exists), but the temp
    // file's `File::create` inside it fails with a permission error before
    // the atomic rename ever runs.
    let parent = root.join("fid");
    tokio::fs::create_dir_all(&parent).await.unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();

    let result = b.put("fid/vid", Bytes::from_static(b"data")).await;
    assert!(
        result.is_err(),
        "put must fail when the temp-file create fails"
    );

    let mut entries = tokio::fs::read_dir(&parent).await.unwrap();
    let mut names = Vec::new();
    while let Some(entry) = entries.next_entry().await.unwrap() {
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    assert!(
        !names.iter().any(|n| n.contains(".tmp.")),
        "no orphaned tmp file should remain, found: {names:?}"
    );

    // Restore permissions so the temp-dir cleanup below can actually remove it.
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    drop(tokio::fs::remove_dir_all(&root).await);
}

/// A stream whose cumulative size crosses `max_size` partway through must
/// be rejected *and* leave no file (partial or final) at the target path —
/// abort mid-stream, clean up.
#[tokio::test]
async fn local_fs_put_stream_enforces_max_size_mid_stream() {
    let root = unique_root();
    let b = LocalFsBackend::new("fs", &root);

    // Three 10-byte chunks (30 bytes total) against a 15-byte max_size: the
    // limit is crossed on the second chunk, well before the stream ends.
    let chunks: Vec<std::io::Result<Bytes>> = vec![
        Ok(Bytes::from_static(b"0123456789")),
        Ok(Bytes::from_static(b"0123456789")),
        Ok(Bytes::from_static(b"0123456789")),
    ];
    let stream: BoxStream<'_, std::io::Result<Bytes>> = Box::pin(stream::iter(chunks));

    let result = b.put_stream("fid/vid", stream, Some(15)).await;
    assert!(
        result.is_err(),
        "put_stream must reject a stream exceeding max_size"
    );

    let target = root.join("fid").join("vid");
    assert!(
        !target.exists(),
        "no destination file should be left behind after a rejected stream"
    );

    let parent = root.join("fid");
    if let Ok(mut entries) = tokio::fs::read_dir(&parent).await {
        while let Some(entry) = entries.next_entry().await.unwrap() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            assert!(!name.contains(".tmp."), "found leftover tmp file: {name}");
        }
    }

    drop(tokio::fs::remove_dir_all(&root).await);
}

/// `put_stream`'s incremental hash (fed chunk-by-chunk as they are written)
/// must equal `hash::sha256` computed over the fully concatenated bytes, and
/// `bytes_written` must equal the total chunk length.
#[tokio::test]
async fn local_fs_put_stream_computes_hash_incrementally_matches_full_buffer_hash() {
    let root = unique_root();
    let b = LocalFsBackend::new("fs", &root);

    let chunk_bytes: Vec<&'static [u8]> = vec![b"hello, ", b"streaming ", b"world!"];
    let total_len: u64 = chunk_bytes.iter().map(|c| c.len() as u64).sum();
    let concatenated: Vec<u8> = chunk_bytes.concat();

    let chunks: Vec<std::io::Result<Bytes>> = chunk_bytes
        .into_iter()
        .map(|c| Ok(Bytes::from_static(c)))
        .collect();
    let stream: BoxStream<'_, std::io::Result<Bytes>> = Box::pin(stream::iter(chunks));

    let (bytes_written, digest) = b
        .put_stream("fid2/vid2", stream, None)
        .await
        .expect("put_stream should succeed when under max_size");

    assert_eq!(bytes_written, total_len);
    let expected_digest = hash::digest_to_array(hash::sha256(&concatenated));
    assert_eq!(digest, expected_digest);

    // Sanity: the bytes actually landed at the target path too.
    assert_eq!(b.get("fid2/vid2").await.unwrap(), Bytes::from(concatenated));

    drop(tokio::fs::remove_dir_all(&root).await);
}

/// `LocalFsBackend::get_stream`'s manual-chunked-read loop (64 KiB chunks)
/// must reassemble a blob spanning multiple chunks to the exact same bytes
/// `get` returns.
#[tokio::test]
async fn local_fs_get_stream_reassembles_multi_chunk_blob() {
    let root = unique_root();
    let b = LocalFsBackend::new("fs", &root);

    // 200 KB — comfortably more than one 64 KiB chunk.
    let payload: Vec<u8> = (0..200_000)
        .map(|i| u8::try_from(i % 256).unwrap())
        .collect();
    b.put("fid/vid", Bytes::from(payload.clone()))
        .await
        .unwrap();

    let mut stream = b.get_stream("fid/vid").await.unwrap();
    let mut collected = Vec::new();
    let mut chunk_count = 0u32;
    while let Some(chunk) = stream.next().await {
        collected.extend_from_slice(&chunk.unwrap());
        chunk_count += 1;
    }
    assert_eq!(collected, payload);
    assert!(
        chunk_count > 1,
        "a 200KB blob must be delivered as more than one 64KB chunk"
    );

    drop(tokio::fs::remove_dir_all(&root).await);
}

#[tokio::test]
async fn local_fs_get_stream_missing_errors() {
    let root = unique_root();
    let b = LocalFsBackend::new("fs", &root);
    assert!(b.get_stream("nope/nope").await.is_err());
    drop(tokio::fs::remove_dir_all(&root).await);
}

/// The first `publish_exclusive` call to a fresh path must create it; a
/// second call to the SAME path with DIFFERENT bytes must report
/// `created: false` and must leave the
/// already-published bytes completely untouched — the core integrity
/// guarantee `StorageBackend::publish_exclusive` exists for.
#[tokio::test]
async fn local_fs_publish_exclusive_rejects_second_write_to_same_path() {
    let root = unique_root();
    let b = LocalFsBackend::new("fs", &root);

    let stream1: BoxStream<'_, std::io::Result<Bytes>> =
        Box::pin(stream::iter(vec![Ok(Bytes::from_static(b"first"))]));
    let outcome1 = b
        .publish_exclusive("fid/vid", stream1, None)
        .await
        .expect("first publish_exclusive call must succeed");
    assert!(
        outcome1.created,
        "first publish_exclusive call to a fresh path must create it"
    );
    assert_eq!(
        b.get("fid/vid").await.unwrap(),
        Bytes::from_static(b"first")
    );

    let stream2: BoxStream<'_, std::io::Result<Bytes>> = Box::pin(stream::iter(vec![Ok(
        Bytes::from_static(b"second-different-bytes"),
    )]));
    let outcome2 = b
        .publish_exclusive("fid/vid", stream2, None)
        .await
        .expect("second publish_exclusive call must not error, just report created=false");
    assert!(
        !outcome2.created,
        "a second publish_exclusive call to an already-published path must not overwrite it"
    );
    // The measured bytes/digest of THIS (rejected) attempt are still
    // reported, so a caller can still run a server-side idempotency check.
    assert_eq!(
        outcome2.bytes_written,
        "second-different-bytes".len() as u64
    );

    // The backend must still hold the FIRST call's bytes, byte for byte —
    // this is the actual immutability guarantee under test.
    assert_eq!(
        b.get("fid/vid").await.unwrap(),
        Bytes::from_static(b"first"),
        "an already-published blob must never be overwritten by publish_exclusive"
    );

    drop(tokio::fs::remove_dir_all(&root).await);
}

/// Same guarantee as `local_fs_publish_exclusive_rejects_second_write_to_same_path`,
/// for `InMemoryBackend` (the check-then-insert happens under one lock guard,
/// so it is atomic rather than the trait default's TOCTOU fallback).
#[tokio::test]
async fn in_memory_publish_exclusive_rejects_second_write_to_same_path() {
    let b = InMemoryBackend::new("mem");

    let stream1: BoxStream<'_, std::io::Result<Bytes>> =
        Box::pin(stream::iter(vec![Ok(Bytes::from_static(b"first"))]));
    let outcome1 = b
        .publish_exclusive("fid/vid", stream1, None)
        .await
        .expect("first publish_exclusive call must succeed");
    assert!(
        outcome1.created,
        "first publish_exclusive call to a fresh path must create it"
    );

    let stream2: BoxStream<'_, std::io::Result<Bytes>> = Box::pin(stream::iter(vec![Ok(
        Bytes::from_static(b"second-different-bytes"),
    )]));
    let outcome2 = b
        .publish_exclusive("fid/vid", stream2, None)
        .await
        .expect("second publish_exclusive call must not error, just report created=false");
    assert!(
        !outcome2.created,
        "a second publish_exclusive call to an already-published path must not overwrite it"
    );

    assert_eq!(
        b.get("fid/vid").await.unwrap(),
        Bytes::from_static(b"first"),
        "an already-published blob must never be overwritten by publish_exclusive"
    );
}

/// Simulates a file truncated on disk between `get_stream`'s open (which
/// observes the length via `file.metadata()`) and the stream actually being
/// read: `LocalFsBackend` must surface `UnexpectedEof` on the resulting short
/// read rather than silently ending the body short of the length it already
/// committed to. No race here: a second, independent file handle truncates
/// the file while the stream's own handle is untouched, so its later read
/// deterministically lands past the new end-of-file.
#[tokio::test]
async fn local_fs_get_stream_errors_on_truncation_after_open() {
    let root = unique_root();
    let b = LocalFsBackend::new("fs", &root);

    b.put("fid/vid", Bytes::from(vec![7u8; 100])).await.unwrap();

    // Open the stream: this observes the current (100-byte) length.
    let mut stream = b.get_stream("fid/vid").await.unwrap();

    // Truncate the underlying file via a separate handle, after the stream
    // was opened but before anything is read from it.
    let target = root.join("fid").join("vid");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&target)
        .unwrap();
    file.set_len(10).unwrap();
    drop(file);

    let mut collected = Vec::new();
    let mut saw_eof_error = false;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => collected.extend_from_slice(&bytes),
            Err(e) => {
                assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof);
                saw_eof_error = true;
                break;
            }
        }
    }
    assert!(
        saw_eof_error,
        "a file truncated after get_stream opened it must error, not end silently short"
    );
    assert_eq!(
        collected.len(),
        10,
        "bytes actually readable before the truncated end must still be yielded"
    );

    drop(tokio::fs::remove_dir_all(&root).await);
}

#[tokio::test]
async fn registry_resolves_default_and_unknown() {
    let mem: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let reg = BackendRegistry::new(vec![mem], "mem").unwrap();
    assert_eq!(reg.default_id(), "mem");
    assert_eq!(reg.default_backend().id(), "mem");
    assert!(reg.get("mem").is_ok());
    assert!(reg.get("ghost").is_err());
    assert_eq!(reg.list().len(), 1);
}

#[test]
fn registry_rejects_absent_default() {
    let mem: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    assert!(BackendRegistry::new(vec![mem], "other").is_err());
}
