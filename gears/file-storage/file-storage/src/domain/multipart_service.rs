//! `MultipartService` — multipart upload control-plane logic.
//!
//! Owns the P2-M3 / multipart-coordinator flows: initiate (server-authoritative
//! plan + per-part signed URLs), complete, and abort.
//!
//! The control-plane byte route (`upload_multipart_part`) has been removed as
//! part of the multipart-coordinator feature — bytes now flow exclusively to
//! the sidecar via the per-part signed URLs returned by `initiate_multipart_upload`
//! (DESIGN §4.6, ADR-0003, FEATURE §8 migration).
//!
//! Holds its own copies of the shared dependencies (`Store`, `BackendRegistry`,
//! `Authorizer`, `QuotaClient`, `Issuer`) so it does NOT reference `FileService`
//! — that keeps the fan-in graph clean and avoids raising the HK score of
//! `FileService`.

// Domain terms (ETag, If-Match, FileStorage, GET/PUT, BLAKE3) appear in the docs.
#![allow(clippy::doc_markdown)]

use std::sync::Arc;

use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::audit::{AuditEntry, AuditOperation};
use crate::domain::authz::{Authorizer, actions};
use crate::domain::error::DomainError;
use crate::domain::multipart::{
    MultipartPartPlan, MultipartPlan, MultipartUploadState, compute_plan,
};
use crate::domain::policy::{PolicyResolver, PolicyScope};
use crate::domain::ports::MultipartStore;
use crate::infra::backend::BackendRegistry;
use crate::infra::external_clients::{QuotaClient, QuotaDecision};
use crate::infra::signed_url::{Claims, Issuer, MultipartClaims, Op, UploadConstraints};

/// Quota metric name (duplicated from service.rs; both refer to the same
/// platform metric — no abstraction needed here).
const QUOTA_METRIC_NAME: &str = "gts.cf.qe.metric.type.v1~cf.qe.metric.file_storage_bytes.v1";

/// The multipart-upload service (multipart-coordinator feature).
///
/// Extracted from `FileService` to reduce its Henry-Kafura coupling score.
/// All multipart control-plane operations live here; the struct is wired
/// alongside `FileService` in `gear.rs` and served under the same REST prefix.
#[allow(unknown_lints, de0309_must_have_domain_model)]
pub struct MultipartService {
    store: Arc<dyn MultipartStore>,
    backends: BackendRegistry,
    authorizer: Arc<dyn Authorizer>,
    quota_client: Option<Arc<dyn QuotaClient>>,
    /// Signed-URL issuer for minting per-part sidecar tokens.
    issuer: Arc<Issuer>,
    /// Base URL of the sidecar (e.g. `"http://sidecar.example.com"`).
    sidecar_base_url: String,
    /// Signed-URL TTL in seconds (shared with the session expiry).
    url_ttl_secs: i64,
}

impl MultipartService {
    pub fn new(
        store: Arc<dyn MultipartStore>,
        backends: BackendRegistry,
        authorizer: Arc<dyn Authorizer>,
        quota_client: Option<Arc<dyn QuotaClient>>,
        issuer: Arc<Issuer>,
        sidecar_base_url: String,
        url_ttl_secs: i64,
    ) -> Self {
        Self {
            store,
            backends,
            authorizer,
            quota_client,
            issuer,
            sidecar_base_url,
            url_ttl_secs,
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
    /// At multipart initiate time this is called with the declared total size,
    /// giving the quota service a precise figure rather than a pessimistic ceiling.
    ///
    /// **Fail-closed**: a failing quota client denies the request.
    ///
    /// @cpt-cf-file-storage-fr-storage-quota
    async fn check_quota_bytes(
        &self,
        tenant_id: Uuid,
        owner_id: Uuid,
        additional_bytes: u64,
    ) -> Result<(), DomainError> {
        let Some(qc) = &self.quota_client else {
            return Ok(());
        };
        match qc
            .check_storage_quota(tenant_id, owner_id, additional_bytes, QUOTA_METRIC_NAME)
            .await?
        {
            QuotaDecision::Allowed => Ok(()),
            QuotaDecision::Denied { reason } => Err(DomainError::quota_exceeded(reason)),
        }
    }

    /// Best-effort compensation when session persistence fails after the backend
    /// handle was already created and the pending version row was already inserted.
    ///
    /// Aborts the backend multipart handle and removes the pending version row so
    /// they are not left as orphans. Both steps are best-effort: errors are logged
    /// but not propagated — the caller's original error is returned instead, and
    /// any remaining orphans are reclaimed by the orphan-reconciliation sweep.
    async fn compensate_failed_session_create(
        &self,
        ctx: &SecurityContext,
        upload_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
        backend_path: &str,
        backend_handle: &str,
    ) {
        // Best-effort: abort the backend handle.
        let backend = self.backends.default_backend();
        if let Err(abort_err) = backend.abort_multipart(backend_path, backend_handle).await {
            tracing::warn!(
                ?abort_err,
                %upload_id,
                "best-effort backend abort failed after session persistence error"
            );
        }
        // Best-effort: remove the pending version row.
        if let Err(del_err) = self
            .store
            .delete_version(
                file_id,
                version_id,
                Self::audit_ok(
                    ctx,
                    Some(file_id),
                    AuditOperation::DeleteVersion,
                    serde_json::json!({
                        "version_id": version_id,
                        "reason": "multipart_session_create_failed"
                    }),
                ),
            )
            .await
        {
            tracing::warn!(
                ?del_err,
                %upload_id,
                "best-effort pending-version delete failed after session persistence error"
            );
        }
    }

    // ── multipart upload (multipart-coordinator feature) ─────────────────────

    /// `POST /files/{id}/multipart`: initiate a multipart upload session.
    ///
    /// Server-authoritative: validates the intent, pre-registers a `pending`
    /// version, creates the backend session, computes the **exact parts plan**,
    /// and returns **one signed URL per part** pointing at the sidecar
    /// (FEATURE §2, §3, §4; DESIGN §4.6).
    ///
    /// Policy/quota gates (FEATURE §7):
    /// - Allowed MIME: `415`
    /// - Declared size ≤ effective max: `413`
    /// - Storage quota: `507`
    ///
    /// The complete-time total-size check is kept as defence-in-depth.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    /// @cpt-cf-file-storage-fr-size-limits-policy
    /// @cpt-cf-file-storage-fr-storage-quota
    pub async fn initiate_multipart_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        declared_mime: &str,
        declared_size: u64,
        preferred_part_size: Option<u64>,
        _concurrency: Option<u32>,
    ) -> Result<MultipartPlan, DomainError> {
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

        // Policy checks: allowed mime type and size (at initiate, against the
        // declared total size — DESIGN §4.6 server-authoritative gate).
        //
        // @cpt-cf-file-storage-fr-size-limits-policy
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

        // Gate: reject if the declared total size exceeds the effective limit.
        // This is the DESIGN-aligned fix for CodeRabbit F2: validate up front at
        // initiate time rather than deferring to complete time.
        //
        // @cpt-cf-file-storage-fr-size-limits-policy
        if let Some(limit) = effective_max
            && declared_size > limit
        {
            return Err(DomainError::policy_size_exceeded(
                limit,
                "policy size limit",
            ));
        }

        // Quota check against the declared size (not the pessimistic effective_max).
        // PRD §5.4: "check before accepting any operation that increases storage
        // consumption" — the declared size is our best estimate at this stage.
        //
        // @cpt-cf-file-storage-fr-storage-quota
        self.check_quota_bytes(tenant_id, file.owner_id, declared_size)
            .await?;

        let now = OffsetDateTime::now_utc();
        let upload_id = Uuid::now_v7();
        let version_id = Uuid::now_v7();
        let backend_path = Self::backend_path(file_id, version_id);
        let backend_id = backend.id().to_owned();

        // Compute the server-authoritative parts plan (FEATURE §3).
        // `backend_min_part_size` is not yet exposed by the BackendCapabilities
        // API so we fall back to the `DEFAULT_MIN_PART_SIZE` constant.
        let (chosen_part_size, raw_parts) = compute_plan(declared_size, preferred_part_size, None);

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

        // Use the configured TTL for both the session row and the signed URLs.
        let expires_at = now + time::Duration::seconds(self.url_ttl_secs.max(1));

        // Persist the session row. On failure, best-effort compensate to avoid
        // orphaning the backend handle and the pending version row.
        if let Err(err) = self
            .store
            .create_multipart_upload(
                upload_id,
                file_id,
                version_id,
                &backend_handle,
                declared_mime,
                declared_size,
                chosen_part_size,
                expires_at,
                now,
            )
            .await
        {
            self.compensate_failed_session_create(
                ctx,
                upload_id,
                file_id,
                version_id,
                &backend_path,
                &backend_handle,
            )
            .await;
            return Err(err);
        }

        // Mint one signed URL per part (FEATURE §4).
        // Each token carries the exact `size` claim the sidecar will enforce.
        let exp = expires_at.unix_timestamp();
        let mut parts = Vec::with_capacity(raw_parts.len());
        for (part_number, offset, size) in raw_parts {
            let claims = Claims {
                op: Op::MultipartPart,
                file_id,
                version_id,
                backend_id: backend_id.clone(),
                backend_path: backend_path.clone(),
                exp,
                upload: UploadConstraints::default(),
                multipart: MultipartClaims {
                    upload_id,
                    part_number,
                    offset,
                    size,
                },
            };
            let token = self.issuer.issue(claims, now)?;
            let upload_url = format!(
                "{}/api/file-storage-data/v1/multipart/{file_id}/{version_id}/parts/{part_number}?fs-token={token}",
                self.sidecar_base_url
            );
            parts.push(MultipartPartPlan {
                part_number,
                offset,
                size,
                upload_url,
            });
        }

        Ok(MultipartPlan {
            upload_id,
            version_id,
            part_hash_algorithm: "SHA-256".to_owned(),
            part_size: chosen_part_size,
            parts,
            expires_at,
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

        // Compute total assembled size from the parts that the sidecar wrote.
        let total_size: i64 = parts.iter().map(|p| p.size).sum();

        // Defence-in-depth: verify the assembled size matches `declared_size`
        // (FEATURE §6, §7 — "Total assembled size = declared_size").
        //
        // The primary enforcement is per-part at the sidecar (the `size` claim
        // in each token); this check catches residual mismatches (e.g. a
        // missing/extra part).
        if session.declared_size > 0 {
            let expected = i64::try_from(session.declared_size).unwrap_or(i64::MAX);
            if total_size != expected {
                return Err(DomainError::conflict(format!(
                    "multipart upload {upload_id}: assembled size {total_size} \
                     does not match declared_size {expected}"
                )));
            }
        }

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
        let completed = self
            .store
            .complete_multipart_upload(upload_id, audit)
            .await?;
        if !completed {
            // Concurrent complete/abort already transitioned the session out of
            // `in_progress`. The backend object was already assembled above; the
            // now-orphaned blob is reclaimed by the orphan-reconciliation sweep.
            return Err(DomainError::multipart_upload_not_in_progress(
                upload_id,
                session.state.as_str(),
            ));
        }

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

        // Mark the session aborted (CAS: in_progress → aborted).
        let aborted = self.store.abort_multipart_upload(upload_id, audit).await?;
        if !aborted {
            // A concurrent complete/abort transitioned the session out of
            // `in_progress` between our snapshot read and this CAS. Surface a
            // conflict and STOP — critically, we must not fall through to the
            // pending-version delete below: had the race been a concurrent
            // *complete*, that version is now finalized/bound and deleting it
            // would corrupt the completed upload.
            return Err(DomainError::multipart_upload_not_in_progress(
                upload_id,
                session.state.as_str(),
            ));
        }

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
