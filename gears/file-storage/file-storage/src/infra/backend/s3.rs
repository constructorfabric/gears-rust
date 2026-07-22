//! S3-compatible storage backend
//! (`cpt-cf-file-storage-fr-backend-abstraction`, ADR-0005
//! `cpt-cf-file-storage-adr-s3-client-selection`).
//!
//! Requests are **signed** by `rusty-s3` (a sign-only, Sans-IO request builder —
//! it never performs I/O itself) and **executed** by this gear's existing
//! `reqwest` client. S3's XML response/error bodies are parsed in-house via
//! `quick-xml`, per the ADR.
//!
//! ## Dependency-feature deviation (Stage 0, recorded here per plan.md)
//! `rusty-s3` gates its `ListObjectsV2`/`CreateMultipartUpload`/
//! `CompleteMultipartUpload` **action builders** (not just their bundled
//! response-parsing types) behind the `full` cargo feature — disabling it
//! removes the ability to construct those requests at all, not just their
//! response parsing. `full` is therefore enabled (alongside `aws-lc-rs`, reused
//! from the workspace's existing TLS stack rather than adding `rustcrypto`).
//! This module never uses rusty-s3's own `instant-xml`-based response types
//! (e.g. `ListObjectsV2Response`); every S3 response body this backend reads is
//! parsed with `quick-xml` directly, matching the ADR's intent.
//!
//! ## `reqwest::Client` ownership
//! `S3Backend` constructs its own `reqwest::Client` internally (cheap: the
//! client is a thin `Arc` handle). Stage 4/5 callers may switch to injecting a
//! shared client if that proves more convenient once those call sites exist;
//! nothing about this stage's trait contract depends on which is chosen.

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
    BackendCapabilities, MultipartCompletionPart, StorageBackend, build_manifest_and_root,
};

/// Expiry for the presigned URLs this backend signs. Requests execute
/// immediately after signing (there is no user-facing redirect), so this only
/// needs to survive clock skew plus the request's own latency.
const SIGN_DURATION: Duration = Duration::from_mins(1);

/// Default `put_stream` multipart threshold: 8 MiB, comfortably above S3's
/// own 5 MiB minimum part size, so a real S3 never rejects a part this
/// backend produces. Also used as the part size once multipart is underway.
/// Tests override this (via `with_multipart_threshold_bytes`) with a small
/// value so they can exercise the multipart path without generating
/// megabytes of data.
const DEFAULT_MULTIPART_THRESHOLD_BYTES: u64 = 8 * 1024 * 1024;

/// An S3-compatible storage backend. Talks to any S3-compatible HTTP API
/// (real AWS S3, `MinIO`, `s3s-fs` in tests) via path-style addressing.
pub struct S3Backend {
    id: String,
    bucket: rusty_s3::Bucket,
    credentials: rusty_s3::Credentials,
    http: reqwest::Client,
    /// Page size passed as `max-keys` to `ListObjectsV2`. `None` leaves the
    /// server's own default (S3: up to 1000 keys per page) in effect. Tests
    /// use a small value to exercise `list_paths`'s continuation-token
    /// pagination loop without seeding hundreds of real objects.
    list_page_size: Option<u16>,
    /// `put_stream`'s threshold (in bytes) between a single buffered
    /// `PutObject` and driving a native multipart upload; also used as the
    /// part size once multipart is underway. See `DEFAULT_MULTIPART_THRESHOLD_BYTES`.
    multipart_threshold_bytes: u64,
}

impl S3Backend {
    /// Construct a new S3 backend.
    ///
    /// `endpoint` is the S3-compatible HTTP(S) endpoint (path-style
    /// addressing is used throughout, i.e. `UrlStyle::Path` — matches
    /// `s3s-fs`/MinIO-style deployments as well as real S3 when path-style is
    /// explicitly requested).
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

    /// Override `ListObjectsV2`'s `max-keys` page size. Defaults to `None`
    /// (server default, up to 1000). Exposed for tests that need to exercise
    /// pagination without seeding hundreds of objects.
    #[must_use]
    pub fn with_list_page_size(mut self, n: u16) -> Self {
        self.list_page_size = Some(n);
        self
    }

    /// Override `put_stream`'s multipart threshold/part size. Defaults to
    /// `DEFAULT_MULTIPART_THRESHOLD_BYTES` (8 MiB). Exposed for tests that
    /// need to exercise the multipart `put_stream` path without generating
    /// megabytes of data.
    #[must_use]
    pub fn with_multipart_threshold_bytes(mut self, n: u64) -> Self {
        self.multipart_threshold_bytes = n;
        self
    }

    /// Build an `S3Backend` from a `config::S3BackendConfig` entry (P2 1.7.3
    /// config wiring). Shared by `gear.rs`'s `build_backend_registry` and the
    /// sidecar's `FS_SIDECAR_S3_BACKENDS` parsing so the two don't duplicate
    /// construction logic.
    ///
    /// - `endpoint: None` derives a real-AWS endpoint from `region`
    ///   (`https://s3.{region}.amazonaws.com`); `Some(url)` is used verbatim
    ///   (`MinIO`/`s3s-fs`/any other S3-compatible endpoint).
    /// - Credentials fall back to the standard `AWS_ACCESS_KEY_ID`/
    ///   `AWS_SECRET_ACCESS_KEY` environment variables when the config entry
    ///   itself leaves them unset — a deliberately simple fallback, not a
    ///   full IMDS/profile chain.
    /// - `cfg.path_style` is currently accepted but not forwarded: this
    ///   constructor always builds a path-style `rusty_s3::Bucket` (see
    ///   `S3BackendConfig::path_style`'s doc comment for why that's still
    ///   correct against real S3 too).
    /// - Performs no I/O — invalid input (a bad endpoint URL, missing
    ///   credentials with no environment fallback) surfaces as a returned
    ///   `Err`, never a panic, so a caller can treat it as an init-time error.
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

    /// Convert an opaque backend path (e.g. `/{file_id}/{version_id}`) into
    /// the S3 object key used for every operation (S3 keys never start with
    /// `/`). This is the exact inverse of `key_to_path` — every path must
    /// round-trip through `put` -> `list_paths` and compare equal.
    fn path_to_key(path: &str) -> &str {
        path.strip_prefix('/').unwrap_or(path)
    }

    /// Convert an S3 object key (as returned by `ListObjectsV2`) back into
    /// this gear's opaque backend-path convention. Inverse of `path_to_key`.
    fn key_to_path(key: &str) -> String {
        format!("/{key}")
    }

    fn transport_err(&self, e: &reqwest::Error) -> DomainError {
        DomainError::backend(&self.id, e.to_string())
    }

    /// Build a `DomainError` from a non-2xx response, parsing the S3 XML error
    /// body (`<Error><Code>...</Code><Message>...</Message></Error>`) via
    /// `quick-xml` when a body is present (HEAD responses never carry one).
    fn s3_error(&self, status: StatusCode, body: &[u8]) -> DomainError {
        match parse_error_body(body) {
            Some((code, message)) => {
                DomainError::backend(&self.id, format!("S3 error {status} ({code}): {message}"))
            }
            None => DomainError::backend(&self.id, format!("S3 error {status}")),
        }
    }

    /// Send a request that may carry an S3 XML error body on failure
    /// (everything except HEAD). Returns the raw success body.
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

    fn head_error(&self, path: &str, status: StatusCode) -> DomainError {
        DomainError::backend(&self.id, format!("HEAD {path} failed: {status}"))
    }

    /// POST a `CompleteMultipartUpload` request that assembles `parts`
    /// (defensively sorted ascending by part number) into the final object.
    /// This does **not** re-read the assembled object to hash it: both
    /// callers already hold what they need without a re-download — `put_stream`
    /// hashed the whole object incrementally as it uploaded, and
    /// `complete_multipart` builds the ADR-0006 offset-manifest root from the
    /// per-part digests it was handed. Either way a large multipart upload
    /// stays a single pass over the bytes instead of upload-then-re-download.
    async fn finalize_multipart(
        &self,
        path: &str,
        upload_handle: &str,
        parts: &[(u32, String)],
    ) -> Result<(), DomainError> {
        let mut sorted_parts = parts.to_vec();
        sorted_parts.sort_by_key(|(part_number, _)| *part_number);
        let etags: Vec<&str> = sorted_parts.iter().map(|(_, etag)| etag.as_str()).collect();

        let key = Self::path_to_key(path);
        let action = self.bucket.complete_multipart_upload(
            Some(&self.credentials),
            key,
            upload_handle,
            etags.iter().copied(),
        );
        let url = action.sign(SIGN_DURATION);
        let body = action.body();
        self.send_and_check(self.http.post(url).body(body)).await?;
        Ok(())
    }
}

impl fmt::Debug for S3Backend {
    /// Manual `Debug`, redacting `credentials` (mirrors
    /// `FileStorageConfig`'s manual `Debug` impl's secret-redaction pattern).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Backend")
            .field("id", &self.id)
            .field("bucket", &self.bucket.name())
            .field("region", &self.bucket.region())
            .field("credentials", &"<redacted>")
            .field("list_page_size", &self.list_page_size)
            .field("multipart_threshold_bytes", &self.multipart_threshold_bytes)
            // `reqwest::Client` has no useful `Debug` output of its own beyond
            // internal connection-pool state; omit it explicitly.
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

    /// Streams `stream` into `path` without ever buffering the whole object
    /// in memory once it crosses `multipart_threshold_bytes`: below the
    /// threshold, the (small) object is buffered whole and written with one
    /// `PutObject`; above it, this drives a native multipart upload, holding
    /// at most one part's worth of bytes beyond the current chunk at a time.
    /// The SHA-256 digest is computed incrementally as bytes arrive
    /// (`hash::Hasher`), and `max_size` is enforced the moment the running
    /// total exceeds it — mid-stream, before any extra part is flushed. If a
    /// multipart upload was already initiated when the stream fails (a
    /// transport error or a `max_size` violation) or when finishing the
    /// upload fails (uploading the final part / `CompleteMultipartUpload`),
    /// the multipart session is aborted so no orphaned session or partial
    /// object is left behind.
    async fn put_stream(
        &self,
        path: &str,
        mut stream: BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<(u64, [u8; 32]), DomainError> {
        let mut hasher = hash::Hasher::new();
        let mut buf: Vec<u8> = Vec::new();
        let mut upload_handle: Option<String> = None;
        let mut parts: Vec<(u32, String)> = Vec::new();
        let mut next_part_number: u32 = 1;
        // Byte offset of the next part within the object. `put_stream`
        // produces a `whole-sha256` version (the digest is computed
        // incrementally over the whole stream, not from an offset-manifest),
        // so this is threaded purely to satisfy `upload_part`'s ADR-0006
        // signature; it is never used to build a manifest on this path.
        let mut next_part_offset: u64 = 0;

        let collect_result: Result<(), DomainError> = async {
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| DomainError::backend(&self.id, e.to_string()))?;
                buf.extend_from_slice(&chunk);
                hasher.update(&chunk);
                if max_size.is_some_and(|m| hasher.len() > m) {
                    return Err(DomainError::validation("size", "exceeds max_size"));
                }

                // Flush full part-sized chunks as they accumulate, so at most
                // one part's worth of bytes (plus the current chunk) is ever
                // held in memory beyond what's already been shipped.
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
                        // Unreachable: `upload_handle` was just set to `Some`
                        // above if it was `None`. Handled defensively rather
                        // than via `expect`/`unwrap`.
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
                // Best-effort cleanup: never leave a dangling multipart
                // session behind after a rejected/failed stream.
                drop(self.abort_multipart(path, handle).await);
            }
            return Err(e);
        }

        let bytes_written = hasher.len();
        let digest = hash::digest_to_array(hasher.finalize());

        match upload_handle {
            None => {
                // Never crossed the threshold: the whole (small) object is
                // already buffered — issue one PutObject.
                self.put(path, Bytes::from(buf)).await?;
                Ok((bytes_written, digest))
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
                // Use `finalize_multipart`, not `complete_multipart`, so the
                // just-assembled object is never re-downloaded just to hash it:
                // the digest was already computed incrementally as the bytes
                // were uploaded, and is bit-identical to what re-reading and
                // hashing the stored object would yield (a test asserts the two
                // actually agree). This keeps a large streaming upload to a
                // single pass over the bytes instead of upload-then-re-download.
                match self.finalize_multipart(path, &handle, &parts).await {
                    Ok(()) => Ok((bytes_written, digest)),
                    Err(e) => {
                        drop(self.abort_multipart(path, &handle).await);
                        Err(e)
                    }
                }
            }
        }
    }

    async fn get(&self, path: &str) -> Result<Bytes, DomainError> {
        let key = Self::path_to_key(path);
        let url = self
            .bucket
            .get_object(Some(&self.credentials), key)
            .sign(SIGN_DURATION);
        self.send_and_check(self.http.get(url)).await
    }

    /// Presign and execute a `GetObject` for `path`, returning the response
    /// body as a `BoxStream` of chunks (`Response::bytes_stream()`) instead of
    /// buffering it whole, so a read-back never holds more than one chunk in
    /// memory at a time regardless of object size. The request itself is sent
    /// and its status checked eagerly (before returning), so a missing object
    /// or an S3 error surfaces from this call directly rather than from
    /// polling the returned stream.
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
            .map(|r| r.map_err(std::io::Error::other));
        Ok(Box::pin(stream))
    }

    /// Native range read: signs a plain `GetObject` request and layers an
    /// **unsigned** `Range` header on top (valid because `Range` is not part
    /// of `SigV4`'s signed canonical request — ADR-0005's Decision Outcome).
    /// Builds the header directly from `range` without a prior `HEAD`, so a
    /// range read never costs more than one round trip.
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

    /// Cheap stat via `HeadObject`: reads only the `Content-Length` response
    /// header, never the object's content.
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

    /// `DeleteObject` is idempotent by construction: S3 returns a success
    /// status for a missing key exactly the same as for a present one, so
    /// there is no separate "already absent" signal to special-case here
    /// (unlike `LocalFsBackend`, which checks the filesystem `NotFound` kind).
    /// Only a genuine transport/auth/5xx error propagates as `Err`.
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

    /// `HeadObject`-based existence check: 200 -> present, 404 -> absent, any
    /// other status (403, 5xx, transport failure) propagates as `Err` rather
    /// than being folded into "missing" (mirrors `LocalFsBackend::exists`'s
    /// present/missing/error three-way split).
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

    /// `CreateMultipartUpload`: signs and POSTs (empty body, `uploads=1` query
    /// param baked into the signed URL), then parses the `<UploadId>` out of
    /// the XML response body via `quick-xml` (deliberately not rusty-s3's own
    /// `instant-xml`-based `CreateMultipartUploadResponse` — see this module's
    /// doc comment). The returned string is the opaque handle passed back into
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

    /// `UploadPart`: PUTs `data` as the request body. Returns `(backend_etag,
    /// part_hash_bytes)` — `backend_etag` is S3's own `ETag` response header
    /// (its surrounding quotes stripped), fed back verbatim into
    /// `complete_multipart`; `part_hash_bytes` is **this gear's own**
    /// SHA-256 of `data`, computed locally rather than derived from S3's
    /// (MD5-based) `ETag`, per the trait's hash convention.
    async fn upload_part(
        &self,
        path: &str,
        upload_handle: &str,
        part_number: u32,
        _part_offset: u64,
        data: Bytes,
    ) -> Result<(String, Vec<u8>), DomainError> {
        let part_hash = hash::sha256(&data);

        // S3's documented limit is 10,000 parts per upload (1..=10_000,
        // 1-indexed) — narrower than `u16::try_from`'s 65_535 ceiling, so that
        // conversion alone would silently accept out-of-range part numbers
        // S3 itself would reject.
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

    /// `CompleteMultipartUpload`: builds the request XML body from `parts`'
    /// backend `ETag`s via `finalize_multipart` (the shared POST helper that
    /// does **not** re-read the object), then builds the ADR-0006
    /// offset-manifest and its `root` from the `(offset, part_hash)` pairs the
    /// caller already collected during upload — **no `GetObject` re-read of the
    /// assembled object**. This removes the mandatory whole-object re-download
    /// on every completed multipart upload (S3's own multipart `ETag` is an
    /// `md5-of-part-md5s` construction and could not serve as this gear's digest
    /// anyway; the manifest root is a plain SHA-256 construction that a client
    /// can independently re-derive from object bytes + the returned manifest).
    async fn complete_multipart(
        &self,
        path: &str,
        upload_handle: &str,
        parts: &[MultipartCompletionPart],
    ) -> Result<(Manifest, [u8; 32]), DomainError> {
        // S3's native completion still needs the (part_number, backend_etag)
        // pairs to assemble the object; it does not need the offsets/hashes.
        let etag_parts: Vec<(u32, String)> = parts
            .iter()
            .map(|(part_number, _, _, etag)| (*part_number, etag.clone()))
            .collect();
        self.finalize_multipart(path, upload_handle, &etag_parts)
            .await?;

        // @cpt-cf-file-storage-algo-content-hash-modes-build-manifest
        build_manifest_and_root(parts)
    }

    /// `AbortMultipartUpload`: discards all previously uploaded parts.
    async fn abort_multipart(&self, path: &str, upload_handle: &str) -> Result<(), DomainError> {
        let key = Self::path_to_key(path);
        let url = self
            .bucket
            .abort_multipart_upload(Some(&self.credentials), key, upload_handle)
            .sign(SIGN_DURATION);
        self.send_and_check(self.http.delete(url)).await?;
        Ok(())
    }

    /// `ListObjectsV2`, looping on the continuation token until the response
    /// is no longer truncated. Every returned `Key` is converted back to this
    /// gear's `"/{file_id}/{version_id}"` path convention via `key_to_path`.
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

    /// Readiness probe: `ListObjectsV2` (`max-keys=1`) against the bucket
    /// itself, not a `HeadObject` against a well-known probe key.
    ///
    /// `HeadObject` cannot distinguish "bucket exists, probe key absent"
    /// from "bucket does not exist (or is misconfigured)": both come back as
    /// a bare `404` with no body — HEAD responses never carry one, so there
    /// is nothing in the response to tell `NoSuchBucket` apart from
    /// `NoSuchKey`. `exists`'s 404-means-absent mapping (correct for its own
    /// contract) previously leaked into readiness via this method, so a
    /// missing/misconfigured bucket reported `Ok(false)` — "reachable,
    /// object absent" — same as the expected steady-state, and `/readyz`
    /// passed while every real read/write against that backend would fail.
    ///
    /// `ListObjectsV2` is bucket-scoped: it returns `200` (with an empty
    /// `<Contents>` list) for *any* existing, accessible bucket regardless of
    /// its contents, and a genuine error status (404 `NoSuchBucket`, 403
    /// `AccessDenied`, etc.) only when the bucket itself is missing or
    /// inaccessible. `send_and_check` already maps any non-2xx status to
    /// `Err`, so success/failure of this call alone is the bucket-level
    /// signal readiness needs — the returned listing body is discarded.
    async fn is_ready(&self) -> Result<(), DomainError> {
        let mut action = self.bucket.list_objects_v2(Some(&self.credentials));
        action.with_max_keys(1);
        let url = action.sign(SIGN_DURATION);
        self.send_and_check(self.http.get(url)).await.map(|_| ())
    }
}

/// A single parsed `ListObjectsV2` response page.
struct ListObjectsPage {
    keys: Vec<String>,
    is_truncated: bool,
    next_continuation_token: Option<String>,
}

/// Parse a `ListObjectsV2` XML response body via `quick-xml`, extracting just
/// the fields `list_paths` needs. Deliberately does **not** use rusty-s3's own
/// `ListObjectsV2Response` (`instant-xml`-based) — see this module's doc
/// comment for why.
///
/// Keys are percent-decoded: `rusty_s3::Bucket::list_objects_v2` always
/// requests `encoding-type=url`, so S3 (and S3-compatible servers) percent-
/// encode `<Key>` values in the response to keep them XML-safe.
fn parse_list_objects_response(body: &[u8]) -> Result<ListObjectsPage, quick_xml::Error> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut keys = Vec::new();
    let mut is_truncated = false;
    let mut next_continuation_token = None;

    // `<Key>` is only meaningful while inside `<Contents>` (as opposed to,
    // e.g., a `<Prefix>` under `<CommonPrefixes>`).
    let mut in_contents = false;
    let mut current_tag: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if name == "Contents" {
                    in_contents = true;
                }
                current_tag = Some(name);
            }
            Event::End(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if name == "Contents" {
                    in_contents = false;
                }
                current_tag = None;
            }
            Event::Text(t) => {
                let decoded = t.decode()?;
                let text = quick_xml::escape::unescape(&decoded).map_or_else(
                    |_| decoded.clone().into_owned(),
                    std::borrow::Cow::into_owned,
                );
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

/// Parse `CreateMultipartUpload`'s XML response body
/// (`<InitiateMultipartUploadResult><UploadId>...</UploadId></InitiateMultipartUploadResult>`)
/// via `quick-xml`, extracting just the `UploadId`. Deliberately does not use
/// rusty-s3's own `instant-xml`-based `CreateMultipartUploadResponse` — see
/// this module's doc comment for why.
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
                current_tag = Some(String::from_utf8_lossy(e.local_name().as_ref()).into_owned());
            }
            Ok(Event::End(_)) => current_tag = None,
            Ok(Event::Text(t)) => {
                if current_tag.as_deref() == Some("UploadId") {
                    let Ok(decoded) = t.decode() else { continue };
                    return Some(quick_xml::escape::unescape(&decoded).map_or_else(
                        |_| decoded.clone().into_owned(),
                        std::borrow::Cow::into_owned,
                    ));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    None
}

/// Parse an S3 XML error body (`<Error><Code>...</Code><Message>...</Message></Error>`)
/// via `quick-xml`, returning `(code, message)`. Returns `None` if the body is
/// empty or not parseable (e.g. a HEAD response's empty body, or a transport
/// failure that never reached an S3-compatible server at all).
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
                current_tag = Some(String::from_utf8_lossy(e.local_name().as_ref()).into_owned());
            }
            Ok(Event::End(_)) => current_tag = None,
            Ok(Event::Text(t)) => {
                let Ok(decoded) = t.decode() else { continue };
                let text = quick_xml::escape::unescape(&decoded).map_or_else(
                    |_| decoded.clone().into_owned(),
                    std::borrow::Cow::into_owned,
                );
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

/// Minimal percent-decoder for `ListObjectsV2`'s `encoding-type=url` response
/// keys. Self-contained rather than pulling in the `percent-encoding` crate
/// for this single call site (rusty-s3 depends on it, but only privately).
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
