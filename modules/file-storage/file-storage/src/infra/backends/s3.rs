//! `s3-compatible` backend adapter.
//!
//! The only P1 backend kind. Talks to AWS S3 / MinIO / Ceph RGW / Wasabi /
//! GCS S3-compat / `s3s-fs` via `aws-sdk-s3`. Generates SigV4 PUT/GET
//! presigned URLs per ADR-0003 and (optionally) pins
//! `If-Match` / `If-None-Match: *` into the SignedHeaders set when the
//! deployer declared `PresignedConditionalPut`.

use std::time::Duration;

use async_trait::async_trait;
use aws_config::Region;
use aws_credential_types::Credentials;
use aws_sdk_s3::{
    Client as S3Client,
    config::{BehaviorVersion, Config as S3Config, SharedCredentialsProvider},
    presigning::PresigningConfig,
};
use bytes::Bytes;
use file_storage_sdk::{BackendCapability, FileMeta, PresignedDownload, UrlParams};
use futures::StreamExt;
use time::OffsetDateTime;
use tracing::warn;

use crate::domain::error::DomainError;

use super::r#trait::{
    BackendDescriptor, BackendObjectKey, BackendReadResult, HeadResult, PresignedGetItem,
    PresignedGetOutcome, StorageBackend,
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
    pub public_read_urls: bool,
}

pub struct S3Backend {
    descriptor: BackendDescriptor,
    bucket: String,
    endpoint: String,
    public_read_urls: bool,
    client: S3Client,
}

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
            public_read_urls: cfg.public_read_urls,
            client,
        }
    }

    fn supports_conditional_put(&self) -> bool {
        self.descriptor
            .capabilities()
            .contains(&BackendCapability::PresignedConditionalPut)
    }
}

#[async_trait]
impl StorageBackend for S3Backend {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn open_read(
        &self,
        key: &BackendObjectKey,
    ) -> Result<BackendReadResult, DomainError> {
        let resp = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| DomainError::backend(format!("S3 GetObject failed: {e}")))?;

        let content_hash = resp
            .e_tag()
            .map(|s| s.trim_matches('"').to_owned())
            .unwrap_or_default();

        let body = resp.body;
        let stream = body
            .into_async_read();
        let stream = tokio_util::io::ReaderStream::new(stream)
            .map(|chunk| {
                chunk
                    .map(Bytes::from)
                    .map_err(|e| file_storage_sdk::FileStorageError::BackendFailure(e.to_string()))
            });

        Ok(BackendReadResult {
            bytes: Box::pin(stream),
            content_hash,
        })
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

    async fn issue_presigned_put(
        &self,
        key: &BackendObjectKey,
        meta: &FileMeta,
        _params: &UrlParams,
        expected_etag: &str,
        ttl_seconds: u64,
    ) -> Result<String, DomainError> {
        let presign_cfg = PresigningConfig::expires_in(Duration::from_secs(ttl_seconds))
            .map_err(|e| DomainError::backend(format!("invalid presign TTL: {e}")))?;

        // Per derive_s3_key, the key has form `f/{file_id_hex}` — strip the
        // prefix to recover the file_id for object metadata.
        let file_id_hint = key.strip_prefix("f/").unwrap_or(key);
        let mut req = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(meta.mime_type.clone())
            .metadata("file-id", file_id_hint)
            .metadata("gts-file-type", meta.gts_file_type.clone());

        if self.supports_conditional_put() {
            // Pin If-None-Match: "*" on initial create. For overwrites the
            // service layer would change this to If-Match with the row's
            // current etag; in P1 the lifecycle's create-only path always
            // uses "*". The ADR-0004 hardening layer is opt-in.
            req = req.if_none_match("*");
            let _ = expected_etag;
        }

        let presigned = req
            .presigned(presign_cfg)
            .await
            .map_err(|e| DomainError::backend(format!("S3 presign PUT failed: {e}")))?;
        Ok(presigned.uri().to_string())
    }

    async fn issue_presigned_gets(
        &self,
        items: Vec<PresignedGetItem>,
    ) -> Result<Vec<PresignedGetOutcome>, DomainError> {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let result = self.presign_one_get(&item).await;
            out.push(PresignedGetOutcome {
                key: item.key,
                result,
            });
        }
        Ok(out)
    }

    async fn head_object(
        &self,
        key: &BackendObjectKey,
    ) -> Result<HeadResult, DomainError> {
        let resp = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| DomainError::backend(format!("S3 HeadObject failed: {e}")))?;
        let content_hash = resp
            .e_tag()
            .map(|s| s.trim_matches('"').to_owned())
            .unwrap_or_default();
        let size_bytes = resp
            .content_length()
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(0);
        Ok(HeadResult {
            content_hash,
            size_bytes,
        })
    }
}

impl S3Backend {
    async fn presign_one_get(
        &self,
        item: &PresignedGetItem,
    ) -> Result<PresignedDownload, DomainError> {
        // Public-read short-circuit.
        if self.public_read_urls
            || self
                .descriptor
                .capabilities()
                .contains(&BackendCapability::PublicReadUrls)
        {
            // Bare HTTPS URL (path-style — works against all S3-compatibles).
            let url = format!(
                "{}/{}/{}",
                self.endpoint.trim_end_matches('/'),
                self.bucket,
                item.key
            );
            // 50-year far-future expiration for public links.
            let far_future = OffsetDateTime::now_utc() + time::Duration::days(365 * 50);
            return Ok(PresignedDownload {
                url,
                expires_at: far_future,
                is_public: true,
            });
        }

        let presign_cfg = PresigningConfig::expires_in(Duration::from_secs(item.expires_in_seconds))
            .map_err(|e| DomainError::backend(format!("invalid presign TTL: {e}")))?;

        let mut req = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&item.key);
        if let Some(disp) = item
            .params
            .content_disposition
            .clone()
            .or_else(|| item.display_name_hint.as_ref().map(|n| format!(r#"attachment; filename="{n}""#)))
        {
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
            + time::Duration::seconds(
                i64::try_from(item.expires_in_seconds).unwrap_or(i64::MAX),
            );
        Ok(PresignedDownload {
            url: presigned.uri().to_string(),
            expires_at,
            is_public: false,
        })
    }
}

#[allow(dead_code)]
fn _unused_warn_marker() {
    warn!("placeholder");
}
