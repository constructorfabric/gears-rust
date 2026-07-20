//! `S3Backend` tests, run against an in-process `s3s-fs` (filesystem-backed
//! S3-compatible HTTP) server — no external infra required.

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

/// Start an in-process `s3s-fs` server bound to an ephemeral port. `s3s-fs`
/// serves a hyper/tower `S3Service` (not axum), so connections are accepted
/// manually via a `hyper_util` auto (H1/H2) connection builder, mirroring
/// `s3s-fs`'s own `main.rs` binary. The returned `TempDir` is `s3s-fs`'s
/// backing filesystem root — it must be kept alive for the caller's test
/// duration (dropping it deletes the backing directory).
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

/// Build an `S3Backend` pointed at a freshly started `s3s-fs` server, with
/// `bucket`'s backing directory pre-created (`s3s-fs` does not auto-create
/// buckets — a bucket is just a top-level directory under its root).
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

    // Secondary/state-artifact check: bytes are physically present under the
    // expected key in s3s-fs's on-disk layout (`<root>/<bucket>/<key>`).
    let on_disk = dir.path().join(&bucket).join("contract").join("put-get");
    let raw = tokio::fs::read(&on_disk)
        .await
        .unwrap_or_else(|e| panic!("expected object at {on_disk:?}: {e}"));
    assert_eq!(raw, b"hello, contract");
}

/// `get_stream`'s eagerly-sent `GetObject` + `bytes_stream()` chunks must
/// reassemble to the exact same bytes `get` returns, for an object large
/// enough that a real HTTP response is plausibly delivered across more than
/// one chunk (`assert_backend_contract`'s own `get_stream` check already
/// covers the small-object case).
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

/// P2 1.6: `is_ready` against a reachable, correctly-authenticated `s3s-fs`
/// endpoint with an existing (empty) bucket must succeed — `ListObjectsV2`
/// returns `200` with an empty listing, which `is_ready` must treat as
/// "ready".
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

/// P2 1.6: `is_ready` against an endpoint nothing listens on must fail
/// (transport error), not silently report ready.
#[tokio::test]
async fn s3_is_ready_err_against_closed_port() {
    // Bind then immediately drop: reserves an ephemeral port that is
    // guaranteed closed (nothing listens) for the rest of the test, so the
    // backend's probe request hits a connection-refused transport error.
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

/// Regression test for the bucket-missing false positive: `is_ready` must
/// fail against a reachable, correctly-authenticated endpoint whose target
/// bucket does not exist. Deliberately does NOT go through `make_backend`
/// (which pre-creates the bucket directory) — `s3s-fs` has no bucket at this
/// path, so `HeadObject` and `ListObjectsV2` both 404, but only
/// `ListObjectsV2`'s `NoSuchBucket` can be told apart from a merely-absent
/// key. Before the `ListObjectsV2`-based probe, this same 404 was folded
/// into `exists`'s "key absent" `Ok(false)` and `is_ready` reported ready.
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
    // A tiny page size (2) against 5 seeded objects forces at least 3
    // `ListObjectsV2` pages, actually exercising the continuation-token loop.
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

    // S3's minimum part size is 5 MiB, except for the last part — use
    // distinct byte patterns per part so a mis-ordered assembly is
    // detectable, keeping the first two parts at the 5 MiB minimum.
    let part_size = 5 * 1024 * 1024;
    let part1 = vec![b'a'; part_size];
    let part2 = vec![b'b'; part_size];
    let part3 = vec![b'c'; 1024]; // last part, below the minimum is fine

    let path = "multipart/round-trip";
    let upload_handle = backend.initiate_multipart(path).await.unwrap();

    // ADR-0006: `upload_part` now takes each part's byte offset within the
    // assembled object (part1 @ 0, part2 @ 5 MiB, part3 @ 10 MiB).
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

    // Each part's returned hash is this gear's own SHA-256 of that part's
    // bytes, not S3's MD5-based ETag.
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

    // ADR-0006 mode 2: the stored digest is `sha256(manifest)`, an
    // offset-manifest composite — NOT `sha256(assembled bytes)`. Build the
    // expected manifest independently from the known per-part digests/offsets.
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

    // A subsequent `get()` returns the full assembled object.
    let mut expected_bytes = Vec::with_capacity(part1.len() + part2.len() + part3.len());
    expected_bytes.extend_from_slice(&part1);
    expected_bytes.extend_from_slice(&part2);
    expected_bytes.extend_from_slice(&part3);
    let got = backend.get(path).await.unwrap();
    assert_eq!(got.as_ref(), expected_bytes.as_slice());

    // Secondary/state-artifact check: the s3s-fs backing file matches.
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

    // The object was never completed, so it must not exist.
    assert!(backend.get(path).await.is_err());
    assert!(!backend.exists(path).await.unwrap());
}

#[tokio::test]
async fn s3_backend_upload_part_rejects_part_number_outside_s3_limits() {
    // Validation happens before any network I/O, so this deliberately does
    // not start an `s3s-fs` server — an unreachable endpoint is enough to
    // prove the rejection never gets as far as signing/sending a request.
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

/// Box a fixed set of chunks into the `BoxStream` shape `put_stream` expects.
fn chunk_stream(chunks: Vec<Bytes>) -> BoxStream<'static, std::io::Result<Bytes>> {
    Box::pin(stream::iter(chunks.into_iter().map(Ok)))
}

#[tokio::test]
async fn s3_backend_put_stream_small_uses_single_put() {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    // Default multipart threshold (8 MiB) — this stream stays well under it,
    // so `put_stream` must take the single-`PutObject` path.
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

    // Secondary/state-artifact check: a single plain object landed on disk
    // (no multipart session was ever initiated), matching s3s-fs's on-disk
    // layout for a regular `PutObject`.
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
    // A small-enough threshold that a real multi-MiB stream crosses it across
    // multiple full-size (>= S3's 5 MiB minimum) parts.
    let part_size: u64 = 5 * 1024 * 1024;
    let backend = make_backend(addr, &dir, &bucket)
        .await
        .with_multipart_threshold_bytes(part_size);

    // 11 MiB total, fed in 1 MiB chunks: two full 5 MiB parts plus a 1 MiB
    // tail part, comfortably exercising the multipart path across >= 2 parts
    // of >= 5 MiB each.
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

    // `get()` returns the fully assembled object, matching the concatenated
    // input exactly.
    let got = backend.get(path).await.unwrap();
    assert_eq!(got.as_ref(), concatenated.as_slice());

    // The incrementally-computed digest `put_stream` returned must agree
    // with a hash computed directly over the actually-stored (re-read)
    // bytes — i.e. `put_stream`'s incremental hash and `complete_multipart`'s
    // own re-read-and-hash never disagree.
    assert_eq!(digest, hash::digest_to_array(hash::sha256(&got)));
}

#[tokio::test]
async fn s3_backend_put_stream_enforces_max_size_mid_stream() {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    // A small threshold (8 bytes) so the first 10-byte chunk alone already
    // initiates a multipart upload (and flushes one full part) before the
    // second chunk pushes the running total past `max_size` — exercising the
    // "abort an already-initiated multipart session" cleanup path, not just
    // the "never even start multipart" one.
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

    // Nothing must be left behind: no completed object, and (implicitly) the
    // multipart session initiated for the first chunk was aborted rather
    // than left dangling.
    assert!(!backend.exists(path).await.unwrap());
    assert!(backend.get(path).await.is_err());
}

#[tokio::test]
async fn s3_backend_publish_exclusive_single_put_rejects_overwrite() {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    // Stays under the default 8 MiB threshold → single-`PutObject` path with
    // `If-None-Match: *`.
    let backend = make_backend(addr, &dir, &bucket).await;

    let path = "publish-exclusive/small";
    let first: &[u8] = b"the original immutable blob";
    let second: &[u8] = b"a replay attempt with different bytes";

    // First publish to a fresh path creates the object.
    let outcome = backend
        .publish_exclusive(path, chunk_stream(vec![Bytes::from_static(first)]), None)
        .await
        .expect("first publish_exclusive should succeed");
    assert!(outcome.created, "first publish must report created = true");
    assert_eq!(outcome.bytes_written, first.len() as u64);
    assert_eq!(outcome.digest, hash::digest_to_array(hash::sha256(first)));

    // A second publish to the same path (a replayed PUT token) must NOT
    // overwrite: s3s-fs honours `If-None-Match: *` with a 412, which maps to
    // `created = false`. `bytes_written`/`digest` still describe this attempt.
    let outcome2 = backend
        .publish_exclusive(path, chunk_stream(vec![Bytes::from_static(second)]), None)
        .await
        .expect("second publish_exclusive must return Ok(created=false), not an error");
    assert!(
        !outcome2.created,
        "second publish to an existing path must report created = false"
    );
    assert_eq!(outcome2.bytes_written, second.len() as u64);

    // The immutability guarantee: the stored bytes are still the FIRST blob.
    let got = backend.get(path).await.unwrap();
    assert_eq!(
        got.as_ref(),
        first,
        "the original bytes must survive the rejected overwrite"
    );
}

#[tokio::test]
async fn s3_backend_publish_exclusive_multipart_rejects_overwrite() {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    // Small threshold so an 11 MiB stream takes the native-multipart path,
    // whose terminal `CompleteMultipartUpload` carries `If-None-Match: *`.
    let part_size: u64 = 5 * 1024 * 1024;
    let backend = make_backend(addr, &dir, &bucket)
        .await
        .with_multipart_threshold_bytes(part_size);

    let chunk_size = 1024 * 1024;
    let num_chunks: u8 = 11;
    let first_chunks: Vec<Bytes> = (0..num_chunks)
        .map(|i| Bytes::from(vec![b'a' + i; chunk_size]))
        .collect();
    let first_concat: Vec<u8> = first_chunks.iter().flat_map(|c| c.to_vec()).collect();

    let path = "publish-exclusive/large";
    let outcome = backend
        .publish_exclusive(path, chunk_stream(first_chunks), None)
        .await
        .expect("first multipart publish_exclusive should succeed");
    assert!(outcome.created, "first multipart publish must create");
    assert_eq!(outcome.bytes_written, first_concat.len() as u64);

    // Second multipart publish to the same path: `CompleteMultipartUpload`
    // gets a 412, and the just-opened multipart session is aborted rather than
    // leaked (no orphan session/parts left behind).
    let second_chunks: Vec<Bytes> = (0..num_chunks)
        .map(|i| Bytes::from(vec![b'z' - i; chunk_size]))
        .collect();
    let outcome2 = backend
        .publish_exclusive(path, chunk_stream(second_chunks), None)
        .await
        .expect("second multipart publish must return Ok(created=false)");
    assert!(
        !outcome2.created,
        "second multipart publish to an existing path must report created = false"
    );

    // The original assembled object is intact.
    let got = backend.get(path).await.unwrap();
    assert_eq!(
        got.as_ref(),
        first_concat.as_slice(),
        "the original multipart object must survive the rejected overwrite"
    );
}
