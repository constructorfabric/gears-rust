use std::sync::Arc;

use bytes::Bytes;
use file_storage_sdk::ByteRange;
use futures::StreamExt;
use futures::stream::{self, BoxStream};

use crate::infra::content::hash;

use super::*;

fn unique_root() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("cf-fs-test-{}", uuid::Uuid::now_v7()));
    p
}

/// `get_stream` chunks concatenated must equal `get` for every backend.
async fn assert_get_stream_matches_get(backend: &dyn StorageBackend, path: &str, expected: &[u8]) {
    let mut stream = backend.get_stream(path).await.unwrap();
    let mut streamed = Vec::new();
    while let Some(chunk) = stream.next().await {
        streamed.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(streamed, expected);
}

/// Behavioral contract every `StorageBackend` implementation must satisfy.
pub async fn assert_backend_contract(backend: &dyn StorageBackend) {
    backend
        .put("contract/put-get", Bytes::from_static(b"hello, contract"))
        .await
        .unwrap();
    assert_eq!(
        backend.get("contract/put-get").await.unwrap(),
        Bytes::from_static(b"hello, contract")
    );
    assert!(backend.exists("contract/put-get").await.unwrap());

    assert_get_stream_matches_get(backend, "contract/put-get", b"hello, contract").await;

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

    backend
        .put("contract/delete", Bytes::from_static(b"x"))
        .await
        .unwrap();
    backend.delete("contract/delete").await.unwrap();
    backend.delete("contract/delete").await.unwrap();
    assert!(!backend.exists("contract/delete").await.unwrap());

    assert!(!backend.exists("contract/never-existed").await.unwrap());
    assert_missing_object_errors(backend, "contract/never-existed").await;
}

/// Reading an object that was never written is an error on every read path.
async fn assert_missing_object_errors(backend: &dyn StorageBackend, path: &str) {
    assert!(backend.get(path).await.is_err());
    assert!(backend.get_stream(path).await.is_err());
}

/// A stream crossing `max_size` mid-way is rejected by `put_stream` and nothing is stored.
pub async fn assert_put_stream_rejects_oversized_stream(
    backend: &dyn StorageBackend,
    path: &str,
    max_size: u64,
) {
    // Three 10-byte chunks: the limit is crossed on the second chunk for any `max_size` < 20.
    let chunks: Vec<std::io::Result<Bytes>> = (0..3)
        .map(|_| Ok(Bytes::from_static(b"0123456789")))
        .collect();
    let stream: BoxStream<'_, std::io::Result<Bytes>> = Box::pin(stream::iter(chunks));

    let result = backend.put_stream(path, stream, Some(max_size)).await;
    assert!(
        result.is_err(),
        "put_stream must reject a stream exceeding max_size"
    );
    assert!(!backend.exists(path).await.unwrap());
    assert!(backend.get(path).await.is_err());
}

#[tokio::test]
async fn in_memory_satisfies_backend_contract() {
    let b = InMemoryBackend::new("mem");
    assert_backend_contract(&b).await;
}

#[tokio::test]
async fn local_fs_satisfies_backend_contract() {
    let root = unique_root();
    let b = LocalFsBackend::new("fs", &root);
    assert_backend_contract(&b).await;
    drop(tokio::fs::remove_dir_all(&root).await);
}

#[tokio::test]
async fn local_fs_is_ready_self_tests_hard_links_and_leaves_no_probe_files() {
    let root = unique_root();
    tokio::fs::create_dir_all(&root).await.unwrap();
    let b = LocalFsBackend::new("fs", &root);
    b.is_ready().await.expect("hard links work on a regular fs");
    // Cached: a second call succeeds without touching the fs again.
    b.is_ready().await.expect("cached self-test result");
    let mut entries = tokio::fs::read_dir(&root).await.unwrap();
    assert!(
        entries.next_entry().await.unwrap().is_none(),
        "probe files must be removed"
    );
    drop(tokio::fs::remove_dir_all(&root).await);
}

#[cfg(unix)]
async fn set_mode(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .await
        .unwrap();
}

/// A failed self-test is not remembered: once the root is writable again, `is_ready` passes; a
/// success is cached even if the root turns read-only afterwards. No probe files are left.
#[cfg(unix)]
#[tokio::test]
async fn local_fs_is_ready_caches_only_success() {
    let root = unique_root();
    tokio::fs::create_dir_all(&root).await.unwrap();
    set_mode(&root, 0o555).await;
    // Root (or a permissive fs) ignores the mode: nothing to test.
    if tokio::fs::write(root.join(".writable-check"), b"x")
        .await
        .is_ok()
    {
        set_mode(&root, 0o755).await;
        drop(tokio::fs::remove_dir_all(&root).await);
        return;
    }

    let b = LocalFsBackend::new("fs", &root);
    let err = b.is_ready().await.expect_err("read-only root must fail");
    assert!(
        err.to_string().contains("cannot write a probe file"),
        "unexpected error: {err}"
    );
    b.is_ready().await.expect_err("the failure is retried");

    set_mode(&root, 0o755).await;
    b.is_ready()
        .await
        .expect("passes once the root is writable");

    set_mode(&root, 0o555).await;
    b.is_ready().await.expect("success is cached");
    set_mode(&root, 0o755).await;

    let mut entries = tokio::fs::read_dir(&root).await.unwrap();
    assert!(
        entries.next_entry().await.unwrap().is_none(),
        "no probe files may remain"
    );
    drop(tokio::fs::remove_dir_all(&root).await);
}

#[test]
fn local_fs_io_errors_classify_transient_vs_permanent() {
    use std::io::{Error, ErrorKind};
    let b = LocalFsBackend::new("fs", "/nonexistent");
    for kind in [
        ErrorKind::TimedOut,
        ErrorKind::Interrupted,
        ErrorKind::WouldBlock,
    ] {
        assert!(matches!(
            b.io_err(&Error::from(kind)),
            DomainError::BackendUnavailable { .. }
        ));
    }
    assert!(matches!(
        b.io_err(&Error::from(ErrorKind::PermissionDenied)),
        DomainError::Backend { .. }
    ));
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

    // Distinct byte pattern per payload so a torn or mixed result is detectable.
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

    // Strip the write bit from the pre-created parent dir: the temp file create fails
    // before the atomic rename runs.
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

    // Restore permissions so temp-dir cleanup can remove it.
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    drop(tokio::fs::remove_dir_all(&root).await);
}

/// A stream crossing `max_size` mid-way is rejected and leaves no file or tmp file behind.
#[tokio::test]
async fn local_fs_put_stream_enforces_max_size_mid_stream() {
    let root = unique_root();
    let b = LocalFsBackend::new("fs", &root);

    assert_put_stream_rejects_oversized_stream(&b, "fid/vid", 15).await;

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

/// `put_stream`'s incremental hash must equal `hash::sha256` of the concatenated bytes.
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

    assert_eq!(b.get("fid2/vid2").await.unwrap(), Bytes::from(concatenated));

    drop(tokio::fs::remove_dir_all(&root).await);
}

/// A blob spanning several 64 KiB read chunks must reassemble exactly via `get_stream`.
#[tokio::test]
async fn local_fs_get_stream_reassembles_multi_chunk_blob() {
    let root = unique_root();
    let b = LocalFsBackend::new("fs", &root);

    // 200 KB: more than one 64 KiB chunk.
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

/// A second `publish_exclusive` to one path reports `created: false` and keeps the first bytes.
async fn assert_publish_exclusive_rejects_second_write(backend: &dyn StorageBackend) {
    let stream1: BoxStream<'_, std::io::Result<Bytes>> =
        Box::pin(stream::iter(vec![Ok(Bytes::from_static(b"first"))]));
    let outcome1 = backend
        .publish_exclusive("fid/vid", stream1, None)
        .await
        .expect("first publish_exclusive call must succeed");
    assert!(
        outcome1.created,
        "first publish_exclusive call to a fresh path must create it"
    );
    assert_eq!(
        backend.get("fid/vid").await.unwrap(),
        Bytes::from_static(b"first")
    );

    let stream2: BoxStream<'_, std::io::Result<Bytes>> = Box::pin(stream::iter(vec![Ok(
        Bytes::from_static(b"second-different-bytes"),
    )]));
    let outcome2 = backend
        .publish_exclusive("fid/vid", stream2, None)
        .await
        .expect("second publish_exclusive call must not error, just report created=false");
    assert!(
        !outcome2.created,
        "a second publish_exclusive call to an already-published path must not overwrite it"
    );
    // The rejected attempt's measured bytes/digest are still reported (for idempotency checks).
    assert_eq!(
        outcome2.bytes_written,
        "second-different-bytes".len() as u64
    );

    assert_eq!(
        backend.get("fid/vid").await.unwrap(),
        Bytes::from_static(b"first"),
        "an already-published blob must never be overwritten by publish_exclusive"
    );
}

#[tokio::test]
async fn local_fs_publish_exclusive_rejects_second_write_to_same_path() {
    let root = unique_root();
    assert_publish_exclusive_rejects_second_write(&LocalFsBackend::new("fs", &root)).await;
    drop(tokio::fs::remove_dir_all(&root).await);
}

/// `InMemoryBackend` counterpart; its check-then-insert is atomic under one lock guard.
#[tokio::test]
async fn in_memory_publish_exclusive_rejects_second_write_to_same_path() {
    assert_publish_exclusive_rejects_second_write(&InMemoryBackend::new("mem")).await;
}

/// Two barrier-released `publish_exclusive` racers on one path: exactly one `created: true`,
/// and the stored object is one payload in full, never a torn mix. Repeated to hit the overlap.
async fn assert_publish_exclusive_concurrent_racers_never_mix(
    make_backend: impl Fn() -> Arc<dyn StorageBackend>,
) {
    const SIZE: usize = 64 * 1024;
    const ITERATIONS: u32 = 20;

    for iteration in 0..ITERATIONS {
        let backend = make_backend();
        let path = format!("fid/vid-{iteration}");

        let payload_a = Bytes::from(vec![0xAAu8; SIZE]);
        let payload_b = Bytes::from(vec![0xBBu8; SIZE]);
        let barrier = Arc::new(tokio::sync::Barrier::new(2));

        let task = |payload: Bytes,
                    backend: Arc<dyn StorageBackend>,
                    path: String,
                    barrier: Arc<tokio::sync::Barrier>| async move {
            let len = payload.len() as u64;
            let stream: BoxStream<'_, std::io::Result<Bytes>> =
                Box::pin(stream::once(async move { Ok(payload) }));
            // Rendezvous so both calls start as close to simultaneously as possible.
            barrier.wait().await;
            backend.publish_exclusive(&path, stream, Some(len)).await
        };

        let handle_a = tokio::spawn(task(
            payload_a.clone(),
            Arc::clone(&backend),
            path.clone(),
            Arc::clone(&barrier),
        ));
        let handle_b = tokio::spawn(task(
            payload_b.clone(),
            Arc::clone(&backend),
            path.clone(),
            Arc::clone(&barrier),
        ));

        let outcome_a = handle_a
            .await
            .expect("racer A task must not panic")
            .expect("racer A's publish_exclusive must not error");
        let outcome_b = handle_b
            .await
            .expect("racer B task must not panic")
            .expect("racer B's publish_exclusive must not error");

        assert_ne!(
            outcome_a.created, outcome_b.created,
            "iteration {iteration}: exactly one racer must report created:true and the \
             other created:false, got ({}, {})",
            outcome_a.created, outcome_b.created
        );

        let got = backend.get(&path).await.unwrap();
        assert!(
            got == payload_a || got == payload_b,
            "iteration {iteration}: the stored object must equal exactly one racer's full \
             payload, never a mix of both"
        );
        if outcome_a.created {
            assert_eq!(
                got, payload_a,
                "iteration {iteration}: racer A reported created:true but the stored \
                 object does not match its payload"
            );
        } else {
            assert_eq!(
                got, payload_b,
                "iteration {iteration}: racer B reported created:true but the stored \
                 object does not match its payload"
            );
        }
    }
}

#[tokio::test]
async fn local_fs_publish_exclusive_concurrent_racers_never_mix() {
    let root = unique_root();
    assert_publish_exclusive_concurrent_racers_never_mix(|| {
        Arc::new(LocalFsBackend::new("fs", &root)) as Arc<dyn StorageBackend>
    })
    .await;
    drop(tokio::fs::remove_dir_all(&root).await);
}

#[tokio::test]
async fn in_memory_publish_exclusive_concurrent_racers_never_mix() {
    assert_publish_exclusive_concurrent_racers_never_mix(|| {
        Arc::new(InMemoryBackend::new("mem")) as Arc<dyn StorageBackend>
    })
    .await;
}

/// A chunk-level I/O error surfaces as `DomainError::Backend` and nothing is published.
#[tokio::test]
async fn in_memory_publish_exclusive_propagates_chunk_error_and_publishes_nothing() {
    let b = InMemoryBackend::new("mem");

    let chunks: Vec<std::io::Result<Bytes>> = vec![
        Ok(Bytes::from_static(b"good-chunk-1")),
        Err(std::io::Error::other("simulated stream failure")),
    ];
    let stream: BoxStream<'_, std::io::Result<Bytes>> = Box::pin(stream::iter(chunks));

    let result = b.publish_exclusive("fid/vid", stream, None).await;
    match result {
        Ok(_) => panic!("a stream that errors partway through must not be published successfully"),
        Err(DomainError::Backend { backend_id, .. }) => {
            assert_eq!(
                backend_id, "mem",
                "the backend error must be attributed to the failing backend's id"
            );
        }
        Err(e) => panic!("expected a DomainError::Backend, got {e:?}"),
    }

    assert!(
        !b.exists("fid/vid").await.unwrap(),
        "a publish_exclusive call whose stream errors must not leave a partial object behind"
    );
}

/// `max_size` is enforced as chunks arrive: `DomainError::Validation` and nothing published.
#[tokio::test]
async fn in_memory_publish_exclusive_enforces_max_size_and_publishes_nothing() {
    let b = InMemoryBackend::new("mem");

    let chunks: Vec<std::io::Result<Bytes>> = vec![
        Ok(Bytes::from_static(b"0123456789")),
        Ok(Bytes::from_static(b"0123456789")),
    ];
    let stream: BoxStream<'_, std::io::Result<Bytes>> = Box::pin(stream::iter(chunks));

    let result = b.publish_exclusive("fid/vid", stream, Some(15)).await;
    match result {
        Ok(_) => panic!("a stream exceeding max_size must be rejected, not published"),
        Err(DomainError::Validation { field, .. }) => {
            assert_eq!(
                field, "size",
                "the rejection must be attributed to the size field"
            );
        }
        Err(e) => panic!("expected a DomainError::Validation, got {e:?}"),
    }

    assert!(
        !b.exists("fid/vid").await.unwrap(),
        "a rejected publish_exclusive must not leave a partial object behind"
    );
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
