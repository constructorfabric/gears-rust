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

/// A stream crossing `max_size` mid-way is rejected and leaves no file at the target.
#[tokio::test]
async fn local_fs_put_stream_enforces_max_size_mid_stream() {
    let root = unique_root();
    let b = LocalFsBackend::new("fs", &root);

    // Three 10-byte chunks against max_size 15: the limit is crossed on the second chunk.
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

#[tokio::test]
async fn local_fs_get_stream_missing_errors() {
    let root = unique_root();
    let b = LocalFsBackend::new("fs", &root);
    assert!(b.get_stream("nope/nope").await.is_err());
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
