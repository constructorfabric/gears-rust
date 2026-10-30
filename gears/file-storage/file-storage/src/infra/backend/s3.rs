//! S3-compatible storage backend (ADR-0005).
//!
//! Requests are signed by `rusty-s3` (sign-only, no I/O) and executed by a `reqwest` client
//! owned by the backend. S3 XML response/error bodies are parsed with `quick-xml`; rusty-s3's
//! own `instant-xml` response types are deliberately unused. rusty-s3's `full` feature is
//! enabled because it also gates the `ListObjectsV2`/multipart action builders.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use file_storage_sdk::ByteRange;
use futures::StreamExt;
use futures::stream::BoxStream;
use reqwest::StatusCode;
use reqwest::header::{CONTENT_LENGTH, ETAG, RANGE};
use rusty_s3::S3Action;

use crate::domain::error::DomainError;
use crate::infra::content::hash;
use crate::infra::content::hash_mode::Manifest;

use super::{
    BackendCapabilities, MultipartCompletionPart, PublishOutcome, StorageBackend,
    build_manifest_and_root,
};

/// Whether the terminal write of a streamed upload may overwrite an existing object.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WriteMode {
    /// Plain `PutObject` / `CompleteMultipartUpload`: last write wins.
    Overwrite,
    /// `If-None-Match: *`; a `412` is not an error but `created: false`. Needs an endpoint that
    /// honours S3 conditional writes (AWS S3 since 2024-08).
    CreateExclusive,
}

/// Result of the shared streaming-upload core (`S3Backend::stream_upload`).
struct StreamUploadOutcome {
    bytes_written: u64,
    digest: [u8; 32],
    /// `false` only in `CreateExclusive` when the object already existed.
    created: bool,
}

/// Expiry of signed URLs; requests execute immediately, so it only covers clock skew and latency.
const SIGN_DURATION: Duration = Duration::from_mins(1);

/// Default `put_stream` multipart threshold and part size: 8 MiB, above S3's 5 MiB minimum
/// part size so S3 never rejects a part. Tests shrink it via `with_multipart_threshold_bytes`.
const DEFAULT_MULTIPART_THRESHOLD_BYTES: u64 = 8 * 1024 * 1024;

/// An S3-compatible storage backend (AWS S3, `MinIO`, `s3s-fs`) using path-style addressing.
pub struct S3Backend {
    id: String,
    bucket: rusty_s3::Bucket,
    credentials: rusty_s3::Credentials,
    http: reqwest::Client,
    /// `max-keys` for `ListObjectsV2`; `None` keeps the server default (S3: up to 1000).
    list_page_size: Option<u16>,
    /// Bytes at which `put_stream` switches from one `PutObject` to multipart; also the part size.
    multipart_threshold_bytes: u64,
}

impl S3Backend {
    /// Creates a backend for the S3-compatible `endpoint`, always with path-style addressing.
    pub fn new(
        id: impl Into<String>,
        endpoint: url::Url,
        region: impl Into<String>,
        bucket_name: impl Into<String>,
        access_key_id: impl Into<String>,
        secret_access_key: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let id = id.into();
        let bucket = rusty_s3::Bucket::new(
            endpoint,
            rusty_s3::UrlStyle::Path,
            bucket_name.into(),
            region.into(),
        )
        .map_err(|e| DomainError::backend(&id, format!("invalid S3 bucket config: {e}")))?;
        let credentials = rusty_s3::Credentials::new(access_key_id, secret_access_key);
        Ok(Self {
            id,
            bucket,
            credentials,
            http: reqwest::Client::new(),
            list_page_size: None,
            multipart_threshold_bytes: DEFAULT_MULTIPART_THRESHOLD_BYTES,
        })
    }

    /// Overrides `ListObjectsV2`'s `max-keys` page size (for pagination tests).
    #[must_use]
    pub fn with_list_page_size(mut self, n: u16) -> Self {
        self.list_page_size = Some(n);
        self
    }

    /// Overrides `put_stream`'s multipart threshold/part size (for multipart tests).
    #[must_use]
    pub fn with_multipart_threshold_bytes(mut self, n: u64) -> Self {
        self.multipart_threshold_bytes = n;
        self
    }

    /// Builds a backend from a `config::S3BackendConfig` entry; performs no I/O.
    ///
    /// - `endpoint: None` becomes `https://s3.{region}.amazonaws.com`; `Some(url)` is used as is.
    /// - Missing credentials fall back to `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` (no
    ///   IMDS/profile chain).
    /// - `cfg.path_style` is not forwarded: the bucket is always path-style.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid endpoint URL or missing credentials.
    pub fn from_config(cfg: &crate::config::S3BackendConfig) -> Result<Self, DomainError> {
        let endpoint_str = cfg
            .endpoint
            .clone()
            .unwrap_or_else(|| format!("https://s3.{}.amazonaws.com", cfg.region));
        let endpoint = endpoint_str.parse::<url::Url>().map_err(|e| {
            DomainError::backend(
                &cfg.id,
                format!("invalid S3 endpoint {endpoint_str:?}: {e}"),
            )
        })?;
        let access_key_id = cfg
            .access_key_id
            .clone()
            .or_else(|| std::env::var("AWS_ACCESS_KEY_ID").ok())
            .ok_or_else(|| {
                DomainError::backend(
                    &cfg.id,
                    "no access_key_id configured and AWS_ACCESS_KEY_ID is not set",
                )
            })?;
        let secret_access_key = cfg
            .secret_access_key
            .as_ref()
            .map(|s| s.expose().to_owned())
            .or_else(|| std::env::var("AWS_SECRET_ACCESS_KEY").ok())
            .ok_or_else(|| {
                DomainError::backend(
                    &cfg.id,
                    "no secret_access_key configured and AWS_SECRET_ACCESS_KEY is not set",
                )
            })?;
        Self::new(
            &cfg.id,
            endpoint,
            &cfg.region,
            &cfg.bucket,
            access_key_id,
            secret_access_key,
        )
    }

    /// Converts a backend path (`/{file_id}/{version_id}`) to an S3 key (no leading `/`);
    /// exact inverse of `key_to_path`.
    fn path_to_key(path: &str) -> &str {
        path.strip_prefix('/').unwrap_or(path)
    }

    /// Converts an S3 key back to a backend path; inverse of `path_to_key`.
    fn key_to_path(key: &str) -> String {
        format!("/{key}")
    }

    /// Timeouts, connect failures, request dispatch and body-read failures are transient;
    /// builder, redirect, decode and the like are configuration or protocol errors.
    fn transport_err(&self, e: &reqwest::Error) -> DomainError {
        self.classified(is_transient_transport(e), e.to_string())
    }

    /// Maps a non-2xx response to a `DomainError`, using the S3 XML error body if present
    /// (HEAD responses have none).
    fn s3_error(&self, status: StatusCode, body: &[u8]) -> DomainError {
        let message = match parse_error_body(body) {
            Some((code, message)) => format!("S3 error {status} ({code}): {message}"),
            None => format!("S3 error {status}"),
        };
        let code = parse_error_body(body).map(|(code, _)| code);
        self.classified(is_transient_s3(status, code.as_deref()), message)
    }

    fn classified(&self, transient: bool, message: String) -> DomainError {
        if transient {
            DomainError::backend_unavailable(&self.id, message)
        } else {
            DomainError::backend(&self.id, message)
        }
    }

    /// Sends a non-HEAD request and returns the success body; non-2xx becomes an error.
    async fn send_and_check(&self, req: reqwest::RequestBuilder) -> Result<Bytes, DomainError> {
        let resp = req.send().await.map_err(|e| self.transport_err(&e))?;
        let status = resp.status();
        let body = resp.bytes().await.map_err(|e| self.transport_err(&e))?;
        if status.is_success() {
            Ok(body)
        } else {
            Err(self.s3_error(status, &body))
        }
    }

    /// Like `send_and_check` for a create-exclusive write: `Ok(true)` = created, `Ok(false)` = `412`
    /// (object already existed, untouched). Any other non-2xx is an error.
    ///
    /// `exclusive` says the request carried `If-None-Match`: only then is a `409` the lost race.
    async fn send_and_check_created(
        &self,
        req: reqwest::RequestBuilder,
        exclusive: bool,
    ) -> Result<bool, DomainError> {
        let resp = req.send().await.map_err(|e| self.transport_err(&e))?;
        let status = resp.status();
        let body = resp.bytes().await.map_err(|e| self.transport_err(&e))?;
        if status.is_success() {
            Ok(true)
        } else if status == StatusCode::PRECONDITION_FAILED {
            Ok(false)
        } else if exclusive && status == StatusCode::CONFLICT {
            // `ConditionalRequestConflict`: a concurrent DELETE of the key finished before this
            // conditional write; the outcome is unknown, so it is a transient failure, not
            // `created: false`.
            Err(self.classified(true, conflict_message(status, &body)))
        } else {
            Err(self.s3_error(status, &body))
        }
    }

    /// `PutObject` with `If-None-Match: *` (added to the signed header set): `Ok(true)` = created,
    /// `Ok(false)` = `412`.
    async fn put_create_exclusive(&self, path: &str, bytes: Bytes) -> Result<bool, DomainError> {
        let key = Self::path_to_key(path);
        let mut action = self.bucket.put_object(Some(&self.credentials), key);
        // Every signed header must also be sent with the signed value, or S3 rejects the signature.
        action.headers_mut().insert("if-none-match", "*");
        let url = action.sign(SIGN_DURATION);
        self.send_and_check_created(
            self.http.put(url).header("if-none-match", "*").body(bytes),
            true,
        )
        .await
    }

    fn head_error(&self, path: &str, status: StatusCode) -> DomainError {
        self.classified(
            is_transient_status(status),
            format!("HEAD {path} failed: {status}"),
        )
    }

    /// POSTs `CompleteMultipartUpload` for `parts` (sorted by part number). Never re-reads the
    /// assembled object: callers already hold the digest (`put_stream` hashes incrementally,
    /// `complete_multipart` builds the ADR-0006 manifest root from per-part digests).
    ///
    /// In `CreateExclusive` the request carries `If-None-Match: *` and `Ok(false)` means the object
    /// already existed (`412`).
    async fn finalize_multipart(
        &self,
        path: &str,
        upload_handle: &str,
        parts: &[(u32, String)],
        mode: WriteMode,
    ) -> Result<bool, DomainError> {
        let mut sorted_parts = parts.to_vec();
        sorted_parts.sort_by_key(|(part_number, _)| *part_number);
        let etags: Vec<&str> = sorted_parts.iter().map(|(_, etag)| etag.as_str()).collect();

        let key = Self::path_to_key(path);
        let mut action = self.bucket.complete_multipart_upload(
            Some(&self.credentials),
            key,
            upload_handle,
            etags.iter().copied(),
        );
        let exclusive = mode == WriteMode::CreateExclusive;
        if exclusive {
            // Signed and sent, see `put_create_exclusive`.
            action.headers_mut().insert("if-none-match", "*");
        }
        let url = action.sign(SIGN_DURATION);
        let body = action.body();
        let mut req = self.http.post(url).body(body);
        if exclusive {
            req = req.header("if-none-match", "*");
        }
        self.send_and_check_created(req, exclusive).await
    }

    /// Shared core of `put_stream` and `publish_exclusive`. Streams into `path`: below `multipart_threshold_bytes` the object is buffered and
    /// written with one `PutObject`; above it a native multipart upload runs, holding at most
    /// one part plus the current chunk in memory. SHA-256 is computed incrementally and
    /// `max_size` is enforced mid-stream. Any failure after the multipart upload was initiated
    /// aborts it, leaving no orphaned session or partial object. A `412` in `CreateExclusive` yields
    /// `created: false` and aborts any multipart session opened along the way.
    async fn stream_upload(
        &self,
        path: &str,
        mut stream: BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
        mode: WriteMode,
    ) -> Result<StreamUploadOutcome, DomainError> {
        let mut hasher = hash::Hasher::new();
        let mut buf: Vec<u8> = Vec::new();
        let mut upload_handle: Option<String> = None;
        let mut parts: Vec<(u32, String)> = Vec::new();
        let mut next_part_number: u32 = 1;
        // Only satisfies `upload_part`'s ADR-0006 signature: this path hashes the whole
        // stream and never builds an offset manifest.
        let mut next_part_offset: u64 = 0;

        let collect_result: Result<(), DomainError> = async {
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| DomainError::backend(&self.id, e.to_string()))?;
                buf.extend_from_slice(&chunk);
                hasher.update(&chunk);
                if max_size.is_some_and(|m| hasher.len() > m) {
                    return Err(DomainError::validation("size", "exceeds max_size"));
                }

                // Flush full parts as they accumulate to bound memory.
                while buf.len() as u64 >= self.multipart_threshold_bytes {
                    if upload_handle.is_none() {
                        upload_handle = Some(self.initiate_multipart(path).await?);
                    }
                    let part_size =
                        usize::try_from(self.multipart_threshold_bytes).unwrap_or(buf.len());
                    let part_bytes: Vec<u8> = buf.drain(..part_size).collect();
                    let part_offset = next_part_offset;
                    next_part_offset += part_bytes.len() as u64;
                    let part_number = next_part_number;
                    next_part_number += 1;
                    let Some(handle) = upload_handle.as_deref() else {
                        // Unreachable (set just above); handled without `expect`/`unwrap`.
                        return Err(DomainError::backend(
                            &self.id,
                            "multipart handle missing right after initiation",
                        ));
                    };
                    let (etag, _part_hash) = self
                        .upload_part(
                            path,
                            handle,
                            part_number,
                            part_offset,
                            Bytes::from(part_bytes),
                        )
                        .await?;
                    parts.push((part_number, etag));
                }
            }
            Ok(())
        }
        .await;

        if let Err(e) = collect_result {
            if let Some(handle) = &upload_handle {
                // Best-effort cleanup of the multipart session.
                drop(self.abort_multipart(path, handle).await);
            }
            return Err(e);
        }

        let bytes_written = hasher.len();
        let digest = hash::digest_to_array(hasher.finalize());

        match upload_handle {
            None => {
                // Never crossed the threshold: one `PutObject`.
                let created = match mode {
                    WriteMode::Overwrite => {
                        self.put(path, Bytes::from(buf)).await?;
                        true
                    }
                    WriteMode::CreateExclusive => {
                        self.put_create_exclusive(path, Bytes::from(buf)).await?
                    }
                };
                Ok(StreamUploadOutcome {
                    bytes_written,
                    digest,
                    created,
                })
            }
            Some(handle) => {
                if !buf.is_empty() {
                    let part_number = next_part_number;
                    let part_offset = next_part_offset;
                    match self
                        .upload_part(path, &handle, part_number, part_offset, Bytes::from(buf))
                        .await
                    {
                        Ok((etag, _part_hash)) => parts.push((part_number, etag)),
                        Err(e) => {
                            drop(self.abort_multipart(path, &handle).await);
                            return Err(e);
                        }
                    }
                }
                // `finalize_multipart`, not `complete_multipart`: the digest was computed
                // while uploading, so the object is not re-downloaded to hash it.
                match self.finalize_multipart(path, &handle, &parts, mode).await {
                    Ok(created) => {
                        // `412`: our `CompleteMultipartUpload` did not take effect and the session is open; abort it.
                        if !created {
                            drop(self.abort_multipart(path, &handle).await);
                        }
                        Ok(StreamUploadOutcome {
                            bytes_written,
                            digest,
                            created,
                        })
                    }
                    Err(e) => {
                        drop(self.abort_multipart(path, &handle).await);
                        Err(e)
                    }
                }
            }
        }
    }
}

impl fmt::Debug for S3Backend {
    /// `Debug` that redacts `credentials`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Backend")
            .field("id", &self.id)
            .field("bucket", &self.bucket.name())
            .field("region", &self.bucket.region())
            .field("credentials", &"<redacted>")
            .field("list_page_size", &self.list_page_size)
            .field("multipart_threshold_bytes", &self.multipart_threshold_bytes)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl StorageBackend for S3Backend {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            multipart_native: true,
            range_native: true,
            durable: true,
            ..BackendCapabilities::default()
        }
    }

    async fn put(&self, path: &str, bytes: Bytes) -> Result<(), DomainError> {
        let key = Self::path_to_key(path);
        let url = self
            .bucket
            .put_object(Some(&self.credentials), key)
            .sign(SIGN_DURATION);
        self.send_and_check(self.http.put(url).body(bytes)).await?;
        Ok(())
    }

    /// Streams into `path` (overwrite allowed); see `stream_upload`.
    async fn put_stream(
        &self,
        path: &str,
        stream: BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<(u64, [u8; 32]), DomainError> {
        let o = self
            .stream_upload(path, stream, max_size, WriteMode::Overwrite)
            .await?;
        Ok((o.bytes_written, o.digest))
    }

    /// Create-exclusive publish: like `put_stream` but never overwrites; atomicity comes from S3
    /// conditional writes, a `412` maps to `created: false`.
    ///
    /// The endpoint MUST honour conditional writes; otherwise this degrades to last-write-wins (ADR-0005 gate).
    async fn publish_exclusive(
        &self,
        path: &str,
        stream: BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<PublishOutcome, DomainError> {
        let o = self
            .stream_upload(path, stream, max_size, WriteMode::CreateExclusive)
            .await?;
        Ok(PublishOutcome {
            bytes_written: o.bytes_written,
            digest: o.digest,
            created: o.created,
        })
    }

    async fn get(&self, path: &str) -> Result<Bytes, DomainError> {
        let key = Self::path_to_key(path);
        let url = self
            .bucket
            .get_object(Some(&self.credentials), key)
            .sign(SIGN_DURATION);
        self.send_and_check(self.http.get(url)).await
    }

    /// `GetObject` returned as a chunk stream (at most one chunk in memory). The status is
    /// checked before returning, so a missing object or S3 error surfaces from this call.
    async fn get_stream(
        &self,
        path: &str,
    ) -> Result<BoxStream<'_, std::io::Result<Bytes>>, DomainError> {
        let key = Self::path_to_key(path);
        let url = self
            .bucket
            .get_object(Some(&self.credentials), key)
            .sign(SIGN_DURATION);
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| self.transport_err(&e))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.bytes().await.map_err(|e| self.transport_err(&e))?;
            return Err(self.s3_error(status, &body));
        }

        let stream = resp
            .bytes_stream()
            .map(|r| r.map_err(|e| body_read_io_error(&e)));
        Ok(Box::pin(stream))
    }

    /// Native range read: a signed `GetObject` plus an unsigned `Range` header (allowed, as
    /// `Range` is not in `SigV4`'s signed canonical request). One round trip, no prior `HEAD`.
    async fn get_range(&self, path: &str, range: ByteRange) -> Result<Bytes, DomainError> {
        let header_value = match range {
            ByteRange::Inclusive { start, end } => {
                if start > end {
                    return Err(DomainError::validation("range", "unsatisfiable byte range"));
                }
                format!("bytes={start}-{end}")
            }
            ByteRange::OpenEnded { start } => format!("bytes={start}-"),
            ByteRange::Suffix { length } => {
                if length == 0 {
                    return Err(DomainError::validation("range", "unsatisfiable byte range"));
                }
                format!("bytes=-{length}")
            }
        };

        let key = Self::path_to_key(path);
        let url = self
            .bucket
            .get_object(Some(&self.credentials), key)
            .sign(SIGN_DURATION);
        let resp = self
            .http
            .get(url)
            .header(RANGE, header_value)
            .send()
            .await
            .map_err(|e| self.transport_err(&e))?;

        let status = resp.status();
        if status == StatusCode::RANGE_NOT_SATISFIABLE {
            return Err(DomainError::validation("range", "unsatisfiable byte range"));
        }
        let body = resp.bytes().await.map_err(|e| self.transport_err(&e))?;
        if status.is_success() {
            Ok(body)
        } else {
            Err(self.s3_error(status, &body))
        }
    }

    /// Size from the `HeadObject` `Content-Length` header.
    async fn size(&self, path: &str) -> Result<u64, DomainError> {
        let key = Self::path_to_key(path);
        let url = self
            .bucket
            .head_object(Some(&self.credentials), key)
            .sign(SIGN_DURATION);
        let resp = self
            .http
            .head(url)
            .send()
            .await
            .map_err(|e| self.transport_err(&e))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(self.head_error(path, status));
        }
        resp.headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or_else(|| DomainError::backend(&self.id, "HEAD response missing Content-Length"))
    }

    /// Idempotent: S3 returns success for a missing key too, so only transport/auth/5xx
    /// errors propagate.
    async fn delete(&self, path: &str) -> Result<(), DomainError> {
        let key = Self::path_to_key(path);
        let url = self
            .bucket
            .delete_object(Some(&self.credentials), key)
            .sign(SIGN_DURATION);
        let resp = self
            .http
            .delete(url)
            .send()
            .await
            .map_err(|e| self.transport_err(&e))?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            let body = resp.bytes().await.unwrap_or_default();
            Err(self.s3_error(status, &body))
        }
    }

    /// `HeadObject` check: 200 present, 404 absent; any other status or transport failure is
    /// an `Err`, not "missing".
    async fn exists(&self, path: &str) -> Result<bool, DomainError> {
        let key = Self::path_to_key(path);
        let url = self
            .bucket
            .head_object(Some(&self.credentials), key)
            .sign(SIGN_DURATION);
        let resp = self
            .http
            .head(url)
            .send()
            .await
            .map_err(|e| self.transport_err(&e))?;
        match resp.status() {
            StatusCode::OK => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            other => Err(self.head_error(path, other)),
        }
    }

    /// `CreateMultipartUpload`; the returned `<UploadId>` is the opaque handle for
    /// `upload_part`/`complete_multipart`/`abort_multipart`.
    async fn initiate_multipart(&self, path: &str) -> Result<String, DomainError> {
        let key = Self::path_to_key(path);
        let url = self
            .bucket
            .create_multipart_upload(Some(&self.credentials), key)
            .sign(SIGN_DURATION);
        let body = self.send_and_check(self.http.post(url)).await?;
        parse_upload_id(&body).ok_or_else(|| {
            DomainError::backend(
                &self.id,
                "CreateMultipartUpload response missing <UploadId>",
            )
        })
    }

    /// `UploadPart`. Returns `(backend_etag, part_hash_bytes)`: S3's `ETag` header with quotes
    /// stripped (fed back into `complete_multipart`) and the locally computed SHA-256 of
    /// `data` (S3's `ETag` is MD5-based).
    async fn upload_part(
        &self,
        path: &str,
        upload_handle: &str,
        part_number: u32,
        _part_offset: u64,
        data: Bytes,
    ) -> Result<(String, Vec<u8>), DomainError> {
        let part_hash = hash::sha256(&data);

        // S3 allows parts 1..=10_000, narrower than `u16`, so check explicitly.
        if !(1..=10_000).contains(&part_number) {
            return Err(DomainError::validation(
                "part_number",
                "must be between 1 and S3's maximum of 10,000 parts",
            ));
        }

        let key = Self::path_to_key(path);
        let part_number_u16 = u16::try_from(part_number).map_err(|_| {
            DomainError::validation("part_number", "exceeds S3's maximum of 10,000 parts")
        })?;
        let url = self
            .bucket
            .upload_part(Some(&self.credentials), key, part_number_u16, upload_handle)
            .sign(SIGN_DURATION);

        let resp = self
            .http
            .put(url)
            .body(data)
            .send()
            .await
            .map_err(|e| self.transport_err(&e))?;
        let status = resp.status();
        let etag_header = resp
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim_matches('"').to_owned());
        let body = resp.bytes().await.map_err(|e| self.transport_err(&e))?;
        if !status.is_success() {
            return Err(self.s3_error(status, &body));
        }
        let etag = etag_header.ok_or_else(|| {
            DomainError::backend(&self.id, "UploadPart response missing ETag header")
        })?;
        Ok((etag, part_hash))
    }

    /// `CompleteMultipartUpload` via `finalize_multipart`, then the ADR-0006 offset manifest
    /// and root from the caller's `(offset, part_hash)` pairs. The object is not re-read (S3's
    /// multipart `ETag` is an MD5-of-MD5s and cannot serve as the digest).
    async fn complete_multipart(
        &self,
        path: &str,
        upload_handle: &str,
        parts: &[MultipartCompletionPart],
    ) -> Result<(Manifest, [u8; 32]), DomainError> {
        // S3 needs only the `(part_number, backend_etag)` pairs.
        let etag_parts: Vec<(u32, String)> = parts
            .iter()
            .map(|(part_number, _, _, etag)| (*part_number, etag.clone()))
            .collect();
        self.finalize_multipart(path, upload_handle, &etag_parts, WriteMode::Overwrite)
            .await?;

        build_manifest_and_root(parts)
    }

    /// `AbortMultipartUpload`: discards all previously uploaded parts.
    async fn abort_multipart(&self, path: &str, upload_handle: &str) -> Result<(), DomainError> {
        let key = Self::path_to_key(path);
        let url = self
            .bucket
            .abort_multipart_upload(Some(&self.credentials), key, upload_handle)
            .sign(SIGN_DURATION);
        let resp = self
            .http
            .delete(url)
            .send()
            .await
            .map_err(|e| self.transport_err(&e))?;
        let status = resp.status();
        let body = resp.bytes().await.map_err(|e| self.transport_err(&e))?;
        if status.is_success() {
            Ok(())
        } else if status == StatusCode::NOT_FOUND
            && parse_error_body(&body).is_some_and(|(code, _)| code == "NoSuchUpload")
        {
            // `NoSuchUpload`: the handle was already consumed (completed) or aborted.
            tracing::debug!(path, "abort_multipart: upload handle already gone");
            Ok(())
        } else {
            Err(self.s3_error(status, &body))
        }
    }

    /// `ListObjectsV2`, following continuation tokens until the listing is complete.
    async fn list_paths(&self) -> Result<Vec<String>, DomainError> {
        let mut paths = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut action = self.bucket.list_objects_v2(Some(&self.credentials));
            if let Some(n) = self.list_page_size {
                action.with_max_keys(n as usize);
            }
            if let Some(token) = &continuation_token {
                action.with_continuation_token(token.clone());
            }
            let url = action.sign(SIGN_DURATION);
            let body = self.send_and_check(self.http.get(url)).await?;

            let page = parse_list_objects_response(&body).map_err(|e| {
                DomainError::backend(
                    &self.id,
                    format!("failed to parse ListObjectsV2 response: {e}"),
                )
            })?;
            paths.extend(page.keys.iter().map(|k| Self::key_to_path(k)));

            if page.is_truncated && page.next_continuation_token.is_some() {
                continuation_token = page.next_continuation_token;
            } else {
                break;
            }
        }

        Ok(paths)
    }

    /// Readiness probe: `ListObjectsV2` (`max-keys=1`) against the bucket. A `HeadObject` on a
    /// probe key would not do: its bodyless `404` cannot tell `NoSuchBucket` from `NoSuchKey`,
    /// so a missing bucket would look healthy. The listing succeeds (even empty) for any
    /// accessible bucket and errors otherwise; the body is discarded.
    async fn is_ready(&self) -> Result<(), DomainError> {
        let mut action = self.bucket.list_objects_v2(Some(&self.credentials));
        action.with_max_keys(1);
        let url = action.sign(SIGN_DURATION);
        self.send_and_check(self.http.get(url)).await.map(|_| ())
    }
}

/// Message for a `409` on a conditional write.
fn conflict_message(status: StatusCode, body: &[u8]) -> String {
    let detail = parse_error_body(body).map_or_else(String::new, |(code, message)| {
        format!(" ({code}): {message}")
    });
    format!("S3 error {status}{detail}; conditional write lost a race with a concurrent change")
}

/// HTTP statuses S3 uses for transient overload / server trouble (`SlowDown` is 503).
fn is_transient_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 500 | 502 | 503 | 504)
}

/// Transient by status or by S3 error code (`RequestTimeout` arrives as a `400`).
fn is_transient_s3(status: StatusCode, code: Option<&str>) -> bool {
    is_transient_status(status)
        || matches!(
            code,
            Some(
                "SlowDown"
                    | "RequestTimeout"
                    | "OperationAborted"
                    | "InternalError"
                    | "ServiceUnavailable"
            )
        )
}

/// I/O error for a failed response-body read: a timeout or dropped connection keeps a transient
/// kind so the consumer classifies it as `BackendUnavailable`.
fn body_read_io_error(e: &reqwest::Error) -> std::io::Error {
    let kind = if e.is_timeout() {
        std::io::ErrorKind::TimedOut
    } else if e.is_connect() || e.is_body() || e.is_request() {
        std::io::ErrorKind::ConnectionReset
    } else {
        std::io::ErrorKind::Other
    };
    std::io::Error::new(kind, e.to_string())
}

/// Transient transport failure: timeout, connect, request dispatch or body read.
fn is_transient_transport(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_request() || e.is_body()
}

/// A single parsed `ListObjectsV2` response page.
struct ListObjectsPage {
    keys: Vec<String>,
    is_truncated: bool,
    next_continuation_token: Option<String>,
}

/// XML-unescaped text of a `quick-xml` event; a broken entity falls back to the raw text.
fn xml_text(t: &quick_xml::events::BytesText<'_>) -> String {
    let raw = t.as_ref();
    quick_xml::escape::unescape(raw).map_or_else(|_| raw.to_owned(), std::borrow::Cow::into_owned)
}

/// Parses a `ListObjectsV2` response, extracting the fields `list_paths` needs. Keys are
/// percent-decoded because `rusty_s3` always requests `encoding-type=url`.
fn parse_list_objects_response(body: &[u8]) -> Result<ListObjectsPage, quick_xml::Error> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut keys = Vec::new();
    let mut is_truncated = false;
    let mut next_continuation_token = None;

    // `<Key>` counts only inside `<Contents>`.
    let mut in_contents = false;
    let mut current_tag: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_owned();
                if name == "Contents" {
                    in_contents = true;
                }
                current_tag = Some(name);
            }
            Event::End(e) => {
                let name = e.local_name().as_ref().to_owned();
                if name == "Contents" {
                    in_contents = false;
                }
                current_tag = None;
            }
            Event::Text(t) => {
                let text = xml_text(&t);
                match current_tag.as_deref() {
                    Some("Key") if in_contents => keys.push(percent_decode(&text)),
                    Some("IsTruncated") => is_truncated = text == "true",
                    Some("NextContinuationToken") => next_continuation_token = Some(text),
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(ListObjectsPage {
        keys,
        is_truncated,
        next_continuation_token,
    })
}

/// Extracts the `UploadId` from a `CreateMultipartUpload` response body.
fn parse_upload_id(body: &[u8]) -> Option<String> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut current_tag: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                current_tag = Some(e.local_name().as_ref().to_owned());
            }
            Ok(Event::End(_)) => current_tag = None,
            Ok(Event::Text(t)) => {
                if current_tag.as_deref() == Some("UploadId") {
                    return Some(xml_text(&t));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    None
}

/// Parses an S3 XML error body into `(code, message)`; `None` if empty or unparseable.
fn parse_error_body(body: &[u8]) -> Option<(String, String)> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    if body.is_empty() {
        return None;
    }

    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut code = None;
    let mut message = None;
    let mut current_tag: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                current_tag = Some(e.local_name().as_ref().to_owned());
            }
            Ok(Event::End(_)) => current_tag = None,
            Ok(Event::Text(t)) => {
                let text = xml_text(&t);
                match current_tag.as_deref() {
                    Some("Code") => code = Some(text),
                    Some("Message") => message = Some(text),
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    code.map(|c| (c, message.unwrap_or_default()))
}

/// Minimal percent-decoder for `ListObjectsV2` keys (avoids a `percent-encoding` dependency).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..=i + 2]).unwrap_or_default(),
                16,
            )
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
#[path = "s3_tests.rs"]
mod s3_tests;
