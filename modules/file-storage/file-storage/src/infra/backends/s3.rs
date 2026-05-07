//! `s3-compatible` backend adapter.
//!
//! The only P1 backend kind. Talks to AWS S3 / MinIO / Ceph RGW / Wasabi /
//! GCS S3-compat / `s3s-fs` via `aws-sdk-s3`. Generates SigV4 PUT/GET
//! presigned URLs per ADR-0003. Multipart upload is the only upload path
//! (per `cpt-cf-file-storage-fr-multipart-upload`).

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use aws_config::Region;
use aws_credential_types::Credentials;
use aws_sdk_s3::{
    Client as S3Client,
    config::{BehaviorVersion, Config as S3Config, SharedCredentialsProvider},
    presigning::PresigningConfig,
    types::{CompletedMultipartUpload, CompletedPart, MetadataDirective},
};
use bytes::Bytes;
use file_storage_sdk::{ByteRange, FileMeta, PresignedDownload, ResolvedByteRange, UploadedPart};
use futures::StreamExt;
use time::OffsetDateTime;

use crate::domain::error::DomainError;

use super::r#trait::{
    BackendDescriptor, BackendObjectKey, BackendObjectMetadata, BackendReadResult,
    CopyObjectResult, MultipartCompleteResult, PresignedGetItem, PresignedGetOutcome,
    StorageBackend,
};

/// Configuration consumed by the S3 backend at construction time.
#[derive(Debug, Clone)]
pub struct S3BackendConfig {
    pub descriptor: BackendDescriptor,
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
}

// @cpt-begin:cpt-cf-file-storage-component-s3-backend:p1:inst-s3-backend-struct
// @cpt-begin:cpt-cf-file-storage-adr-presigned-put-sigv4:p1:inst-s3-backend-struct
pub struct S3Backend {
    descriptor: BackendDescriptor,
    bucket: String,
    endpoint: String,
    client: S3Client,
}
// @cpt-end:cpt-cf-file-storage-component-s3-backend:p1:inst-s3-backend-struct
// @cpt-end:cpt-cf-file-storage-adr-presigned-put-sigv4:p1:inst-s3-backend-struct

impl S3Backend {
    pub fn new(cfg: S3BackendConfig) -> Self {
        let creds = Credentials::new(
            cfg.access_key.clone(),
            cfg.secret_key.clone(),
            None,
            None,
            "file-storage-static",
        );
        let s3_cfg = S3Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(cfg.region.clone()))
            .endpoint_url(cfg.endpoint.clone())
            .credentials_provider(SharedCredentialsProvider::new(creds))
            .force_path_style(true)
            .build();
        let client = S3Client::from_conf(s3_cfg);
        Self {
            descriptor: cfg.descriptor,
            bucket: cfg.bucket,
            endpoint: cfg.endpoint,
            client,
        }
    }

    fn build_object_metadata(&self, meta: &FileMeta) -> Vec<(String, String)> {
        // Mirror to x-amz-meta-* — `gts_file_type` is intentionally NOT
        // mirrored (DB-only per `cpt-cf-file-storage-constraint-etag-content-only`).
        let mut out: Vec<(String, String)> = meta
            .custom_metadata
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        // Pin display name into x-amz-meta- as a hint.
        out.push(("name".to_string(), meta.name.clone()));
        out
    }

    fn render_range(range: ByteRange) -> String {
        match range {
            ByteRange::Inclusive { start, end } => format!("bytes={start}-{end}"),
            ByteRange::From(start) => format!("bytes={start}-"),
            ByteRange::Suffix(n) => format!("bytes=-{n}"),
        }
    }

    fn parse_content_range(s: &str) -> Option<ResolvedByteRange> {
        // "bytes START-END/TOTAL"
        let s = s.trim();
        let s = s.strip_prefix("bytes ")?;
        let (range_part, total_part) = s.split_once('/')?;
        let (start_s, end_s) = range_part.split_once('-')?;
        let start: u64 = start_s.parse().ok()?;
        let end_inclusive: u64 = end_s.parse().ok()?;
        let total: u64 = total_part.parse().ok()?;
        Some(ResolvedByteRange {
            start,
            end_inclusive,
            total,
        })
    }
}

#[async_trait]
impl StorageBackend for S3Backend {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    // ── Multipart upload ────────────────────────────────────────────────────

    async fn create_multipart_upload(
        &self,
        key: &BackendObjectKey,
        meta: &FileMeta,
    ) -> Result<String, DomainError> {
        let mut req = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .content_type(meta.mime_type.clone());
        for (k, v) in self.build_object_metadata(meta) {
            req = req.metadata(k, v);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| DomainError::backend(format!("S3 CreateMultipartUpload failed: {e}")))?;
        resp.upload_id()
            .map(|s| s.to_owned())
            .ok_or_else(|| DomainError::backend("CreateMultipartUpload returned no upload_id"))
    }

    async fn presign_upload_parts(
        &self,
        key: &BackendObjectKey,
        upload_id: &str,
        part_count: u32,
        ttl_seconds: u64,
    ) -> Result<Vec<String>, DomainError> {
        if part_count == 0 || part_count > 10_000 {
            return Err(DomainError::bad_request(format!(
                "part_count {part_count} out of range (must be 1..=10000)"
            )));
        }
        let presign_cfg = PresigningConfig::expires_in(Duration::from_secs(ttl_seconds))
            .map_err(|e| DomainError::backend(format!("invalid presign TTL: {e}")))?;
        let mut urls = Vec::with_capacity(part_count as usize);
        for n in 1..=part_count {
            let presigned = self
                .client
                .upload_part()
                .bucket(&self.bucket)
                .key(key)
                .upload_id(upload_id)
                .part_number(n as i32)
                .presigned(presign_cfg.clone())
                .await
                .map_err(|e| {
                    DomainError::backend(format!("S3 presign UploadPart#{n} failed: {e}"))
                })?;
            urls.push(presigned.uri().to_string());
        }
        Ok(urls)
    }

    async fn complete_multipart_upload(
        &self,
        key: &BackendObjectKey,
        upload_id: &str,
        parts: &[UploadedPart],
    ) -> Result<MultipartCompleteResult, DomainError> {
        let completed_parts: Vec<CompletedPart> = parts
            .iter()
            .map(|p| {
                CompletedPart::builder()
                    .part_number(p.part_number as i32)
                    .e_tag(p.etag.clone())
                    .build()
            })
            .collect();
        let completed = CompletedMultipartUpload::builder()
            .set_parts(Some(completed_parts))
            .build();

        let resp = self
            .client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .multipart_upload(completed)
            .send()
            .await
            .map_err(|e| DomainError::backend(format!("S3 CompleteMultipartUpload failed: {e}")))?;

        let etag = resp
            .e_tag()
            .map(|s| s.trim_matches('"').to_owned())
            .ok_or_else(|| DomainError::backend("CompleteMultipartUpload returned no ETag"))?;
        let version_id = resp.version_id().map(|s| s.to_owned());

        // S3 Complete response does not carry size; HEAD to capture it.
        let head = self.head_object(key).await?;
        Ok(MultipartCompleteResult {
            etag,
            version_id,
            size_bytes: head.size_bytes,
        })
    }

    async fn abort_multipart_upload(
        &self,
        key: &BackendObjectKey,
        upload_id: &str,
    ) -> Result<(), DomainError> {
        match self
            .client
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await
        {
            Ok(_) => Ok(()),
            // Idempotent: NoSuchUpload is fine.
            Err(e) => {
                let s = format!("{e}");
                if s.contains("NoSuchUpload") {
                    Ok(())
                } else {
                    Err(DomainError::backend(format!(
                        "S3 AbortMultipartUpload failed: {e}"
                    )))
                }
            }
        }
    }

    // ── Read / metadata ─────────────────────────────────────────────────────

    async fn open_read(
        &self,
        key: &BackendObjectKey,
        range: Option<ByteRange>,
    ) -> Result<BackendReadResult, DomainError> {
        let mut req = self.client.get_object().bucket(&self.bucket).key(key);
        if let Some(r) = range {
            req = req.range(Self::render_range(r));
        }
        let resp = req
            .send()
            .await
            .map_err(|e| DomainError::backend(format!("S3 GetObject failed: {e}")))?;

        let etag = resp
            .e_tag()
            .map(|s| s.trim_matches('"').to_owned())
            .unwrap_or_default();
        let version_id = resp.version_id().map(|s| s.to_owned());
        let size_bytes = resp
            .content_length()
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(0);
        let content_type = resp.content_type().map(|s| s.to_owned());
        let content_disposition = resp.content_disposition().map(|s| s.to_owned());
        let user_metadata: BTreeMap<String, String> = resp
            .metadata()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let resolved_range = if range.is_some() {
            resp.content_range().and_then(Self::parse_content_range)
        } else {
            None
        };

        let body = resp.body;
        let stream = body.into_async_read();
        let stream = tokio_util::io::ReaderStream::new(stream).map(|chunk| {
            chunk
                .map(Bytes::from)
                .map_err(|e| file_storage_sdk::FileStorageError::BackendFailure(e.to_string()))
        });

        Ok(BackendReadResult {
            bytes: Box::pin(stream),
            metadata: BackendObjectMetadata {
                etag,
                version_id,
                size_bytes,
                content_type,
                content_disposition,
                user_metadata,
            },
            range: resolved_range,
        })
    }

    async fn head_object(
        &self,
        key: &BackendObjectKey,
    ) -> Result<BackendObjectMetadata, DomainError> {
        let resp = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| DomainError::backend(format!("S3 HeadObject failed: {e}")))?;
        let etag = resp
            .e_tag()
            .map(|s| s.trim_matches('"').to_owned())
            .unwrap_or_default();
        let version_id = resp.version_id().map(|s| s.to_owned());
        let size_bytes = resp
            .content_length()
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(0);
        let content_type = resp.content_type().map(|s| s.to_owned());
        let content_disposition = resp.content_disposition().map(|s| s.to_owned());
        let user_metadata: BTreeMap<String, String> = resp
            .metadata()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        Ok(BackendObjectMetadata {
            etag,
            version_id,
            size_bytes,
            content_type,
            content_disposition,
            user_metadata,
        })
    }

    async fn copy_object_self_replace_meta(
        &self,
        key: &BackendObjectKey,
        meta: &FileMeta,
        if_match_etag: Option<&str>,
    ) -> Result<CopyObjectResult, DomainError> {
        let copy_source = format!("{}/{}", self.bucket, key);
        let mut req = self
            .client
            .copy_object()
            .bucket(&self.bucket)
            .key(key)
            .copy_source(copy_source)
            .metadata_directive(MetadataDirective::Replace)
            .content_type(meta.mime_type.clone());
        for (k, v) in self.build_object_metadata(meta) {
            req = req.metadata(k, v);
        }
        if let Some(etag) = if_match_etag {
            req = req.copy_source_if_match(format!("\"{etag}\""));
        }
        let resp = req
            .send()
            .await
            .map_err(|e| DomainError::backend(format!("S3 CopyObject failed: {e}")))?;

        let etag = resp
            .copy_object_result()
            .and_then(|r| r.e_tag())
            .map(|s| s.trim_matches('"').to_owned())
            .ok_or_else(|| DomainError::backend("CopyObject returned no ETag"))?;
        let version_id = resp.version_id().map(|s| s.to_owned());
        Ok(CopyObjectResult { etag, version_id })
    }

    async fn delete_object(&self, key: &BackendObjectKey) -> Result<(), DomainError> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| DomainError::backend(format!("S3 DeleteObject failed: {e}")))?;
        Ok(())
    }

    // ── Presigned download URLs ─────────────────────────────────────────────

    async fn issue_presigned_gets(
        &self,
        items: Vec<PresignedGetItem>,
    ) -> Result<Vec<PresignedGetOutcome>, DomainError> {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let result = self.presign_one_get(&item).await;
            out.push(PresignedGetOutcome {
                key: item.key.clone(),
                result,
            });
        }
        Ok(out)
    }
}

impl S3Backend {
    async fn presign_one_get(
        &self,
        item: &PresignedGetItem,
    ) -> Result<PresignedDownload, DomainError> {
        let cap = item.capability.as_str();
        let is_public = cap.starts_with("download.s3.public.");
        let is_versioned = cap.ends_with(".versioned.v1");

        if is_public {
            // Bare HTTPS path-style URL — works against all S3-compatibles.
            let mut url = format!(
                "{}/{}/{}",
                self.endpoint.trim_end_matches('/'),
                self.bucket,
                item.key
            );
            if is_versioned {
                if let Some(vid) = &item.version_id {
                    url.push_str(&format!("?versionId={vid}"));
                }
            }
            let far_future = OffsetDateTime::now_utc() + time::Duration::days(365 * 50);
            return Ok(PresignedDownload {
                url,
                expires_at: far_future,
                is_public: true,
            });
        }

        // SigV4-signed GET.
        let presign_cfg =
            PresigningConfig::expires_in(Duration::from_secs(item.expires_in_seconds))
                .map_err(|e| DomainError::backend(format!("invalid presign TTL: {e}")))?;
        let mut req = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&item.key);
        if is_versioned {
            if let Some(vid) = &item.version_id {
                req = req.version_id(vid.clone());
            }
        }
        if let Some(disp) = item.params.content_disposition.clone().or_else(|| {
            item.display_name_hint
                .as_ref()
                .map(|n| format!(r#"attachment; filename="{n}""#))
        }) {
            req = req.response_content_disposition(disp);
        }
        if let Some(ct) = item
            .params
            .content_type_override
            .clone()
            .or_else(|| item.mime_type_hint.clone())
        {
            req = req.response_content_type(ct);
        }

        let presigned = req
            .presigned(presign_cfg)
            .await
            .map_err(|e| DomainError::backend(format!("S3 presign GET failed: {e}")))?;
        let expires_at = OffsetDateTime::now_utc()
            + time::Duration::seconds(i64::try_from(item.expires_in_seconds).unwrap_or(i64::MAX));
        Ok(PresignedDownload {
            url: presigned.uri().to_string(),
            expires_at,
            is_public: false,
        })
    }
}
