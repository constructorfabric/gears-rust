//! `FileService` — control-plane business logic.
//!
//! Owns the P1 flows: create + presign upload, finalize + bind (optimistic CAS),
//! download-URL issuance, metadata CRUD, listing, versioning, and delete. It
//! depends on the [`Store`] persistence facade (tenant-scoped persistence), the
//! backend registry (byte storage), the signed-URL issuer, and an [`Authorizer`].
//! Content bytes never flow through this service — they move via
//! [`crate::domain::data_plane::DataPlaneService`].
//!
//! ## Module layout (path-split to stay ≤ 600 SLOC per file)
//!
//! The impl block is spread across sibling files; shared types and the struct
//! definition live here:
//! - `create.rs`   — create_file, presign_version, policy/quota helpers
//! - `write.rs`    — authorize_write, finalize_upload, bind, update_metadata,
//!   transfer_ownership, best_effort_blob_delete
//! - `read_ops.rs` — get_file, get_file_with_metadata, list_files, get_version,
//!   download_url, list_versions, restore_version,
//!   delete_file, delete_file_inner, delete_version
//! - `backend.rs`  — migrate_backend, list_backends, get_backend,
//!   DataPlanePort trait impl

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
    /// Short default TTL (seconds) stamped on every signed URL; the issuer caps
    /// it at `max_url_ttl` (DESIGN §4.5).
    pub default_url_ttl_secs: i64,
    pub sidecar_base_url: String,
    pub default_page_size: u64,
    pub max_page_size: u64,
    /// Window (seconds) for which an idempotency key is retained.
    /// After this window, a retry with the same key is treated as a fresh request.
    ///
    /// @cpt-cf-file-storage-fr-upload-idempotency
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

/// Outcome of the token-authenticated finalize callback (upload-flow
/// redesign) — the single-part half of the ONE shared bind-state model
/// (`domain::multipart::BindState`): surfaced to the uploading client by the
/// sidecar as fixed `PUT`-response headers — `X-FS-Bound: true` + `ETag` on
/// a won CAS, `X-FS-Bound: conflict` + `X-FS-Current-ETag` on a lost one.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone)]
pub struct FinalizeByTokenOutcome {
    /// `None` — the token did not request a bind (staged/manual mode).
    /// `Some(Bound)` / `Some(Conflict)` otherwise (`Manual` is never
    /// produced here — a manual-mode token simply has no bind claim).
    pub bind_state: Option<crate::domain::multipart::BindState>,
    /// Content ETag after a successful bind (`Bound` only).
    pub etag: Option<String>,
    /// The file's CURRENT content ETag on a lost CAS (`Conflict` only) —
    /// what a manual rebind's `If-Match` needs, no re-upload required.
    pub current_etag: Option<String>,
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
/// @cpt-cf-file-storage-fr-storage-quota
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
    /// Optional quota enforcement client. `None` means no quota check is
    /// performed (permissive). When present, errors from the client deny the
    /// request (fail-closed: a quota check failure is safer than allowing
    /// potentially unbounded storage growth).
    pub(super) quota_client: Option<Arc<dyn QuotaClient>>,
    /// Optional usage reporter. `None` means no usage deltas are reported.
    /// Failures are fire-and-forget: the adapter logs and swallows them.
    ///
    /// @cpt-cf-file-storage-fr-usage-reporting
    pub(super) usage_reporter: Option<Arc<dyn UsageReporter>>,
    /// Metrics port (P2 1.8 remediation). Defaults to a no-op implementation
    /// (see [`Self::new`]); `gear.rs` opts into the real OTel-backed meter via
    /// [`Self::with_metrics`].
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

    /// Install a real metrics port (P2 1.8 remediation). Kept as a builder
    /// step rather than a `new()` parameter so the ~40 existing
    /// `FileService::new(...)` call sites across the integration-test suite
    /// keep compiling unchanged; only `gear.rs` needs to opt in.
    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<dyn FileStorageMetricsPort>) -> Self {
        self.metrics = metrics;
        self
    }

    // ── helpers ─────────────────────────────────────────────────────────────

    pub(super) fn tenant_scope(ctx: &SecurityContext) -> AccessScope {
        AccessScope::for_tenant(ctx.subject_tenant_id())
    }

    pub(super) fn backend_path(file_id: Uuid, version_id: Uuid) -> String {
        format!("/{file_id}/{version_id}")
    }

    pub(super) fn validate_gts_type(t: &str) -> Result<(), DomainError> {
        if gts::GtsTypeId::try_new(t).is_ok() {
            Ok(())
        } else {
            Err(DomainError::invalid_gts_type(t))
        }
    }

    /// Return the token verifier backed by the control plane's signing key.
    /// The data-plane finalize handler uses this to validate the sidecar's
    /// upload token without knowing the private key.
    #[must_use]
    pub fn verifier(&self) -> crate::infra::signed_url::Verifier {
        self.issuer.verifier()
    }

    /// Mint a signed URL for `op` against `v`.
    ///
    /// `download_meta` is `Some((content_type, etag))` for `Op::Get` tokens
    /// only (P2 1.11) — the version's stored MIME and content ETag, so the
    /// sidecar can emit real `Content-Type`/`ETag` response headers without a
    /// DB lookup. It is silently ignored (never populated in the claims) for
    /// any other `op`; non-GET call sites pass `None`.
    pub(super) fn sign_url(
        &self,
        op: Op,
        v: &VersionRef,
        constraints: UploadConstraints,
        download_meta: Option<(String, String)>,
    ) -> Result<String, DomainError> {
        self.sign_url_with_bind(op, v, constraints, download_meta, false)
    }

    /// [`Self::sign_url`] with an explicit `bind_on_finalize` claim
    /// (upload-flow redesign). Only the new-file `create_file` path with
    /// `bind: "auto"` passes `true` — see `Claims::bind_on_finalize`.
    pub(super) fn sign_url_with_bind(
        &self,
        op: Op,
        v: &VersionRef,
        constraints: UploadConstraints,
        download_meta: Option<(String, String)>,
        bind_on_finalize: bool,
    ) -> Result<String, DomainError> {
        // P2 2.13: resolve (and validate) the path segment before doing any
        // signing work, so a rejected `op` never wastes a token mint.
        let verb = content_verb(op)?;
        let now = OffsetDateTime::now_utc();
        // P2 1.11: only a GET (download) token ever carries content_type/etag.
        let (content_type, etag) = match op {
            Op::Get => download_meta.unwrap_or_default(),
            Op::Put | Op::MultipartPart => (String::new(), String::new()),
        };
        // P2 1.8: mint a fresh correlation id per signed URL. The sidecar
        // echoes it back as `x-request-id` on its finalize callback so both
        // planes' logs can be joined on the same id.
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
            bind_on_finalize,
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

    // ── audit helpers (P2-M4) ────────────────────────────────────────────────

    /// Extract a stable actor kind string from the `SecurityContext`.
    // @cpt-begin:cpt-cf-file-storage-algo-audit-trail-build-entry:p1:inst-buildentry-actor-kind
    pub(super) fn actor_kind(ctx: &SecurityContext) -> &'static str {
        match ctx.subject_type() {
            Some("app") => "app",
            _ => "user",
        }
    }
    // @cpt-end:cpt-cf-file-storage-algo-audit-trail-build-entry:p1:inst-buildentry-actor-kind

    /// Build a success audit entry for a file-scoped write operation.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-dod:cpt-cf-file-storage-dod-audit-trail-transactional-write:p1
    // @cpt-begin:cpt-cf-file-storage-algo-audit-trail-build-entry:p1:inst-buildentry-identity
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
    // @cpt-end:cpt-cf-file-storage-algo-audit-trail-build-entry:p1:inst-buildentry-identity

    // ── usage reporting helpers (P2-M5) ──────────────────────────────────────

    /// Fire-and-forget usage delta report. Failures are logged but never
    /// propagated — a failing usage reporter must not block file operations.
    ///
    /// @cpt-cf-file-storage-fr-usage-reporting
    // @cpt-begin:cpt-cf-file-storage-algo-ownership-transfer-usage-rebalance:p1:inst-rebalance-noop-if-unwired
    pub(super) fn report_usage(&self, delta: UsageDelta) {
        if let Some(reporter) = self.usage_reporter.clone() {
            tokio::spawn(async move {
                reporter.report(delta).await;
            });
        }
    }
    // @cpt-end:cpt-cf-file-storage-algo-ownership-transfer-usage-rebalance:p1:inst-rebalance-noop-if-unwired

    /// Build a [`FileEvent`] for a write operation.
    ///
    /// @cpt-cf-file-storage-fr-file-events
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

/// Map a [`sign_url`](FileService::sign_url) [`Op`] to its sidecar path
/// segment (`/api/file-storage-data/v1/{verb}/{file}/{version}`).
///
/// P2 2.13: `Op::MultipartPart` is rejected rather than mapped. A multipart
/// *part* upload lives at a distinct sidecar route —
/// `/api/file-storage-data/v1/multipart/{file}/{version}/parts/{part}`
/// (`sidecar.rs`'s `upload_multipart_part` route) — and needs part-specific
/// claims (`upload_id`, `part_number`, `offset`, exact `size`) that this
/// generic two-segment template has no way to carry. Mapping it to
/// `"multipart-part"` here previously produced
/// `/api/file-storage-data/v1/multipart-part/{file}/{version}`, a URL the
/// sidecar does not serve (would 404). `MultipartService::initiate` already
/// mints correct part URLs directly against the real route and is the single
/// source of truth for that shape, so `sign_url` must never be asked to mint
/// one — reject instead of re-deriving (and risking re-breaking) that
/// mapping here.
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

/// Serializable form of `UploadTicket` stored in the idempotency record.
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
