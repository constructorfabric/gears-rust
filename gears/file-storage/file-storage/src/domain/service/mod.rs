//! `FileService` — control-plane business logic (create/presign, finalize/bind with
//! optimistic CAS, downloads, metadata, listing, versioning, delete). Content bytes never
//! flow through it; they move via `crate::domain::data_plane::DataPlaneService`.
//!
//! The impl is split across `create.rs`, `write.rs`, `read_ops.rs` and `backend.rs`;
//! shared types and the struct live here.

// Domain terms (ETag, If-Match, FileStorage, GET/PUT) recur throughout the docs.
#![allow(clippy::doc_markdown)]

use std::sync::Arc;

use time::OffsetDateTime;
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::audit::{AuditEntry, AuditOperation, FileEvent};
use crate::domain::authz::Authorizer;
use crate::domain::error::DomainError;
use crate::domain::ports::FileStorageMetricsPort;
use crate::infra::backend::BackendRegistry;
use crate::infra::external_clients::{QuotaClient, UsageDelta, UsageReporter};
use crate::infra::metrics::NoopMetrics;
use crate::infra::signed_url::{Claims, Issuer, MultipartClaims, Op, UploadConstraints};
use crate::infra::storage::Store;

mod backend;
mod create;
mod read_ops;
mod write;

/// Service-level configuration distilled from [`crate::config::FileStorageConfig`].
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// Default TTL (seconds) of every signed URL; the issuer caps it at `max_url_ttl`.
    pub default_url_ttl_secs: i64,
    pub sidecar_base_url: String,
    pub default_page_size: u64,
    pub max_page_size: u64,
    /// Seconds an idempotency key is retained; afterwards a retry is a fresh request.
    pub idempotency_ttl_secs: u64,
}

/// Result of creating a file or presigning a new version: identity plus the
/// signed URL the client `PUT`s the bytes to.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone)]
pub struct UploadTicket {
    pub file_id: Uuid,
    pub version_id: Uuid,
    pub upload_url: String,
}

/// Result of `download-url`: the signed URL plus the content ETag.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone)]
pub struct DownloadTicket {
    pub download_url: String,
    pub etag: String,
    pub version_id: Uuid,
}

/// Quota metric name used for storage preflight checks.
pub(super) const QUOTA_METRIC_NAME: &str =
    gts_id!("cf.qe.metric.type.v1~cf.qe.metric.file_storage_bytes.v1");

/// The control-plane file service.
#[allow(unknown_lints, de0309_must_have_domain_model)]
pub struct FileService {
    pub(super) store: Store,
    pub(super) backends: BackendRegistry,
    pub(super) issuer: Arc<Issuer>,
    pub(super) authorizer: Arc<dyn Authorizer>,
    pub(super) cfg: ServiceConfig,
    /// `None` disables quota checks; when present, client errors deny the request (fail-closed).
    pub(super) quota_client: Option<Arc<dyn QuotaClient>>,
    /// `None` disables usage reporting; failures are logged and swallowed.
    pub(super) usage_reporter: Option<Arc<dyn UsageReporter>>,
    /// Defaults to a no-op; `with_metrics` installs the real meter.
    pub(super) metrics: Arc<dyn FileStorageMetricsPort>,
}

impl FileService {
    pub fn new(
        store: Store,
        backends: BackendRegistry,
        issuer: Arc<Issuer>,
        authorizer: Arc<dyn Authorizer>,
        cfg: ServiceConfig,
        quota_client: Option<Arc<dyn QuotaClient>>,
        usage_reporter: Option<Arc<dyn UsageReporter>>,
    ) -> Self {
        Self {
            store,
            backends,
            issuer,
            authorizer,
            cfg,
            quota_client,
            usage_reporter,
            metrics: Arc::new(NoopMetrics),
        }
    }

    /// Install a real metrics port (a builder step so `new()` keeps its signature).
    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<dyn FileStorageMetricsPort>) -> Self {
        self.metrics = metrics;
        self
    }

    pub(super) fn tenant_scope(ctx: &SecurityContext) -> AccessScope {
        AccessScope::for_tenant(ctx.subject_tenant_id())
    }

    pub(super) fn backend_path(file_id: Uuid, version_id: Uuid) -> String {
        crate::domain::storage_layout::backend_path(file_id, version_id)
    }

    pub(super) fn validate_gts_type(t: &str) -> Result<(), DomainError> {
        if gts::GtsTypeId::try_new(t).is_ok() {
            Ok(())
        } else {
            Err(DomainError::invalid_gts_type(t))
        }
    }

    /// Token verifier backed by the control plane's signing key (used by the finalize
    /// handler to validate the sidecar's upload token).
    #[must_use]
    pub fn verifier(&self) -> crate::infra::signed_url::Verifier {
        self.issuer.verifier()
    }

    /// Mint a signed URL for `op` against `v`.
    ///
    /// `download_meta` is `Some((content_type, etag))` for `Op::Get` only, so the sidecar can
    /// emit `Content-Type`/`ETag` without a DB lookup; it is ignored for other ops.
    pub(super) fn sign_url(
        &self,
        op: Op,
        v: &VersionRef,
        constraints: UploadConstraints,
        download_meta: Option<(String, String)>,
    ) -> Result<String, DomainError> {
        let verb = content_verb(op)?;
        let now = OffsetDateTime::now_utc();
        let (content_type, etag) = match op {
            Op::Get => download_meta.unwrap_or_default(),
            Op::Put | Op::MultipartPart => (String::new(), String::new()),
        };
        // Fresh correlation id per URL; the sidecar echoes it as `x-request-id` on finalize.
        let claims = Claims {
            op,
            file_id: v.file_id,
            version_id: v.version_id,
            backend_id: v.backend_id.clone(),
            backend_path: v.backend_path.clone(),
            exp: now.unix_timestamp() + self.cfg.default_url_ttl_secs,
            upload: constraints,
            multipart: MultipartClaims::default(),
            request_id: Uuid::now_v7().to_string(),
            content_type,
            etag,
        };
        let token = self.issuer.issue(claims, now)?;
        Ok(format!(
            "{}/api/file-storage-data/v1/{}/{}/{}?fs-token={}",
            self.cfg.sidecar_base_url.trim_end_matches('/'),
            verb,
            v.file_id,
            v.version_id,
            token
        ))
    }

    /// Stable actor kind string for the audit log.
    pub(super) fn actor_kind(ctx: &SecurityContext) -> &'static str {
        match ctx.subject_type() {
            Some("app") => "app",
            _ => "user",
        }
    }

    /// Build a success audit entry for a file-scoped write operation.
    pub(super) fn audit_ok(
        ctx: &SecurityContext,
        file_id: Option<Uuid>,
        operation: AuditOperation,
        detail: serde_json::Value,
    ) -> AuditEntry {
        AuditEntry::success(
            ctx.subject_tenant_id(),
            Self::actor_kind(ctx),
            ctx.subject_id(),
            file_id,
            operation,
            detail,
        )
    }

    /// Fire-and-forget usage delta; a failing reporter must not block file operations.
    pub(super) fn report_usage(&self, delta: UsageDelta) {
        if let Some(reporter) = self.usage_reporter.clone() {
            tokio::spawn(async move {
                reporter.report(delta).await;
            });
        }
    }

    pub(super) fn make_file_event(
        tenant_id: Uuid,
        owner_id: Uuid,
        file_id: Uuid,
        event_type: &str,
        payload: serde_json::Value,
    ) -> FileEvent {
        FileEvent {
            tenant_id,
            owner_id,
            file_id,
            event_type: event_type.to_owned(),
            payload,
        }
    }
}

/// Map an `Op` to its sidecar path segment.
///
/// `Op::MultipartPart` is rejected: part uploads use a distinct route with part-specific
/// claims, and `MultipartService::initiate` is the single place that mints those URLs.
fn content_verb(op: Op) -> Result<&'static str, DomainError> {
    match op {
        Op::Get => Ok("download"),
        Op::Put => Ok("upload"),
        Op::MultipartPart => Err(DomainError::InternalError),
    }
}

/// A minimal reference to a version's backend location, for URL signing.
#[allow(unknown_lints, de0309_must_have_domain_model)]
pub(super) struct VersionRef {
    pub(super) file_id: Uuid,
    pub(super) version_id: Uuid,
    pub(super) backend_id: String,
    pub(super) backend_path: String,
}

/// Serializable `UploadTicket` stored in the idempotency record.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct IdempotencyTicket {
    pub(super) file_id: Uuid,
    pub(super) version_id: Uuid,
    pub(super) upload_url: String,
}

impl From<IdempotencyTicket> for UploadTicket {
    fn from(t: IdempotencyTicket) -> Self {
        Self {
            file_id: t.file_id,
            version_id: t.version_id,
            upload_url: t.upload_url,
        }
    }
}

#[cfg(test)]
mod service_tests;
