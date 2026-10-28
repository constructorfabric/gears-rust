//! `S3Backend` tests against an in-process `s3s-fs` server.

use std::net::SocketAddr;

use bytes::Bytes;
use file_storage_sdk::ByteRange;
use futures::stream::{self, BoxStream};
use rusty_s3::S3Action;
use tempfile::TempDir;

use super::{S3Backend, StatusCode, conflict_message};
use crate::domain::error::DomainError;
use crate::infra::backend::StorageBackend;
use crate::infra::backend::backend_tests::{
    assert_backend_contract, assert_put_stream_rejects_oversized_stream,
};
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

fn unique_bucket() -> String {
    format!("test-{}", uuid::Uuid::now_v7())
}

/// A fresh server plus a backend on a fresh bucket; keep the `TempDir` alive for the test.
async fn fixture() -> (S3Backend, TempDir, String) {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = unique_bucket();
    // The bucket dir is pre-created: `s3s-fs` does not auto-create buckets.
    tokio::fs::create_dir_all(dir.path().join(&bucket))
        .await
        .expect("pre-create s3s-fs bucket directory");
    let endpoint: url::Url = format!("http://{addr}")
        .parse()
        .expect("valid endpoint url");
    let backend = S3Backend::new(
        "s3-test",
        endpoint,
        "us-east-1",
        &bucket,
        TEST_ACCESS_KEY,
        TEST_SECRET_KEY,
    )
    .expect("construct S3Backend");
    (backend, dir, bucket)
}

#[tokio::test]
async fn s3_backend_put_get_round_trip() {
    let (backend, dir, bucket) = fixture().await;

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

    let (backend, _dir, _bucket) = fixture().await;

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
async fn s3_backend_get_range_returns_native_partial_content() {
    let (backend, _dir, _bucket) = fixture().await;

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
async fn s3_is_ready_ok_against_s3s_fs() {
    let (backend, _dir, _bucket) = fixture().await;

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

    let err = backend
        .is_ready()
        .await
        .expect_err("is_ready must fail against an unreachable endpoint");
    assert!(
        matches!(err, DomainError::BackendUnavailable { .. }),
        "a transport failure is transient, got {err:?}"
    );
}

/// Status classification: 5xx overload is transient, 4xx is permanent.
#[test]
fn s3_error_classifies_transient_and_permanent_statuses() {
    let endpoint: url::Url = "http://127.0.0.1:1".parse().expect("valid endpoint url");
    let backend = S3Backend::new("s3-test", endpoint, "us-east-1", "b", "k", "s")
        .expect("construct S3Backend");
    let slow_down = b"<Error><Code>SlowDown</Code><Message>slow</Message></Error>";
    for status in [500u16, 502, 503, 504] {
        let status = StatusCode::from_u16(status).unwrap();
        assert!(
            matches!(
                backend.s3_error(status, slow_down),
                DomainError::BackendUnavailable { .. }
            ),
            "{status} must be transient"
        );
    }
    let timeout = b"<Error><Code>RequestTimeout</Code><Message>slow</Message></Error>";
    assert!(
        matches!(
            backend.s3_error(StatusCode::BAD_REQUEST, timeout),
            DomainError::BackendUnavailable { .. }
        ),
        "400 RequestTimeout must be transient"
    );
    for code in ["OperationAborted", "InternalError", "ServiceUnavailable"] {
        let body = format!("<Error><Code>{code}</Code><Message>m</Message></Error>");
        assert!(
            matches!(
                backend.s3_error(StatusCode::CONFLICT, body.as_bytes()),
                DomainError::BackendUnavailable { .. }
            ),
            "{code} must be transient"
        );
    }
    let no_bucket = b"<Error><Code>NoSuchBucket</Code><Message>m</Message></Error>";
    assert!(
        matches!(
            backend.s3_error(StatusCode::NOT_FOUND, no_bucket),
            DomainError::Backend { .. }
        ),
        "404 NoSuchBucket must be permanent"
    );
    for status in [400u16, 403, 404, 409] {
        let status = StatusCode::from_u16(status).unwrap();
        assert!(
            matches!(backend.s3_error(status, b""), DomainError::Backend { .. }),
            "{status} must be permanent in the generic classifier"
        );
    }
    let message = conflict_message(
        StatusCode::CONFLICT,
        b"<Error><Code>ConditionalRequestConflict</Code><Message>m</Message></Error>",
    );
    assert!(message.contains("ConditionalRequestConflict"));
}

/// `is_ready` must fail when the bucket does not exist: `NoSuchBucket` from `ListObjectsV2`
/// is distinguishable from an absent key (unlike `HeadObject`). Bypasses `fixture`.
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
    let (backend, _dir, _bucket) = fixture().await;
    // Page size 2 over 5 objects forces at least 3 `ListObjectsV2` pages.
    let backend = backend.with_list_page_size(2);

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
    let (backend, dir, bucket) = fixture().await;

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
    let (backend, _dir, _bucket) = fixture().await;

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
    // Default multipart threshold (8 MiB): this stream stays below it, so a single `PutObject`.
    let (backend, dir, bucket) = fixture().await;

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
    let (backend, _dir, _bucket) = fixture().await;
    // Low threshold so a multi-MiB stream crosses it with full 5 MiB parts.
    let part_size: u64 = 5 * 1024 * 1024;
    let backend = backend.with_multipart_threshold_bytes(part_size);

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
    // Threshold 8 bytes: the first 10-byte chunk starts multipart, the second exceeds
    // `max_size`, so an already-initiated session must be aborted.
    let (backend, _dir, _bucket) = fixture().await;
    let backend = backend.with_multipart_threshold_bytes(8);

    assert_put_stream_rejects_oversized_stream(&backend, "put-stream/rejected", 15).await;
}

/// One-shot mock S3: answers the first request with `status` and `body`.
async fn mock_s3_once(status: u16, body: &'static str) -> S3Backend {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind mock s3");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            if stream.read(&mut buf).await.is_ok() {
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.ok();
            }
        }
    });
    let endpoint: url::Url = format!("http://{addr}")
        .parse()
        .expect("valid endpoint url");
    S3Backend::new("s3-test", endpoint, "us-east-1", "b", "k", "s").expect("construct S3Backend")
}

#[tokio::test]
async fn s3_abort_multipart_404_only_ok_for_no_such_upload() {
    let backend = mock_s3_once(
        404,
        "<Error><Code>NoSuchUpload</Code><Message>m</Message></Error>",
    )
    .await;
    backend
        .abort_multipart("/f/v", "handle")
        .await
        .expect("an already-gone upload is a success");

    let backend = mock_s3_once(
        404,
        "<Error><Code>NoSuchBucket</Code><Message>m</Message></Error>",
    )
    .await;
    let err = backend
        .abort_multipart("/f/v", "handle")
        .await
        .expect_err("a missing bucket is not a successful abort");
    assert!(matches!(err, DomainError::Backend { .. }), "got {err:?}");
}

/// Runs `send_and_check_created` against a mock answering `status`/`body`.
async fn created_against(
    status: u16,
    body: &'static str,
    exclusive: bool,
) -> Result<bool, DomainError> {
    let backend = mock_s3_once(status, body).await;
    let url = backend
        .bucket
        .put_object(None, "k")
        .sign(super::SIGN_DURATION);
    backend
        .send_and_check_created(backend.http.put(url), exclusive)
        .await
}

#[tokio::test]
async fn s3_send_and_check_created_classifies_412_and_409() {
    let cond = "<Error><Code>ConditionalRequestConflict</Code><Message>m</Message></Error>";
    assert!(
        !created_against(412, "", true)
            .await
            .expect("412 is created:false")
    );

    let err = created_against(409, cond, true)
        .await
        .expect_err("a conditional 409 is a lost race");
    assert!(matches!(err, DomainError::BackendUnavailable { .. }));
    assert!(err.to_string().contains("lost a race"), "got {err}");

    let other = "<Error><Code>BucketAlreadyOwnedByYou</Code><Message>m</Message></Error>";
    let err = created_against(409, other, false)
        .await
        .expect_err("an unconditional 409 is an ordinary error");
    assert!(matches!(err, DomainError::Backend { .. }), "got {err:?}");
    assert!(!err.to_string().contains("lost a race"), "got {err}");
}

/// A builder error (no network) is a configuration problem, not a transient outage.
#[tokio::test]
async fn s3_transport_err_builder_error_is_permanent() {
    let endpoint: url::Url = "http://127.0.0.1:1".parse().expect("valid endpoint url");
    let backend = S3Backend::new("s3-test", endpoint, "us-east-1", "b", "k", "s")
        .expect("construct S3Backend");
    let e = backend
        .http
        .get("http://[invalid")
        .send()
        .await
        .expect_err("invalid URL must fail to build");
    assert!(e.is_builder());
    assert!(matches!(
        backend.transport_err(&e),
        DomainError::Backend { .. }
    ));
}

/// A second `publish_exclusive` to one path reports `created = false` and keeps the first
/// bytes, on both the single-`PutObject` and the multipart path (`If-None-Match: *` on each).
#[tokio::test]
async fn s3_backend_publish_exclusive_rejects_overwrite() {
    const MIB: usize = 1024 * 1024;
    // (name, multipart threshold, chunk size, chunk count): the default 8 MiB threshold keeps the
    // small payload on a single PUT; the 5 MiB one sends 11 MiB through multipart.
    let cases = [
        ("single-put", None, 8, 1),
        ("multipart", Some(5 * MIB as u64), MIB, 11),
    ];
    for (name, threshold, chunk_size, num_chunks) in cases {
        let (backend, _dir, _bucket) = fixture().await;
        let backend = match threshold {
            Some(t) => backend.with_multipart_threshold_bytes(t),
            None => backend,
        };
        let payload = |byte_of: fn(u8) -> u8| -> Vec<Bytes> {
            (0..num_chunks)
                .map(|i| Bytes::from(vec![byte_of(i); chunk_size]))
                .collect()
        };
        let first = payload(|i| b'a' + i);
        let second = payload(|i| b'z' - i);
        let first_concat: Vec<u8> = first.iter().flat_map(|c| c.to_vec()).collect();
        let path = format!("publish-exclusive/{name}");

        let outcome = backend
            .publish_exclusive(&path, chunk_stream(first), None)
            .await
            .unwrap_or_else(|e| panic!("{name}: first publish_exclusive must succeed: {e:?}"));
        assert!(outcome.created, "{name}: first publish must create");
        assert_eq!(outcome.bytes_written, first_concat.len() as u64, "{name}");
        assert_eq!(
            outcome.digest,
            hash::digest_to_array(hash::sha256(&first_concat)),
            "{name}"
        );

        // A replay must return `Ok(created = false)`, not an error, and the multipart session
        // it opened is aborted.
        let outcome2 = backend
            .publish_exclusive(&path, chunk_stream(second), None)
            .await
            .unwrap_or_else(|e| panic!("{name}: second publish must be Ok(created=false): {e:?}"));
        assert!(!outcome2.created, "{name}: second publish must not create");
        assert_eq!(outcome2.bytes_written, first_concat.len() as u64, "{name}");

        let got = backend.get(&path).await.unwrap();
        assert_eq!(
            got.as_ref(),
            first_concat.as_slice(),
            "{name}: the original bytes must survive the rejected overwrite"
        );
    }
}

// No concurrent-racer test here: `s3s-fs` checks `If-None-Match: *` with a plain `exists()` then
// writes, so concurrent publishes can both win. That is a test-double limitation, not
// `S3Backend`'s; the conditional header is covered sequentially by the tests above.
