//! `FileService` — control-plane business logic.
//!
//! Owns the P1 flows: create + presign upload, finalize + bind (optimistic CAS),
//! download-URL issuance, metadata CRUD, listing, versioning, and delete. It
//! depends on the [`Store`] persistence facade (tenant-scoped persistence), the
//! backend registry (byte storage), the signed-URL issuer, and an [`Authorizer`].
//! Content bytes never flow through this service — they move via
//! [`crate::domain::data_plane::DataPlaneService`].

// Domain terms (ETag, If-Match, FileStorage, GET/PUT) recur throughout the docs.
#![allow(clippy::doc_markdown)]

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage_sdk::{
    CustomMetadataEntry, CustomMetadataPatch, File, FileVersion, NewFile, OwnerFilter,
};

use crate::domain::audit::{AuditEntry, AuditOperation};
use crate::domain::authz::{Authorizer, actions};
use crate::domain::error::DomainError;
use crate::domain::etag;
use crate::domain::multipart::{MultipartPart, MultipartUploadSession, MultipartUploadState};
use crate::domain::policy::{
    EffectivePolicy, PolicyBody, PolicyResolver, PolicyScope, RetentionRuleBody, RetentionScope,
    StoredPolicy, StoredRetentionRule,
};
use crate::infra::backend::{BackendCapabilities, BackendRegistry, StorageBackend};
use crate::infra::content::hash;
use crate::infra::quota::{QuotaClient, QuotaDecision};
use crate::infra::signed_url::{Claims, Issuer, Op, UploadConstraints};
use crate::infra::storage::Store;
use crate::infra::storage::repo::FileEvent;
use crate::infra::storage::store::IdempotencyInsert;
use crate::infra::usage::{UsageDelta, UsageReporter};

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
const QUOTA_METRIC_NAME: &str = "gts.cf.qe.metric.type.v1~cf.qe.metric.file_storage_bytes.v1";

/// The control-plane file service.
#[allow(unknown_lints, de0309_must_have_domain_model)]
pub struct FileService {
    store: Store,
    backends: BackendRegistry,
    issuer: Arc<Issuer>,
    authorizer: Arc<dyn Authorizer>,
    cfg: ServiceConfig,
    /// Optional quota enforcement client. `None` means no quota check is
    /// performed (permissive). When present, errors from the client deny the
    /// request (fail-closed: a quota check failure is safer than allowing
    /// potentially unbounded storage growth).
    quota_client: Option<Arc<dyn QuotaClient>>,
    /// Optional usage reporter. `None` means no usage deltas are reported.
    /// Failures are fire-and-forget: the adapter logs and swallows them.
    ///
    /// @cpt-cf-file-storage-fr-usage-reporting
    usage_reporter: Option<Arc<dyn UsageReporter>>,
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
        }
    }

    // ── helpers ─────────────────────────────────────────────────────────────

    fn tenant_scope(ctx: &SecurityContext) -> AccessScope {
        AccessScope::for_tenant(ctx.subject_tenant_id())
    }

    fn backend_path(file_id: Uuid, version_id: Uuid) -> String {
        format!("/{file_id}/{version_id}")
    }

    fn validate_gts_type(t: &str) -> Result<(), DomainError> {
        if t.starts_with("gts.") && t.contains('~') {
            Ok(())
        } else {
            Err(DomainError::invalid_gts_type(t))
        }
    }

    fn sign_url(
        &self,
        op: Op,
        v: &VersionRef,
        constraints: UploadConstraints,
    ) -> Result<String, DomainError> {
        let now = OffsetDateTime::now_utc();
        let claims = Claims {
            op,
            file_id: v.file_id,
            version_id: v.version_id,
            backend_id: v.backend_id.clone(),
            backend_path: v.backend_path.clone(),
            exp: now.unix_timestamp() + self.cfg.default_url_ttl_secs,
            upload: constraints,
        };
        let token = self.issuer.issue(claims, now)?;
        let verb = match op {
            Op::Get => "download",
            Op::Put => "upload",
        };
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
    fn actor_kind(ctx: &SecurityContext) -> &'static str {
        match ctx.subject_type() {
            Some("app") => "app",
            _ => "user",
        }
    }

    /// Build a success audit entry for a file-scoped write operation.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    fn audit_ok(
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

    // ── policy enforcement helpers (P2-M2) ───────────────────────────────────

    /// Resolve the effective policy for a given `(tenant_id, owner_id)` pair
    /// using an internal (`allow_all`) scope — callers have already been
    /// authorized for the file operation; this is a preflight check only.
    ///
    /// @cpt-cf-file-storage-fr-allowed-types-policy
    /// @cpt-cf-file-storage-fr-size-limits-policy
    /// @cpt-cf-file-storage-fr-metadata-limits
    async fn get_effective_policy_internal(
        &self,
        tenant_id: Uuid,
        owner_id: Uuid,
    ) -> Result<EffectivePolicy, DomainError> {
        let scope = AccessScope::allow_all();
        let tenant_policy = self
            .store
            .get_policy(&scope, tenant_id, &PolicyScope::Tenant, None)
            .await?;
        let user_policy = self
            .store
            .get_policy(&scope, tenant_id, &PolicyScope::User, Some(owner_id))
            .await?;
        Ok(PolicyResolver::resolve(
            tenant_policy.as_ref().map(|p| &p.body),
            user_policy.as_ref().map(|p| &p.body),
        ))
    }

    /// Returns `true` if `mime_type` matches any pattern in `allowed`.
    /// Supports exact match and `*` wildcard for subtype (e.g. `"image/*"`).
    pub(crate) fn mime_allowed(mime_type: &str, allowed: &[String]) -> bool {
        allowed.iter().any(|pat| {
            if pat == mime_type {
                return true;
            }
            // wildcard subtype: "image/*" matches "image/jpeg"
            let (pt, ps) = pat.split_once('/').unwrap_or((pat, "*"));
            let (mt, _) = mime_type.split_once('/').unwrap_or((mime_type, ""));
            ps == "*" && pt == mt
        })
    }

    /// Check that `mime_type` is permitted by the effective policy.
    ///
    /// - `None` allowed_mime_types → all types permitted (no restriction).
    /// - `Some([])` → nothing permitted.
    /// - `Some(list)` → must match a pattern in the list.
    ///
    /// @cpt-cf-file-storage-fr-allowed-types-policy
    pub(crate) fn check_allowed_mime(
        policy: &EffectivePolicy,
        mime_type: &str,
    ) -> Result<(), DomainError> {
        let Some(allowed) = &policy.allowed_mime_types else {
            return Ok(()); // no restriction
        };
        if Self::mime_allowed(mime_type, allowed) {
            Ok(())
        } else {
            Err(DomainError::policy_mime_not_allowed(mime_type))
        }
    }

    /// Compute the effective maximum blob size for `mime_type`, taking the most
    /// restrictive of:
    ///   1. Backend hardware ceiling (`BackendCapabilities.max_size_bytes`).
    ///   2. Policy global limit (`EffectivePolicy.max_bytes`).
    ///   3. Policy per-mime override (`EffectivePolicy.per_mime_max_bytes`).
    ///
    /// `None` means unbounded from all sources.
    ///
    /// @cpt-cf-file-storage-fr-size-limits-policy
    pub(crate) fn compute_effective_max_bytes(
        policy: &EffectivePolicy,
        mime_type: &str,
        backend: &Arc<dyn StorageBackend>,
    ) -> Option<u64> {
        let backend_max = backend.capabilities().max_size_bytes;
        let policy_global = policy.max_bytes;

        // Find the most specific per-mime override that matches.
        let per_mime_max: Option<u64> = policy
            .per_mime_max_bytes
            .iter()
            .filter(|o| Self::mime_allowed(mime_type, std::slice::from_ref(&o.mime)))
            .map(|o| o.max_bytes)
            .reduce(u64::min);

        // Most restrictive = minimum of all non-None ceilings.
        [backend_max, policy_global, per_mime_max]
            .into_iter()
            .flatten()
            .reduce(u64::min)
    }

    /// Validate `entries` against the metadata limits in `policy`.
    ///
    /// @cpt-cf-file-storage-fr-metadata-limits
    pub(crate) fn check_metadata_limits(
        policy: &EffectivePolicy,
        entries: &[(String, String)],
    ) -> Result<(), DomainError> {
        let limits = &policy.metadata_limits;

        if let Some(max_pairs) = limits.max_pairs
            && entries.len() > max_pairs as usize
        {
            return Err(DomainError::policy_metadata_exceeded(format!(
                "too many metadata pairs: {} > {max_pairs}",
                entries.len()
            )));
        }

        let mut total_bytes: usize = 0;
        for (key, value) in entries {
            if let Some(max_key_len) = limits.max_key_len
                && key.len() > max_key_len as usize
            {
                return Err(DomainError::policy_metadata_exceeded(format!(
                    "metadata key '{key}' length {} exceeds limit of {max_key_len}",
                    key.len()
                )));
            }
            if let Some(max_value_len) = limits.max_value_len
                && value.len() > max_value_len as usize
            {
                return Err(DomainError::policy_metadata_exceeded(format!(
                    "metadata value for key '{key}' length {} exceeds limit of {max_value_len}",
                    value.len()
                )));
            }
            total_bytes += key.len() + value.len();
        }

        if let Some(max_total_bytes) = limits.max_total_bytes
            && total_bytes > max_total_bytes as usize
        {
            return Err(DomainError::policy_metadata_exceeded(format!(
                "total metadata size {total_bytes} bytes exceeds limit of {max_total_bytes} bytes"
            )));
        }

        Ok(())
    }

    /// Run a quota preflight check for `additional_bytes` of new storage.
    ///
    /// Passes `effective_max_bytes.unwrap_or(1)` as the pessimistic upper bound:
    /// if the maximum allowed size would bust the quota, we deny early.
    ///
    /// **Fail-closed**: if the quota client returns an error, the error is
    /// propagated and the request is denied. A failing quota service is safer
    /// than silently allowing unbounded storage growth.
    ///
    /// @cpt-cf-file-storage-fr-storage-quota
    async fn check_quota(
        &self,
        tenant_id: Uuid,
        owner_id: Uuid,
        effective_max_bytes: Option<u64>,
    ) -> Result<(), DomainError> {
        let Some(qc) = &self.quota_client else {
            return Ok(()); // no quota client configured — permissive
        };
        // Use the effective max as the pessimistic size estimate. If no max is
        // configured (unlimited policy), pass 1 as a token check that any new
        // storage at all is permitted.
        let additional_bytes = effective_max_bytes.unwrap_or(1);
        match qc
            .check_storage_quota(tenant_id, owner_id, additional_bytes, QUOTA_METRIC_NAME)
            .await?
        {
            QuotaDecision::Allowed => Ok(()),
            QuotaDecision::Denied { reason } => Err(DomainError::quota_exceeded(reason)),
        }
    }

    // ── usage reporting helpers (P2-M5) ──────────────────────────────────────

    /// Fire-and-forget usage delta report. Failures are logged but never
    /// propagated — a failing usage reporter must not block file operations.
    ///
    /// @cpt-cf-file-storage-fr-usage-reporting
    fn report_usage(&self, delta: UsageDelta) {
        if let Some(reporter) = self.usage_reporter.clone() {
            tokio::spawn(async move {
                reporter.report(delta).await;
            });
        }
    }

    /// Build a [`FileEvent`] for a write operation.
    ///
    /// @cpt-cf-file-storage-fr-file-events
    fn make_file_event(
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

    // ── create + presign ─────────────────────────────────────────────────────

    /// `POST /files`: create a file and presign the first content upload.
    /// An optional `idempotency_key` deduplicates retried requests.
    ///
    /// @cpt-cf-file-storage-fr-upload-idempotency
    /// @cpt-cf-file-storage-fr-audit-trail
    pub async fn create_file(
        &self,
        ctx: &SecurityContext,
        new: NewFile,
        idempotency_key: Option<String>,
    ) -> Result<UploadTicket, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        let owner_id = new.owner_id;
        let owner_kind_str = new.owner_kind.as_str().to_owned();

        // @cpt-cf-file-storage-fr-upload-idempotency
        // Check for an existing idempotency record before doing any work.
        if let Some(ref key) = idempotency_key {
            let now = OffsetDateTime::now_utc();
            if let Some(record) = self
                .store
                .get_idempotency_key(tenant_id, &owner_kind_str, owner_id, key, now)
                .await?
            {
                let ticket: UploadTicket =
                    serde_json::from_str::<IdempotencyTicket>(&record.response_body)
                        .map(Into::into)
                        .map_err(|_| {
                            DomainError::database("failed to deserialize idempotency body")
                        })?;
                return Ok(ticket);
            }
        }

        Self::validate_gts_type(&new.gts_file_type)?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &new.gts_file_type, None)
            .await?;

        // @cpt-cf-file-storage-fr-allowed-types-policy
        // @cpt-cf-file-storage-fr-size-limits-policy
        // @cpt-cf-file-storage-fr-metadata-limits
        // @cpt-cf-file-storage-fr-storage-quota
        let policy = self
            .get_effective_policy_internal(tenant_id, owner_id)
            .await?;

        // Validate allowed mime types.
        Self::check_allowed_mime(&policy, &new.mime_type)?;

        // Compute effective size ceiling and validate initial metadata.
        let backend = self.backends.default_backend();
        let effective_max = Self::compute_effective_max_bytes(&policy, &new.mime_type, &backend);

        // Validate initial custom metadata against limits.
        let initial_meta: Vec<(String, String)> = new
            .custom_metadata
            .iter()
            .map(|e| (e.key.clone(), e.value.clone()))
            .collect();
        Self::check_metadata_limits(&policy, &initial_meta)?;

        // Quota preflight — pessimistic: check whether max allowed size fits quota.
        self.check_quota(tenant_id, owner_id, effective_max).await?;

        let now = OffsetDateTime::now_utc();
        let file_id = Uuid::now_v7();
        let version_id = Uuid::now_v7();
        let backend_id = backend.id().to_owned();
        let backend_path = Self::backend_path(file_id, version_id);

        // @cpt-cf-file-storage-fr-audit-trail
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::Create,
            serde_json::json!({ "version_id": version_id, "gts_file_type": new.gts_file_type }),
        );

        // @cpt-cf-file-storage-fr-file-events
        let event = Some(Self::make_file_event(
            tenant_id,
            owner_id,
            file_id,
            "file.created",
            serde_json::json!({ "version_id": version_id, "gts_file_type": new.gts_file_type }),
        ));

        // Sign the upload URL up front — `sign_url` has no DB dependency, so the
        // ticket (and the idempotency replay body derived from it) can be built
        // before the create transaction and persisted atomically within it.
        let upload_url = self.sign_url(
            Op::Put,
            &VersionRef {
                file_id,
                version_id,
                backend_id: backend_id.clone(),
                backend_path: backend_path.clone(),
            },
            UploadConstraints {
                max_size: effective_max,
                ..UploadConstraints::default()
            },
        )?;
        let ticket = UploadTicket {
            file_id,
            version_id,
            upload_url,
        };

        // @cpt-cf-file-storage-fr-upload-idempotency
        // Build the idempotency row so the create transaction persists it in the
        // same commit as the file — a committed create always leaves a replay
        // record behind, so a retry with the same key never creates a 2nd file.
        let idempotency = idempotency_key.as_ref().map(|key| {
            let response_body = serde_json::to_string(&IdempotencyTicket {
                file_id: ticket.file_id,
                version_id: ticket.version_id,
                upload_url: ticket.upload_url.clone(),
            })
            .unwrap_or_default();
            let expires_at = now
                + time::Duration::seconds(
                    i64::try_from(self.cfg.idempotency_ttl_secs).unwrap_or(86400),
                );
            IdempotencyInsert {
                tenant_id,
                owner_kind: owner_kind_str.clone(),
                owner_id,
                key: key.clone(),
                response_status: 201,
                response_body,
                response_etag: String::new(),
                expires_at,
            }
        });

        self.store
            .create_file_with_pending_version_and_event(
                &new,
                file_id,
                version_id,
                tenant_id,
                &backend_id,
                &backend_path,
                now,
                audit,
                event,
                idempotency,
            )
            .await?;

        // @cpt-cf-file-storage-fr-usage-reporting
        // Fire-and-forget: report +1 file to usage collector.
        self.report_usage(UsageDelta {
            tenant_id,
            owner_id,
            bytes_delta: 0, // bytes unknown at creation; finalize_upload updates the backend
            file_count_delta: 1,
        });

        Ok(ticket)
    }

    /// `POST /files/{id}/versions`: presign a new content version on an existing
    /// file (the upload's bytes will be bound via `bind`).
    pub async fn presign_version(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
    ) -> Result<UploadTicket, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        // Reuse the current version's mime as the declared type placeholder.
        let mime_type = self
            .store
            .current_version_mime(&file)
            .await?
            .unwrap_or_else(|| "application/octet-stream".to_owned());

        // @cpt-cf-file-storage-fr-allowed-types-policy
        // @cpt-cf-file-storage-fr-size-limits-policy
        // @cpt-cf-file-storage-fr-storage-quota
        let tenant_id = ctx.subject_tenant_id();
        let owner_id = file.owner_id;
        let policy = self
            .get_effective_policy_internal(tenant_id, owner_id)
            .await?;

        Self::check_allowed_mime(&policy, &mime_type)?;

        let backend = self.backends.default_backend();
        let effective_max = Self::compute_effective_max_bytes(&policy, &mime_type, &backend);

        self.check_quota(tenant_id, owner_id, effective_max).await?;

        let now = OffsetDateTime::now_utc();
        let version_id = Uuid::now_v7();
        let backend_id = backend.id().to_owned();
        let backend_path = Self::backend_path(file_id, version_id);

        self.store
            .insert_pending_version(
                file_id,
                version_id,
                &mime_type,
                &backend_id,
                &backend_path,
                now,
            )
            .await?;

        let upload_url = self.sign_url(
            Op::Put,
            &VersionRef {
                file_id,
                version_id,
                backend_id,
                backend_path,
            },
            UploadConstraints {
                max_size: effective_max,
                ..UploadConstraints::default()
            },
        )?;
        Ok(UploadTicket {
            file_id,
            version_id,
            upload_url,
        })
    }

    // ── finalize + bind (the optimistic CAS) ──────────────────────────────────

    /// Record an uploaded version's size+hash and mark it available. Called by
    /// the sidecar after streaming bytes to the backend (write action).
    /// Authorize a write to `file_id` (WRITE action) without mutating anything.
    /// The data plane calls this as a preflight **before** writing bytes to a
    /// backend, so a rejected request never persists/overwrites blob content
    /// (the post-write `finalize_upload` re-checks as defense-in-depth).
    pub async fn authorize_write(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
    ) -> Result<(), DomainError> {
        let file = self
            .store
            .require_file(&Self::tenant_scope(ctx), file_id)
            .await?;
        self.authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;
        Ok(())
    }

    /// @cpt-cf-file-storage-fr-audit-trail
    pub async fn finalize_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        hash_value: Vec<u8>,
    ) -> Result<(), DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        // @cpt-cf-file-storage-fr-size-limits-policy
        // Defense-in-depth size check: re-enforce the policy size ceiling at
        // finalization time even though the sidecar already checked the
        // upload constraint in the signed URL.
        let version = self.store.get_version(file_id, version_id).await?;
        let (version_mime, backend_id) = version.as_ref().map_or_else(
            || ("application/octet-stream".to_owned(), String::new()),
            |v| (v.mime_type.clone(), v.backend_id.clone()),
        );
        let policy = self
            .get_effective_policy_internal(ctx.subject_tenant_id(), file.owner_id)
            .await?;
        let backend = if backend_id.is_empty() {
            self.backends.default_backend()
        } else {
            self.backends.get(&backend_id)?
        };
        let effective_max = Self::compute_effective_max_bytes(&policy, &version_mime, &backend);
        if let Some(limit) = effective_max
            && size > 0
            && size.cast_unsigned() > limit
        {
            return Err(DomainError::policy_size_exceeded(
                limit,
                "policy size limit",
            ));
        }

        // @cpt-cf-file-storage-fr-audit-trail
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::FinalizeVersion,
            serde_json::json!({ "version_id": version_id, "size": size }),
        );

        let ok = self
            .store
            .finalize_version(file_id, version_id, size, hash_value, audit)
            .await?;
        if !ok {
            return Err(DomainError::version_not_found(file_id, version_id));
        }
        Ok(())
    }

    /// `POST /files/{id}/bind`: swap the content pointer to `version_id` under
    /// optimistic CAS guarded by the `If-Match` content ETag. Returns the
    /// updated file; `412` on conflict (re-read the ETag and rebind).
    ///
    /// `if_match` is the opaque content ETag (or `*`, or `None` for the first
    /// bind). The server recomputes the current ETag and compares — it never
    /// reverses the ETag back to a `content_id`.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    pub async fn bind(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Uuid,
        if_match: Option<&str>,
    ) -> Result<File, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        // The version must exist and be available.
        let version = self
            .store
            .get_version(file_id, version_id)
            .await?
            .ok_or_else(|| DomainError::version_not_found(file_id, version_id))?;
        if version.status != file_storage_sdk::VersionStatus::Available {
            return Err(DomainError::conflict(
                "cannot bind a version whose upload has not been finalized",
            ));
        }

        // Validate the If-Match precondition against the current content ETag.
        let expected_content_id = file.content_id;
        let current_etag = expected_content_id.map(|c| etag::content_etag(file_id, c));
        match if_match {
            // The first bind (no content yet) may omit If-Match; rebinding
            // already-bound content MUST carry it, otherwise the advertised
            // conditional update degrades into an unconditional overwrite.
            None => {
                if expected_content_id.is_some() {
                    return Err(DomainError::precondition_failed(
                        "If-Match is required to rebind already-bound content",
                    ));
                }
            }
            Some(m) => {
                let m = m.trim();
                if m != "*" && Some(m) != current_etag.as_deref() {
                    return Err(DomainError::precondition_failed(
                        "If-Match does not match the current content ETag",
                    ));
                }
            }
        }

        // @cpt-cf-file-storage-fr-audit-trail
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::PatchContent,
            serde_json::json!({ "version_id": version_id }),
        );

        // @cpt-cf-file-storage-fr-file-events
        let event = Some(Self::make_file_event(
            file.tenant_id,
            file.owner_id,
            file_id,
            "file.content_updated",
            serde_json::json!({ "version_id": version_id }),
        ));

        // Swap the content pointer (CAS) and flip `is_current` in a SINGLE
        // transaction so `files.content_id` and `file_versions.is_current` can
        // never diverge if a later write fails (DESIGN §3.7 bind invariant).
        let now = OffsetDateTime::now_utc();
        let swapped = self
            .store
            .bind_atomic_with_event(
                &scope,
                file_id,
                expected_content_id,
                version_id,
                now,
                audit,
                event,
            )
            .await?;
        if !swapped {
            return Err(DomainError::precondition_failed(
                "content pointer changed concurrently; re-read the ETag and rebind",
            ));
        }

        self.store.require_file(&scope, file_id).await
    }

    // ── reads ─────────────────────────────────────────────────────────────────

    /// Get a file's metadata.
    pub async fn get_file(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
    ) -> Result<File, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let scope = self
            .authorizer
            .authorize(ctx, actions::READ, &file.gts_file_type, Some(file_id))
            .await?;
        self.store.require_file(&scope, file_id).await
    }

    /// Get a file plus its custom metadata.
    pub async fn get_file_with_metadata(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
    ) -> Result<(File, Vec<CustomMetadataEntry>), DomainError> {
        let file = self.get_file(ctx, file_id).await?;
        let meta = self.store.list_metadata(file_id).await?;
        Ok((file, meta))
    }

    /// List files for a mandatory owner filter, offset-paginated.
    pub async fn list_files(
        &self,
        ctx: &SecurityContext,
        owner: OwnerFilter,
        limit: Option<u64>,
        offset: u64,
    ) -> Result<Vec<File>, DomainError> {
        // Authorize (access gate), then always tenant-scope the query so the
        // tenant boundary holds regardless of the PDP's returned constraints.
        self.authorizer
            .authorize(ctx, actions::READ, "", None)
            .await?;
        let limit = limit
            .unwrap_or(self.cfg.default_page_size)
            .min(self.cfg.max_page_size);
        self.store
            .list_files(&Self::tenant_scope(ctx), owner, limit, offset)
            .await
    }

    // ── metadata update ────────────────────────────────────────────────────────

    /// `PATCH /files/{id}`: JSON-merge-patch the custom metadata and bump
    /// `meta_version`, optionally guarded by `If-Match-Metadata`.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    pub async fn update_metadata(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        patch: CustomMetadataPatch,
        expected_meta_version: Option<i64>,
    ) -> Result<File, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        // @cpt-cf-file-storage-fr-metadata-limits
        // Compute what the resulting metadata will look like after this patch,
        // then validate against the effective policy.
        let policy = self
            .get_effective_policy_internal(ctx.subject_tenant_id(), file.owner_id)
            .await?;
        let existing = self.store.list_metadata(file_id).await?;
        // Build a map from existing entries and apply the patch (merge semantics).
        let mut merged: HashMap<String, String> =
            existing.into_iter().map(|e| (e.key, e.value)).collect();
        for (key, value) in &patch.entries {
            match value {
                Some(v) => {
                    merged.insert(key.clone(), v.clone());
                }
                None => {
                    merged.remove(key);
                }
            }
        }
        let result_pairs: Vec<(String, String)> = merged.into_iter().collect();
        Self::check_metadata_limits(&policy, &result_pairs)?;

        // @cpt-cf-file-storage-fr-audit-trail
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::PatchMetadata,
            serde_json::json!({ "expected_meta_version": expected_meta_version }),
        );

        // Apply the meta-version CAS and the patch in ONE transaction. The CAS
        // runs first, so a stale `expected_meta_version` aborts before any row
        // is touched and the rollback guarantees no partial metadata change is
        // committed (the optimistic-concurrency guard cannot be bypassed). The
        // per-key delete-then-insert upsert is also covered by the rollback, so
        // a failed insert can never leave a key permanently removed.
        let now = OffsetDateTime::now_utc();
        let bumped = self
            .store
            .patch_metadata_atomic(&scope, file_id, expected_meta_version, patch, now, audit)
            .await?;
        if !bumped {
            return Err(DomainError::precondition_failed(
                "metadata revision changed concurrently (If-Match-Metadata)",
            ));
        }
        self.store.require_file(&scope, file_id).await
    }

    // ── delete ──────────────────────────────────────────────────────────────────

    /// `DELETE /files/{id}`: remove the file and all versions (FK cascade) under
    /// an `If-Match` content-ETag precondition, then best-effort delete the
    /// backend blobs. `If-Match` is **required** (see api.md §DELETE); pass `"*"`
    /// to delete unconditionally when the ETag is unknown.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    pub async fn delete_file(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        if_match: Option<&str>,
    ) -> Result<(), DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::DELETE, &file.gts_file_type, Some(file_id))
            .await?;

        // Validate the If-Match precondition against the current content ETag.
        let current_etag = etag::etag_for(&file);
        match if_match {
            None => {
                return Err(DomainError::precondition_failed(
                    "If-Match is required to delete a file",
                ));
            }
            Some(m) => {
                let m = m.trim();
                if m != "*" && Some(m) != current_etag.as_deref() {
                    return Err(DomainError::precondition_failed(
                        "If-Match does not match the current content ETag",
                    ));
                }
            }
        }

        self.delete_file_inner(ctx, file_id).await
    }

    /// Inner (unconditional) file deletion: authorization and If-Match must have
    /// already been checked by the caller. Collects versions, removes the DB row
    /// (and FK children via cascade), then best-effort-deletes all backend blobs.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    async fn delete_file_inner(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
    ) -> Result<(), DomainError> {
        // Authorization has already been verified by callers; use allow_all() for
        // the DB scope — the tenant boundary was enforced by require_file() above.
        let scope = AccessScope::allow_all();

        // Collect backend blobs before the metadata row (and FK children) vanish.
        let versions = self.store.list_versions(file_id).await?;

        // @cpt-cf-file-storage-fr-audit-trail
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::DeleteFile,
            serde_json::json!({ "version_count": versions.len() }),
        );

        // @cpt-cf-file-storage-fr-file-events
        // We need the file's tenant/owner for the event payload; fetch before deletion.
        let file_meta = self.store.get_file(&scope, file_id).await?;
        let (event_tenant, event_owner) = file_meta.as_ref().map_or_else(
            || (ctx.subject_tenant_id(), Uuid::nil()),
            |f| (f.tenant_id, f.owner_id),
        );
        let event = Some(Self::make_file_event(
            event_tenant,
            event_owner,
            file_id,
            "file.deleted",
            serde_json::json!({ "version_count": versions.len() }),
        ));

        let removed = self
            .store
            .delete_file_with_event(&scope, file_id, audit, event)
            .await?;
        if !removed {
            return Err(DomainError::file_not_found(file_id));
        }

        // @cpt-cf-file-storage-fr-usage-reporting
        let total_bytes: i64 = versions.iter().map(|v| v.size).sum();
        self.report_usage(UsageDelta {
            tenant_id: event_tenant,
            owner_id: event_owner,
            bytes_delta: -total_bytes,
            file_count_delta: -1,
        });

        // Best-effort backend cleanup; a failure degrades to an orphan (P2 GC).
        for v in versions {
            self.best_effort_blob_delete(&v.backend_id, &v.backend_path)
                .await;
        }
        Ok(())
    }

    // ── download + versioning ─────────────────────────────────────────────────

    /// `GET /files/{id}/download-url`: issue a signed download URL pinned to the
    /// current content (or a specific `version_id`).
    pub async fn download_url(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Option<Uuid>,
    ) -> Result<DownloadTicket, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::READ, &file.gts_file_type, Some(file_id))
            .await?;

        let target = match version_id {
            Some(v) => v,
            None => file
                .content_id
                .ok_or_else(|| DomainError::conflict("file has no bound content yet"))?,
        };
        let version = self
            .store
            .get_version(file_id, target)
            .await?
            .ok_or_else(|| DomainError::version_not_found(file_id, target))?;

        if version.status != file_storage_sdk::VersionStatus::Available {
            return Err(DomainError::conflict(
                "cannot issue a download URL for a version whose upload has not been finalized",
            ));
        }

        let download_url = self.sign_url(
            Op::Get,
            &VersionRef {
                file_id,
                version_id: target,
                backend_id: version.backend_id,
                backend_path: version.backend_path,
            },
            UploadConstraints::default(),
        )?;
        Ok(DownloadTicket {
            download_url,
            etag: etag::content_etag(file_id, target),
            version_id: target,
        })
    }

    /// List all versions of a file.
    pub async fn list_versions(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
    ) -> Result<Vec<FileVersion>, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::READ, &file.gts_file_type, Some(file_id))
            .await?;
        self.store.list_versions(file_id).await
    }

    /// Restore a prior version as current (a rebind: pointer swap, no re-upload).
    pub async fn restore_version(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<File, DomainError> {
        let file = self.get_file(ctx, file_id).await?;
        let if_match = etag::etag_for(&file);
        self.bind(ctx, file_id, version_id, if_match.as_deref())
            .await
    }

    /// Delete a single version (and its backend blob). Deleting the only version
    /// is equivalent to deleting the file.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    pub async fn delete_version(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<(), DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::DELETE, &file.gts_file_type, Some(file_id))
            .await?;

        let all = self.store.list_versions(file_id).await?;
        if all.len() <= 1 {
            // Last version → delete the whole file. Authorization has already been
            // checked above; skip the If-Match gate (delete_version has its own
            // contract — no If-Match on DELETE /files/{id}/versions/{vid}).
            return self.delete_file_inner(ctx, file_id).await;
        }
        let Some(version) = all.into_iter().find(|v| v.version_id == version_id) else {
            return Err(DomainError::version_not_found(file_id, version_id));
        };
        if file.content_id == Some(version_id) {
            return Err(DomainError::conflict(
                "cannot delete the current version; bind another version first",
            ));
        }

        // @cpt-cf-file-storage-fr-audit-trail
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::DeleteVersion,
            serde_json::json!({ "version_id": version_id }),
        );

        self.store
            .delete_version(file_id, version_id, audit)
            .await?;
        self.best_effort_blob_delete(&version.backend_id, &version.backend_path)
            .await;
        Ok(())
    }

    // ── ownership transfer (P2-M5) ────────────────────────────────────────────

    /// `POST /files/{id}/transfer`: transfer ownership of a file to a new owner.
    ///
    /// The new owner's `owner_kind` and `owner_id` replace the current values.
    /// An audit row (`TransferOwnership`) and a file event (`file.owner_transferred`)
    /// are enqueued in the same transaction as the update.
    ///
    /// @cpt-cf-file-storage-fr-ownership-transfer
    /// @cpt-cf-file-storage-fr-usage-reporting
    /// @cpt-cf-file-storage-fr-file-events
    /// @cpt-cf-file-storage-fr-audit-trail
    pub async fn transfer_ownership(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        new_owner_kind: file_storage_sdk::OwnerKind,
        new_owner_id: Uuid,
    ) -> Result<File, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        let now = OffsetDateTime::now_utc();
        let tenant_id = file.tenant_id;
        let old_owner_id = file.owner_id;
        let new_owner_kind_str = new_owner_kind.as_str().to_owned();

        // @cpt-cf-file-storage-fr-audit-trail
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::TransferOwnership,
            serde_json::json!({
                "from_owner_kind": file.owner_kind.as_str(),
                "from_owner_id": old_owner_id,
                "to_owner_kind": new_owner_kind_str,
                "to_owner_id": new_owner_id,
            }),
        );

        // @cpt-cf-file-storage-fr-file-events
        let event = Some(Self::make_file_event(
            tenant_id,
            new_owner_id,
            file_id,
            "file.owner_transferred",
            serde_json::json!({
                "from_owner_kind": file.owner_kind.as_str(),
                "from_owner_id": old_owner_id,
                "to_owner_kind": new_owner_kind_str,
                "to_owner_id": new_owner_id,
            }),
        ));

        let updated = self
            .store
            .transfer_ownership_atomic(
                &scope,
                file_id,
                &new_owner_kind_str,
                new_owner_id,
                now,
                audit,
                event,
            )
            .await?;

        if !updated {
            return Err(DomainError::file_not_found(file_id));
        }

        // @cpt-cf-file-storage-fr-usage-reporting
        // Debit old owner, credit new owner. Bytes are unchanged.
        let total_bytes: i64 = self
            .store
            .list_versions(file_id)
            .await?
            .iter()
            .filter(|v| v.status == file_storage_sdk::VersionStatus::Available)
            .map(|v| v.size)
            .sum();
        self.report_usage(UsageDelta {
            tenant_id,
            owner_id: old_owner_id,
            bytes_delta: -total_bytes,
            file_count_delta: -1,
        });
        self.report_usage(UsageDelta {
            tenant_id,
            owner_id: new_owner_id,
            bytes_delta: total_bytes,
            file_count_delta: 1,
        });

        self.store.require_file(&scope, file_id).await
    }

    // ── policy management (P2-M1) ─────────────────────────────────────────────

    /// Get the raw (own-level) policy body for a scope, if one has been set.
    ///
    /// @cpt-cf-file-storage-usecase-configure-policy
    pub async fn get_own_policy(
        &self,
        ctx: &SecurityContext,
        policy_scope: PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<Option<StoredPolicy>, DomainError> {
        let scope = self
            .authorizer
            .authorize(ctx, actions::READ, "", None)
            .await?;
        self.store
            .get_policy(
                &scope,
                ctx.subject_tenant_id(),
                &policy_scope,
                scope_owner_id,
            )
            .await
    }

    /// Set (upsert) the policy for a scope. Tenant-level policy requires the
    /// caller to have appropriate authorization; user-level is self-service.
    ///
    /// @cpt-cf-file-storage-usecase-configure-policy
    pub async fn set_policy(
        &self,
        ctx: &SecurityContext,
        policy_scope: PolicyScope,
        scope_owner_id: Option<Uuid>,
        body: PolicyBody,
    ) -> Result<StoredPolicy, DomainError> {
        let scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, "", None)
            .await?;
        let now = OffsetDateTime::now_utc();
        let tenant_id = ctx.subject_tenant_id();
        let policy_id = self
            .store
            .upsert_policy(&scope, tenant_id, &policy_scope, scope_owner_id, &body, now)
            .await?;
        Ok(StoredPolicy {
            policy_id,
            tenant_id,
            scope: policy_scope,
            scope_owner_id,
            body,
            // The upsert wrote both timestamps to `now`.
            created_at: now,
            updated_at: now,
        })
    }

    /// Compute the effective policy for the current caller context, combining
    /// the tenant-level and user-level policies with most-restrictive-wins.
    ///
    /// @cpt-cf-file-storage-usecase-configure-policy
    /// @cpt-cf-file-storage-fr-allowed-types-policy
    /// @cpt-cf-file-storage-fr-size-limits-policy
    /// @cpt-cf-file-storage-fr-metadata-limits
    pub async fn get_effective_policy(
        &self,
        ctx: &SecurityContext,
        user_owner_id: Option<Uuid>,
    ) -> Result<EffectivePolicy, DomainError> {
        let scope = self
            .authorizer
            .authorize(ctx, actions::READ, "", None)
            .await?;
        let tenant_id = ctx.subject_tenant_id();

        let tenant_policy = self
            .store
            .get_policy(&scope, tenant_id, &PolicyScope::Tenant, None)
            .await?;
        let user_policy = match user_owner_id {
            Some(uid) => {
                self.store
                    .get_policy(&scope, tenant_id, &PolicyScope::User, Some(uid))
                    .await?
            }
            None => None,
        };

        Ok(PolicyResolver::resolve(
            tenant_policy.as_ref().map(|p| &p.body),
            user_policy.as_ref().map(|p| &p.body),
        ))
    }

    /// List retention rules for the caller's tenant.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    pub async fn list_retention_rules(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<StoredRetentionRule>, DomainError> {
        let scope = self
            .authorizer
            .authorize(ctx, actions::READ, "", None)
            .await?;
        self.store
            .list_retention_rules(&scope, ctx.subject_tenant_id())
            .await
    }

    /// Create a new retention rule.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    pub async fn create_retention_rule(
        &self,
        ctx: &SecurityContext,
        retention_scope: RetentionScope,
        scope_target_id: Option<Uuid>,
        body: RetentionRuleBody,
    ) -> Result<StoredRetentionRule, DomainError> {
        let scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, "", None)
            .await?;
        let now = OffsetDateTime::now_utc();
        let tenant_id = ctx.subject_tenant_id();
        let rule_id = self
            .store
            .insert_retention_rule(
                &scope,
                tenant_id,
                &retention_scope,
                scope_target_id,
                &body,
                now,
            )
            .await?;
        Ok(StoredRetentionRule {
            rule_id,
            tenant_id,
            scope: retention_scope,
            scope_target_id,
            body,
            created_at: now,
        })
    }

    /// Delete a retention rule by `rule_id`.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    pub async fn delete_retention_rule(
        &self,
        ctx: &SecurityContext,
        rule_id: Uuid,
    ) -> Result<bool, DomainError> {
        let scope = self
            .authorizer
            .authorize(ctx, actions::DELETE, "", None)
            .await?;
        self.store.delete_retention_rule(&scope, rule_id).await
    }

    // ── multipart upload (P2-M3) ──────────────────────────────────────────────

    /// `POST /files/{id}/multipart`: initiate a multipart upload session.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    pub async fn initiate_multipart_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        declared_mime: &str,
    ) -> Result<MultipartUploadSession, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        let backend = self.backends.default_backend();
        if !backend.capabilities().multipart_native {
            return Err(DomainError::multipart_not_supported(backend.id()));
        }

        // Policy checks: allowed mime type and size.
        let tenant_id = ctx.subject_tenant_id();
        let policy = self
            .get_effective_policy_internal(tenant_id, file.owner_id)
            .await?;
        Self::check_allowed_mime(&policy, declared_mime)?;
        let effective_max = Self::compute_effective_max_bytes(&policy, declared_mime, &backend);
        self.check_quota(tenant_id, file.owner_id, effective_max)
            .await?;

        let now = OffsetDateTime::now_utc();
        let upload_id = Uuid::now_v7();
        let version_id = Uuid::now_v7();
        let backend_path = Self::backend_path(file_id, version_id);
        let backend_id = backend.id().to_owned();

        // Pre-register the pending file_versions row.
        self.store
            .insert_pending_version(
                file_id,
                version_id,
                declared_mime,
                &backend_id,
                &backend_path,
                now,
            )
            .await?;

        // Initiate the multipart upload on the backend.
        let backend_handle = backend.initiate_multipart(&backend_path).await?;

        // Default TTL for multipart sessions: 7 days.
        let expires_at = now + time::Duration::days(7);

        self.store
            .create_multipart_upload(
                upload_id,
                file_id,
                version_id,
                &backend_handle,
                declared_mime,
                expires_at,
                now,
            )
            .await?;

        Ok(MultipartUploadSession {
            upload_id,
            file_id,
            version_id,
            backend_upload_handle: backend_handle,
            state: MultipartUploadState::InProgress,
            declared_mime: declared_mime.to_owned(),
            mime_validated: false,
            created_at: now,
            expires_at,
        })
    }

    /// `PUT /files/{id}/multipart/{upload_id}/parts/{part_number}`: upload one part.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    pub async fn upload_multipart_part(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        upload_id: Uuid,
        part_number: u32,
        data: Bytes,
    ) -> Result<MultipartPart, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        let session = self
            .store
            .get_multipart_upload(upload_id)
            .await?
            .ok_or_else(|| DomainError::multipart_upload_not_found(upload_id))?;

        // Bind the session to the authorized path `file_id`. Authorization above
        // checks the path file, but the session is loaded by `upload_id` alone —
        // without this a caller could drive another file's upload (and corrupt
        // state via a recomputed backend path). Reported as "not found" so a
        // foreign `upload_id` is not distinguishable from a missing one.
        if session.file_id != file_id {
            return Err(DomainError::multipart_upload_not_found(upload_id));
        }

        if session.state != MultipartUploadState::InProgress {
            return Err(DomainError::multipart_upload_not_in_progress(
                upload_id,
                session.state.as_str(),
            ));
        }

        // Fetch the backend from the version row.
        let version = self.store.get_version(file_id, session.version_id).await?;
        let backend_id = version.as_ref().map_or_else(
            || self.backends.default_id().to_owned(),
            |v| v.backend_id.clone(),
        );
        let backend = self.backends.get(&backend_id)?;
        let backend_path = Self::backend_path(file_id, session.version_id);

        let data_size = i64::try_from(data.len())
            .map_err(|_| DomainError::validation("data", "part data too large"))?;
        let (backend_etag, part_hash) = backend
            .upload_part(
                &backend_path,
                &session.backend_upload_handle,
                part_number,
                data,
            )
            .await?;

        let now = OffsetDateTime::now_utc();
        let part_number_i32 = i32::try_from(part_number)
            .map_err(|_| DomainError::validation("part_number", "part number too large"))?;

        self.store
            .upsert_multipart_part(
                upload_id,
                part_number_i32,
                &backend_etag,
                part_hash.clone(),
                data_size,
                now,
            )
            .await?;

        Ok(MultipartPart {
            upload_id,
            part_number,
            backend_etag,
            part_hash,
            size: data_size,
            uploaded_at: now,
        })
    }

    /// `POST /files/{id}/multipart/{upload_id}/complete`: finalize all parts.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    /// @cpt-cf-file-storage-fr-audit-trail
    pub async fn complete_multipart_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        upload_id: Uuid,
    ) -> Result<(), DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        let session = self
            .store
            .get_multipart_upload(upload_id)
            .await?
            .ok_or_else(|| DomainError::multipart_upload_not_found(upload_id))?;

        // Bind the session to the authorized path `file_id`. Authorization above
        // checks the path file, but the session is loaded by `upload_id` alone —
        // without this a caller could drive another file's upload (and corrupt
        // state via a recomputed backend path). Reported as "not found" so a
        // foreign `upload_id` is not distinguishable from a missing one.
        if session.file_id != file_id {
            return Err(DomainError::multipart_upload_not_found(upload_id));
        }

        if session.state != MultipartUploadState::InProgress {
            return Err(DomainError::multipart_upload_not_in_progress(
                upload_id,
                session.state.as_str(),
            ));
        }

        let parts = self.store.list_multipart_parts(upload_id).await?;

        // Fetch the backend from the version row.
        let version = self.store.get_version(file_id, session.version_id).await?;
        let backend_id = version.as_ref().map_or_else(
            || self.backends.default_id().to_owned(),
            |v| v.backend_id.clone(),
        );
        let backend = self.backends.get(&backend_id)?;
        let backend_path = Self::backend_path(file_id, session.version_id);

        // Compute total size.
        let total_size: i64 = parts.iter().map(|p| p.size).sum();

        // Policy size check.
        let policy = self
            .get_effective_policy_internal(ctx.subject_tenant_id(), file.owner_id)
            .await?;
        let effective_max =
            Self::compute_effective_max_bytes(&policy, &session.declared_mime, &backend);
        if let Some(limit) = effective_max
            && total_size > 0
            && total_size.cast_unsigned() > limit
        {
            return Err(DomainError::policy_size_exceeded(
                limit,
                "policy size limit",
            ));
        }

        // Build the parts list for the backend.
        let backend_parts: Vec<(u32, String)> = parts
            .iter()
            .map(|p| (p.part_number, p.backend_etag.clone()))
            .collect();

        // Assemble on the backend, which returns the SHA-256 of the fully
        // assembled object. This is the hash of the bytes actually stored — a
        // hash over concatenated part digests would not match a later `get` +
        // recompute and would break `migrate_backend`'s integrity check.
        let content_hash = backend
            .complete_multipart(
                &backend_path,
                &session.backend_upload_handle,
                &backend_parts,
            )
            .await?;

        // Finalize the version row (no separate audit row — complete below covers it).
        let finalize_audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::FinalizeVersion,
            serde_json::json!({ "version_id": session.version_id, "upload_id": upload_id, "size": total_size }),
        );
        let finalized = self
            .store
            .finalize_version(
                file_id,
                session.version_id,
                total_size,
                content_hash,
                finalize_audit,
            )
            .await?;
        if !finalized {
            // The pending version row disappeared (concurrent abort or cleanup)
            // after the backend assembled the object. Fail loudly instead of
            // reporting success with no bound version; the now-orphaned blob at
            // `backend_path` is reclaimed by the orphan-reconciliation sweep.
            return Err(DomainError::conflict(format!(
                "multipart upload {upload_id}: version row was removed before completion"
            )));
        }

        // Mark the session completed and emit the main audit row.
        // @cpt-cf-file-storage-fr-audit-trail
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::MultipartComplete,
            serde_json::json!({ "upload_id": upload_id, "version_id": session.version_id }),
        );
        self.store
            .complete_multipart_upload(upload_id, audit)
            .await?;

        Ok(())
    }

    /// `DELETE /files/{id}/multipart/{upload_id}`: abort a multipart upload.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    /// @cpt-cf-file-storage-fr-audit-trail
    pub async fn abort_multipart_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        upload_id: Uuid,
    ) -> Result<(), DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        let session = self
            .store
            .get_multipart_upload(upload_id)
            .await?
            .ok_or_else(|| DomainError::multipart_upload_not_found(upload_id))?;

        // Bind the session to the authorized path `file_id`. Authorization above
        // checks the path file, but the session is loaded by `upload_id` alone —
        // without this a caller could drive another file's upload (and corrupt
        // state via a recomputed backend path). Reported as "not found" so a
        // foreign `upload_id` is not distinguishable from a missing one.
        if session.file_id != file_id {
            return Err(DomainError::multipart_upload_not_found(upload_id));
        }

        if session.state != MultipartUploadState::InProgress {
            return Err(DomainError::multipart_upload_not_in_progress(
                upload_id,
                session.state.as_str(),
            ));
        }

        // Fetch the backend from the version row.
        let version = self.store.get_version(file_id, session.version_id).await?;
        let backend_id = version.as_ref().map_or_else(
            || self.backends.default_id().to_owned(),
            |v| v.backend_id.clone(),
        );
        let backend = self.backends.get(&backend_id)?;
        let backend_path = Self::backend_path(file_id, session.version_id);

        backend
            .abort_multipart(&backend_path, &session.backend_upload_handle)
            .await?;

        // @cpt-cf-file-storage-fr-audit-trail
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::MultipartAbort,
            serde_json::json!({ "upload_id": upload_id, "version_id": session.version_id }),
        );

        // Mark the session aborted.
        self.store.abort_multipart_upload(upload_id, audit).await?;

        // Delete the pending version row (no audit row — a pending version is
        // an implementation detail, not a distinct audited file version). A
        // DB error must not be swallowed; a missing row (`false`) is acceptable
        // for an abort, since the pending version being already gone is the
        // desired end state.
        self.store
            .delete_version(
                file_id,
                session.version_id,
                // Deleted as part of abort — record as delete_version for completeness.
                Self::audit_ok(
                    ctx,
                    Some(file_id),
                    AuditOperation::DeleteVersion,
                    serde_json::json!({ "version_id": session.version_id, "reason": "multipart_abort" }),
                ),
            )
            .await?;

        Ok(())
    }

    // ── backend migration (P2-M4) ─────────────────────────────────────────────

    /// Relocate a non-versioned file's content from one backend to another
    /// without changing its identity (`file_id`, ownership, metadata, content
    /// hash).
    ///
    /// Steps:
    /// 1. Verify the file has exactly 1 version (non-versioned files only).
    /// 2. Read the blob from the source backend.
    /// 3. Write the blob to the destination backend at the canonical path.
    /// 4. Verify the content hash matches the stored version hash (SHA-256).
    /// 5. Transactionally update `backend_id` + `backend_path` and emit a
    ///    `BackendMigrate` audit row.
    /// 6. Best-effort delete the source blob (orphan cleanup if this fails).
    ///
    /// Returns `Ok(())` when the file already lives on the target backend
    /// (no-op), or after the migration completes successfully.
    ///
    /// @cpt-cf-file-storage-fr-backend-migration
    pub async fn migrate_backend(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        target_backend_id: &str,
    ) -> Result<(), DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        // Only non-versioned files (exactly 1 version) may be migrated.
        let versions = self.store.list_versions(file_id).await?;
        if versions.len() != 1 {
            return Err(DomainError::versioned_file_migration_not_supported(file_id));
        }

        let version = &versions[0];

        // The version must be in the `available` state.
        if version.status != file_storage_sdk::VersionStatus::Available {
            return Err(DomainError::conflict(
                "cannot migrate a version whose upload has not been finalized",
            ));
        }

        // No-op if already on the target backend.
        if version.backend_id == target_backend_id {
            return Ok(());
        }

        let source = self.backends.get(&version.backend_id)?;
        let dest = self.backends.get(target_backend_id)?;

        // Read the blob from the source backend.
        let bytes = source.get(&version.backend_path).await?;

        // Verify content hash before writing to destination.
        let computed_hash: Vec<u8> = hash::sha256(&bytes);
        if computed_hash != version.hash_value {
            return Err(DomainError::hash_mismatch(
                hex::encode(&version.hash_value),
                hex::encode(&computed_hash),
            ));
        }

        // Write to the destination at the canonical path.
        let dest_path = Self::backend_path(file_id, version.version_id);
        dest.put(&dest_path, bytes).await?;

        // Transactionally update the version row and emit the audit row.
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::BackendMigrate,
            serde_json::json!({
                "from_backend": version.backend_id,
                "to_backend": target_backend_id,
                "version_id": version.version_id,
            }),
        );
        let updated = self
            .store
            .rebind_version_backend(
                file_id,
                version.version_id,
                target_backend_id,
                &dest_path,
                audit,
            )
            .await?;
        if !updated {
            // Concurrent operation removed the version before we could rebind —
            // the blob we just wrote to the destination is now an orphan; clean
            // it up best-effort and return not-found.
            self.best_effort_blob_delete(dest.id(), &dest_path).await;
            return Err(DomainError::version_not_found(file_id, version.version_id));
        }

        // Best-effort delete the source blob.
        self.best_effort_blob_delete(source.id(), &version.backend_path)
            .await;

        Ok(())
    }

    // ── backends discovery ────────────────────────────────────────────────────

    /// `GET /storages`: configured backends and their capabilities.
    #[must_use]
    pub fn list_backends(&self) -> Vec<(String, BackendCapabilities)> {
        self.backends.list()
    }

    /// `GET /storages/{id}`.
    pub fn get_backend(&self, id: &str) -> Result<(String, BackendCapabilities), DomainError> {
        let b = self.backends.get(id)?;
        Ok((b.id().to_owned(), b.capabilities()))
    }

    /// Delete a backend blob, logging (not failing) on error. A failed delete
    /// degrades to an orphan reconciled by the P2 cleanup engine.
    async fn best_effort_blob_delete(&self, backend_id: &str, path: &str) {
        let Ok(backend) = self.backends.get(backend_id) else {
            return;
        };
        if let Err(err) = backend.delete(path).await {
            tracing::warn!(?err, path, "best-effort backend delete failed");
        }
    }

    // ── pub(crate) accessors for DataPlaneService ─────────────────────────────

    /// Backend registry (shared with the data plane).
    pub(crate) fn backends(&self) -> &BackendRegistry {
        &self.backends
    }

    /// Store (shared with the data plane).
    pub(crate) fn store(&self) -> &Store {
        &self.store
    }
}

/// A minimal reference to a version's backend location, for URL signing.
#[allow(unknown_lints, de0309_must_have_domain_model)]
struct VersionRef {
    file_id: Uuid,
    version_id: Uuid,
    backend_id: String,
    backend_path: String,
}

/// Serializable form of `UploadTicket` stored in the idempotency record.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(serde::Serialize, serde::Deserialize)]
struct IdempotencyTicket {
    file_id: Uuid,
    version_id: Uuid,
    upload_url: String,
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
#[path = "service_tests.rs"]
mod service_tests;
