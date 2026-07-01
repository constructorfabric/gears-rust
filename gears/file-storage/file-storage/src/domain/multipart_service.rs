//! `MultipartService` — multipart upload control-plane logic.
//!
//! Owns the P2-M3 flows: initiate, upload-part, complete, and abort.
//! Holds its own copies of the shared dependencies (`Store`, `BackendRegistry`,
//! `Authorizer`, `QuotaClient`) so it does NOT reference `FileService` — that
//! keeps the fan-in graph clean and avoids raising the HK score of `FileService`.

// Domain terms (ETag, If-Match, FileStorage, GET/PUT) recur throughout the docs.
#![allow(clippy::doc_markdown)]

use std::sync::Arc;

use bytes::Bytes;
use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::audit::{AuditEntry, AuditOperation};
use crate::domain::authz::{Authorizer, actions};
use crate::domain::error::DomainError;
use crate::domain::multipart::{MultipartPart, MultipartUploadSession, MultipartUploadState};
use crate::domain::policy::{PolicyResolver, PolicyScope};
use crate::domain::ports::MultipartStore;
use crate::infra::backend::BackendRegistry;
use crate::infra::quota::{QuotaClient, QuotaDecision};

/// Quota metric name (duplicated from service.rs; both refer to the same
/// platform metric — no abstraction needed here).
const QUOTA_METRIC_NAME: &str = "gts.cf.qe.metric.type.v1~cf.qe.metric.file_storage_bytes.v1";

/// The multipart-upload service (P2-M3).
///
/// Extracted from `FileService` to reduce its Henry-Kafura coupling score.
/// All four multipart operations live here; the struct is wired alongside
/// `FileService` in `gear.rs` and served under the same REST prefix.
#[allow(unknown_lints, de0309_must_have_domain_model)]
pub struct MultipartService {
    store: Arc<dyn MultipartStore>,
    backends: BackendRegistry,
    authorizer: Arc<dyn Authorizer>,
    quota_client: Option<Arc<dyn QuotaClient>>,
}

impl MultipartService {
    pub fn new(
        store: Arc<dyn MultipartStore>,
        backends: BackendRegistry,
        authorizer: Arc<dyn Authorizer>,
        quota_client: Option<Arc<dyn QuotaClient>>,
    ) -> Self {
        Self {
            store,
            backends,
            authorizer,
            quota_client,
        }
    }

    // ── private helpers ──────────────────────────────────────────────────────

    fn tenant_scope(ctx: &SecurityContext) -> AccessScope {
        AccessScope::for_tenant(ctx.subject_tenant_id())
    }

    fn backend_path(file_id: Uuid, version_id: Uuid) -> String {
        format!("/{file_id}/{version_id}")
    }

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

    /// Resolve the effective policy for a given `(tenant_id, owner_id)` pair.
    ///
    /// @cpt-cf-file-storage-fr-allowed-types-policy
    /// @cpt-cf-file-storage-fr-size-limits-policy
    async fn get_effective_policy_internal(
        &self,
        tenant_id: Uuid,
        owner_id: Uuid,
    ) -> Result<crate::domain::policy::EffectivePolicy, DomainError> {
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

    /// Run a quota preflight check for `additional_bytes` of new storage.
    ///
    /// **Fail-closed**: a failing quota client denies the request.
    ///
    /// @cpt-cf-file-storage-fr-storage-quota
    async fn check_quota(
        &self,
        tenant_id: Uuid,
        owner_id: Uuid,
        effective_max_bytes: Option<u64>,
    ) -> Result<(), DomainError> {
        let Some(qc) = &self.quota_client else {
            return Ok(());
        };
        let additional_bytes = effective_max_bytes.unwrap_or(1);
        match qc
            .check_storage_quota(tenant_id, owner_id, additional_bytes, QUOTA_METRIC_NAME)
            .await?
        {
            QuotaDecision::Allowed => Ok(()),
            QuotaDecision::Denied { reason } => Err(DomainError::quota_exceeded(reason)),
        }
    }

    // ── multipart upload (P2-M3) ─────────────────────────────────────────────

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
        PolicyResolver::check_allowed_mime(&policy, declared_mime)?;
        let effective_max = PolicyResolver::compute_effective_max_bytes(
            &policy,
            declared_mime,
            backend.capabilities().max_size_bytes,
        );
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
        let effective_max = PolicyResolver::compute_effective_max_bytes(
            &policy,
            &session.declared_mime,
            backend.capabilities().max_size_bytes,
        );
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
}
