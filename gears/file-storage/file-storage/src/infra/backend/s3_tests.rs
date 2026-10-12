//! `S3Backend` tests against an in-process `s3s-fs` server.

use std::net::SocketAddr;

use bytes::Bytes;
use file_storage_sdk::ByteRange;
use futures::stream::{self, BoxStream};
use tempfile::TempDir;

use super::S3Backend;
use crate::infra::backend::StorageBackend;
use crate::infra::backend::backend_tests::assert_backend_contract;
use crate::infra::content::hash;

const TEST_ACCESS_KEY: &str = "test-access-key";
const TEST_SECRET_KEY: &str = "test-secret-key";

/// In-process `s3s-fs` server on an ephemeral port; keep the returned `TempDir` alive.
async fn start_s3s_fs() -> (SocketAddr, TempDir) {
    let dir = tempfile::tempdir().expect("create temp dir for s3s-fs backing store");
    let fs = s3s_fs::FileSystem::new(dir.path()).expect("init s3s-fs FileSystem");

    let mut builder = s3s::service::S3ServiceBuilder::new(fs);
    builder.set_auth(s3s::auth::SimpleAuth::from_single(
        TEST_ACCESS_KEY,
        TEST_SECRET_KEY,
    ));
    let service = builder.build();

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind ephemeral port for s3s-fs test server");
    let local_addr = listener.local_addr().expect("resolve bound local addr");

    tokio::spawn(async move {
        let http_server =
            hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                continue;
            };
            let io = hyper_util::rt::TokioIo::new(socket);
            let conn = http_server
                .serve_connection(io, service.clone())
                .into_owned();
            tokio::spawn(async move {
                drop(conn.await);
            });
        }
    });

    (local_addr, dir)
}

/// `S3Backend` over a fresh `s3s-fs` server; the bucket dir is pre-created (not auto-created).
async fn make_backend(addr: SocketAddr, dir: &TempDir, bucket: &str) -> S3Backend {
    tokio::fs::create_dir_all(dir.path().join(bucket))
        .await
        .expect("pre-create s3s-fs bucket directory");
    let endpoint: url::Url = format!("http://{addr}")
        .parse()
        .expect("valid endpoint url");
    S3Backend::new(
        "s3-test",
        endpoint,
        "us-east-1",
        bucket,
        TEST_ACCESS_KEY,
        TEST_SECRET_KEY,
    )
    .expect("construct S3Backend")
}

fn unique_bucket() -> String {
    format!("test-{}", uuid::Uuid::now_v7())
}

#[tokio::test]
async fn s3_backend_put_get_round_trip() {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    let backend = make_backend(addr, &dir, &bucket).await;

    assert_backend_contract(&backend).await;

    let on_disk = dir.path().join(&bucket).join("contract").join("put-get");
    let raw = tokio::fs::read(&on_disk)
        .await
        .unwrap_or_else(|e| panic!("expected object at {on_disk:?}: {e}"));
    assert_eq!(raw, b"hello, contract");
}

/// A large object's `get_stream` chunks must reassemble to the bytes `get` returns.
#[tokio::test]
async fn s3_backend_get_stream_reassembles_large_object() {
    use futures::StreamExt;

    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    let backend = make_backend(addr, &dir, &bucket).await;

    let payload: Vec<u8> = (0..300_000)
        .map(|i| u8::try_from(i % 256).unwrap())
        .collect();
    backend
        .put("large/obj", Bytes::from(payload.clone()))
        .await
        .unwrap();

    let mut stream = backend.get_stream("large/obj").await.unwrap();
    let mut collected = Vec::new();
    while let Some(chunk) = stream.next().await {
        collected.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(collected, payload);
}

#[tokio::test]
async fn s3_backend_get_stream_missing_object_errors() {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    let backend = make_backend(addr, &dir, &bucket).await;

    assert!(backend.get_stream("nope/nope").await.is_err());
}

#[tokio::test]
async fn s3_backend_get_range_returns_native_partial_content() {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    let backend = make_backend(addr, &dir, &bucket).await;

    backend
        .put("range-obj", Bytes::from_static(b"0123456789abcdef"))
        .await
        .unwrap();

    let inclusive = backend
        .get_range("range-obj", ByteRange::Inclusive { start: 3, end: 7 })
        .await
        .unwrap();
    assert_eq!(inclusive, Bytes::from_static(b"34567"));

    let suffix = backend
        .get_range("range-obj", ByteRange::Suffix { length: 4 })
        .await
        .unwrap();
    assert_eq!(suffix, Bytes::from_static(b"cdef"));

    let open_ended = backend
        .get_range("range-obj", ByteRange::OpenEnded { start: 12 })
        .await
        .unwrap();
    assert_eq!(open_ended, Bytes::from_static(b"cdef"));
}

#[tokio::test]
async fn s3_backend_delete_is_idempotent() {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    let backend = make_backend(addr, &dir, &bucket).await;

    backend
        .put("to-delete", Bytes::from_static(b"gone soon"))
        .await
        .unwrap();
    backend.delete("to-delete").await.unwrap();
    // Second delete on an already-missing key: S3's DeleteObject returns a
    // success status regardless, so this must still be `Ok`.
    backend.delete("to-delete").await.unwrap();
    assert!(!backend.exists("to-delete").await.unwrap());
}

#[tokio::test]
async fn s3_backend_exists_distinguishes_missing_from_error() {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    let backend = make_backend(addr, &dir, &bucket).await;

    assert!(!backend.exists("never-uploaded").await.unwrap());

    backend
        .put("now-present", Bytes::from_static(b"x"))
        .await
        .unwrap();
    assert!(backend.exists("now-present").await.unwrap());
}

#[tokio::test]
async fn s3_is_ready_ok_against_s3s_fs() {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    let backend = make_backend(addr, &dir, &bucket).await;

    backend
        .is_ready()
        .await
        .expect("is_ready must succeed against a reachable, authenticated endpoint");
}

#[tokio::test]
async fn s3_is_ready_err_against_closed_port() {
    // Bind then drop: the port is closed, so the probe gets connection-refused.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("resolve bound local addr");
    drop(listener);

    let endpoint: url::Url = format!("http://{addr}")
        .parse()
        .expect("valid endpoint url");
    let backend = S3Backend::new(
        "s3-test",
        endpoint,
        "us-east-1",
        "irrelevant-bucket",
        TEST_ACCESS_KEY,
        TEST_SECRET_KEY,
    )
    .expect("construct S3Backend");

    backend
        .is_ready()
        .await
        .expect_err("is_ready must fail against an unreachable endpoint");
}

/// `is_ready` must fail when the bucket does not exist: `NoSuchBucket` from `ListObjectsV2`
/// is distinguishable from an absent key (unlike `HeadObject`). Bypasses `make_backend`.
#[tokio::test]
async fn s3_is_ready_err_against_missing_bucket() {
    let (addr, _dir) = start_s3s_fs().await;
    let endpoint: url::Url = format!("http://{addr}")
        .parse()
        .expect("valid endpoint url");
    let backend = S3Backend::new(
        "s3-test",
        endpoint,
        "us-east-1",
        unique_bucket(),
        TEST_ACCESS_KEY,
        TEST_SECRET_KEY,
    )
    .expect("construct S3Backend");

    backend
        .is_ready()
        .await
        .expect_err("is_ready must fail when the target bucket does not exist");
}

#[tokio::test]
async fn s3_backend_list_paths_paginates_across_continuation_token() {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    // Page size 2 over 5 objects forces at least 3 `ListObjectsV2` pages.
    let backend = make_backend(addr, &dir, &bucket)
        .await
        .with_list_page_size(2);

    let mut expected: Vec<String> = Vec::new();
    for i in 0..5 {
        let path = format!("file-{i}/version-{i}");
        backend
            .put(&path, Bytes::from(format!("payload-{i}").into_bytes()))
            .await
            .unwrap();
        expected.push(format!("/{path}"));
    }

    let mut got = backend.list_paths().await.unwrap();
    got.sort();
    expected.sort();
    assert_eq!(got, expected);
}

#[tokio::test]
async fn s3_backend_multipart_initiate_upload_complete_round_trip() {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    let backend = make_backend(addr, &dir, &bucket).await;

    // S3 minimum part size is 5 MiB except the last; distinct patterns detect mis-ordering.
    let part_size = 5 * 1024 * 1024;
    let part1 = vec![b'a'; part_size];
    let part2 = vec![b'b'; part_size];
    let part3 = vec![b'c'; 1024]; // last part, below the minimum is fine

    let path = "multipart/round-trip";
    let upload_handle = backend.initiate_multipart(path).await.unwrap();

    // `upload_part` takes each part's byte offset in the assembled object.
    let off1 = 0u64;
    let off2 = part_size as u64;
    let off3 = 2 * part_size as u64;
    let (etag1, hash1) = backend
        .upload_part(path, &upload_handle, 1, off1, Bytes::from(part1.clone()))
        .await
        .unwrap();
    let (etag2, hash2) = backend
        .upload_part(path, &upload_handle, 2, off2, Bytes::from(part2.clone()))
        .await
        .unwrap();
    let (etag3, hash3) = backend
        .upload_part(path, &upload_handle, 3, off3, Bytes::from(part3.clone()))
        .await
        .unwrap();

    // The part hash is this gear's SHA-256, not S3's MD5 ETag.
    assert_eq!(hash1, hash::sha256(&part1));
    assert_eq!(hash2, hash::sha256(&part2));
    assert_eq!(hash3, hash::sha256(&part3));

    let to_arr = |v: Vec<u8>| -> [u8; 32] { v.try_into().unwrap() };
    // Deliberately out of order — `complete_multipart` sorts by offset.
    let completion_parts = vec![
        (3u32, off3, to_arr(hash3.clone()), etag3),
        (1u32, off1, to_arr(hash1.clone()), etag1),
        (2u32, off2, to_arr(hash2.clone()), etag2),
    ];
    let (manifest, root) = backend
        .complete_multipart(path, &upload_handle, &completion_parts)
        .await
        .unwrap();

    // The stored digest is `sha256(manifest)` (offset-manifest composite), not of the bytes.
    let expected_manifest = crate::infra::content::hash_mode::Manifest::new(vec![
        crate::infra::content::hash_mode::ManifestEntry {
            offset: off1,
            digest: to_arr(hash::sha256(&part1)),
        },
        crate::infra::content::hash_mode::ManifestEntry {
            offset: off2,
            digest: to_arr(hash::sha256(&part2)),
        },
        crate::infra::content::hash_mode::ManifestEntry {
            offset: off3,
            digest: to_arr(hash::sha256(&part3)),
        },
    ])
    .unwrap();
    assert_eq!(
        manifest.to_wire_string(),
        expected_manifest.to_wire_string()
    );
    assert_eq!(root, expected_manifest.root());

    let mut expected_bytes = Vec::with_capacity(part1.len() + part2.len() + part3.len());
    expected_bytes.extend_from_slice(&part1);
    expected_bytes.extend_from_slice(&part2);
    expected_bytes.extend_from_slice(&part3);
    let got = backend.get(path).await.unwrap();
    assert_eq!(got.as_ref(), expected_bytes.as_slice());

    let on_disk = dir
        .path()
        .join(&bucket)
        .join("multipart")
        .join("round-trip");
    let raw = tokio::fs::read(&on_disk)
        .await
        .unwrap_or_else(|e| panic!("expected object at {on_disk:?}: {e}"));
    assert_eq!(raw, expected_bytes);
}

#[tokio::test]
async fn s3_backend_multipart_abort_discards_parts() {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    let backend = make_backend(addr, &dir, &bucket).await;

    let path = "multipart/aborted";
    let upload_handle = backend.initiate_multipart(path).await.unwrap();
    backend
        .upload_part(
            path,
            &upload_handle,
            1,
            0,
            Bytes::from_static(b"never completed"),
        )
        .await
        .unwrap();

    backend.abort_multipart(path, &upload_handle).await.unwrap();

    assert!(backend.get(path).await.is_err());
    assert!(!backend.exists(path).await.unwrap());
}

#[tokio::test]
async fn s3_backend_upload_part_rejects_part_number_outside_s3_limits() {
    // Validation precedes any network I/O, so no server is needed.
    let endpoint: url::Url = "http://127.0.0.1:1".parse().expect("valid endpoint url");
    let backend = S3Backend::new(
        "s3-test",
        endpoint,
        "us-east-1",
        "unused-bucket",
        TEST_ACCESS_KEY,
        TEST_SECRET_KEY,
    )
    .expect("construct S3Backend");

    let over_limit = backend
        .upload_part("some/path", "handle", 10_001, 0, Bytes::from_static(b"x"))
        .await;
    assert!(
        over_limit.is_err(),
        "part_number 10_001 exceeds S3's documented 10,000-part maximum and must be rejected"
    );

    let zero = backend
        .upload_part("some/path", "handle", 0, 0, Bytes::from_static(b"x"))
        .await;
    assert!(
        zero.is_err(),
        "part_number 0 is below S3's 1-indexed minimum and must be rejected"
    );
}

fn chunk_stream(chunks: Vec<Bytes>) -> BoxStream<'static, std::io::Result<Bytes>> {
    Box::pin(stream::iter(chunks.into_iter().map(Ok)))
}

#[tokio::test]
async fn s3_backend_put_stream_small_uses_single_put() {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    // Default multipart threshold (8 MiB): this stream stays below it, so a single `PutObject`.
    let backend = make_backend(addr, &dir, &bucket).await;

    let chunk_bytes: Vec<&'static [u8]> = vec![b"small ", b"stream ", b"payload"];
    let concatenated: Vec<u8> = chunk_bytes.concat();
    let total_len = concatenated.len() as u64;
    let chunks: Vec<Bytes> = chunk_bytes.into_iter().map(Bytes::from_static).collect();

    let path = "put-stream/small";
    let (bytes_written, digest) = backend
        .put_stream(path, chunk_stream(chunks), None)
        .await
        .expect("put_stream should succeed for a small stream");

    assert_eq!(bytes_written, total_len);
    assert_eq!(digest, hash::digest_to_array(hash::sha256(&concatenated)));

    let got = backend.get(path).await.unwrap();
    assert_eq!(got.as_ref(), concatenated.as_slice());

    let on_disk = dir.path().join(&bucket).join("put-stream").join("small");
    let raw = tokio::fs::read(&on_disk)
        .await
        .unwrap_or_else(|e| panic!("expected object at {on_disk:?}: {e}"));
    assert_eq!(raw, concatenated);
}

#[tokio::test]
async fn s3_backend_put_stream_large_uses_multipart() {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    // Low threshold so a multi-MiB stream crosses it with full 5 MiB parts.
    let part_size: u64 = 5 * 1024 * 1024;
    let backend = make_backend(addr, &dir, &bucket)
        .await
        .with_multipart_threshold_bytes(part_size);

    // 11 MiB in 1 MiB chunks: two 5 MiB parts plus a 1 MiB tail.
    let chunk_size = 1024 * 1024;
    let num_chunks: u8 = 11;
    let chunks: Vec<Bytes> = (0..num_chunks)
        .map(|i| Bytes::from(vec![b'a' + i; chunk_size]))
        .collect();
    let concatenated: Vec<u8> = chunks.iter().flat_map(|c| c.to_vec()).collect();
    let total_len = concatenated.len() as u64;

    let path = "put-stream/large";
    let (bytes_written, digest) = backend
        .put_stream(path, chunk_stream(chunks), None)
        .await
        .expect("put_stream should succeed for a large multipart stream");

    assert_eq!(bytes_written, total_len);
    assert_eq!(digest, hash::digest_to_array(hash::sha256(&concatenated)));

    let got = backend.get(path).await.unwrap();
    assert_eq!(got.as_ref(), concatenated.as_slice());

    // The digest `put_stream` returned must match a hash of the stored bytes.
    assert_eq!(digest, hash::digest_to_array(hash::sha256(&got)));
}

#[tokio::test]
async fn s3_backend_put_stream_enforces_max_size_mid_stream() {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    // Threshold 8 bytes: the first 10-byte chunk starts multipart, the second exceeds
    // `max_size`, so an already-initiated session must be aborted.
    let backend = make_backend(addr, &dir, &bucket)
        .await
        .with_multipart_threshold_bytes(8);

    let chunks: Vec<Bytes> = vec![
        Bytes::from_static(b"0123456789"),
        Bytes::from_static(b"0123456789"),
        Bytes::from_static(b"0123456789"),
    ];

    let path = "put-stream/rejected";
    let result = backend
        .put_stream(path, chunk_stream(chunks), Some(15))
        .await;

    assert!(
        result.is_err(),
        "put_stream must reject a stream exceeding max_size"
    );

    // Nothing may be left behind: no object, and the multipart session was aborted.
    assert!(!backend.exists(path).await.unwrap());
    assert!(backend.get(path).await.is_err());
}
