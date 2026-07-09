use std::sync::Arc;

use bytes::Bytes;
use file_storage_sdk::ByteRange;

use super::*;

fn unique_root() -> std::path::PathBuf {
    // No Math.random in scripts, but tests can use uuid + temp_dir.
    let mut p = std::env::temp_dir();
    p.push(format!("cf-fs-test-{}", uuid::Uuid::now_v7()));
    p
}

#[tokio::test]
async fn in_memory_put_get_round_trip() {
    let b = InMemoryBackend::new("mem");
    b.put("a/b", Bytes::from_static(b"hello")).await.unwrap();
    assert_eq!(b.get("a/b").await.unwrap(), Bytes::from_static(b"hello"));
    assert!(b.exists("a/b").await.unwrap());
}

#[tokio::test]
async fn in_memory_get_missing_errors() {
    let b = InMemoryBackend::new("mem");
    assert!(b.get("nope").await.is_err());
    assert!(!b.exists("nope").await.unwrap());
}

#[tokio::test]
async fn delete_is_idempotent() {
    let b = InMemoryBackend::new("mem");
    b.put("x", Bytes::from_static(b"y")).await.unwrap();
    b.delete("x").await.unwrap();
    // second delete on a now-missing blob still succeeds
    b.delete("x").await.unwrap();
    assert!(!b.exists("x").await.unwrap());
}

#[tokio::test]
async fn default_get_range_slices_content() {
    let b = InMemoryBackend::new("mem");
    b.put("p", Bytes::from_static(b"0123456789")).await.unwrap();
    let slice = b
        .get_range("p", ByteRange::Inclusive { start: 2, end: 4 })
        .await
        .unwrap();
    assert_eq!(slice, Bytes::from_static(b"234"));
}

#[tokio::test]
async fn get_range_suffix_returns_tail() {
    let b = InMemoryBackend::new("mem");
    b.put("p", Bytes::from_static(b"0123456789")).await.unwrap();
    let slice = b
        .get_range("p", ByteRange::Suffix { length: 3 })
        .await
        .unwrap();
    assert_eq!(slice, Bytes::from_static(b"789"));
}

#[tokio::test]
async fn local_fs_put_get_round_trip() {
    let root = unique_root();
    let b = LocalFsBackend::new("fs", &root);
    b.put("fid/vid", Bytes::from_static(b"bytes"))
        .await
        .unwrap();
    assert_eq!(
        b.get("fid/vid").await.unwrap(),
        Bytes::from_static(b"bytes")
    );
    assert!(b.exists("fid/vid").await.unwrap());
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
