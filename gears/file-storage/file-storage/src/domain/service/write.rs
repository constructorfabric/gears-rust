//! Write-path operations: finalize upload, bind (CAS), metadata update, and ownership transfer.

use std::collections::HashMap;

use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage_sdk::{CustomMetadataPatch, File};

use crate::domain::audit::{AuditEntry, AuditOperation};
use crate::domain::authz::actions;
use crate::domain::error::DomainError;
use crate::domain::etag;
use crate::domain::policy::PolicyResolver;
use crate::domain::service::{FileService, VersionRef};
use crate::infra::external_clients::UsageDelta;
use crate::infra::signed_url::{Claims, Op, UploadConstraints};

impl FileService {
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

    /// Record an uploaded version's size+hash and mark it available. Called by
    /// the sidecar after streaming bytes to the backend (write action).
    ///
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
        let effective_max = PolicyResolver::compute_effective_max_bytes(
            &policy,
            &version_mime,
            backend.capabilities().max_size_bytes,
        );
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

    /// Issue a signed download URL for a version (shared helper used by
    /// `read_ops.rs`). Visibility is `pub(super)` so only sibling modules use it.
    pub(super) fn build_download_url(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        backend_id: String,
        backend_path: String,
    ) -> Result<String, DomainError> {
        self.sign_url(
            Op::Get,
            &VersionRef {
                file_id,
                version_id,
                backend_id,
                backend_path,
            },
            UploadConstraints::default(),
        )
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
        PolicyResolver::check_metadata_limits(&policy, &result_pairs)?;

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

    /// Record an uploaded version's size+hash and mark it available, authorized
    /// by the sidecar's signed upload token rather than a user `SecurityContext`.
    ///
    /// This is the token-authenticated variant of [`finalize_upload`]. The
    /// control plane minted the token at presign time, so verifying it here
    /// constitutes full authorization — no separate user re-auth is needed
    /// (DESIGN §bind-service "Trusts a sidecar-reported size/hash (the upload
    /// URL was control-signed)").
    ///
    /// The `claims` have already been verified by the caller (signature + expiry
    /// + `op == Put` + `file_id`/`version_id` match).
    ///
    /// This method performs the same defense-in-depth policy size check as the
    /// user-facing path.
    ///
    /// The actor in the audit row is recorded as `"sidecar"` with the `Uuid::nil`
    /// actor id, since no user identity is present in a sidecar callback.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    pub async fn finalize_upload_by_token(
        &self,
        claims: &Claims,
        size: i64,
        hash_value: Vec<u8>,
    ) -> Result<(), DomainError> {
        let file_id = claims.file_id;
        let version_id = claims.version_id;

        // Fetch file via allow_all scope: the data plane operates on a
        // (file_id, version_id) pair already minted by the control plane.
        let file = self
            .store
            .require_file(&AccessScope::allow_all(), file_id)
            .await?;

        // Defense-in-depth size check: re-enforce the policy size ceiling at
        // finalization time even though the sidecar already checked the upload
        // constraint in the signed URL.
        // @cpt-cf-file-storage-fr-size-limits-policy
        let version = self.store.get_version(file_id, version_id).await?;
        let (version_mime, backend_id) = version.as_ref().map_or_else(
            || ("application/octet-stream".to_owned(), String::new()),
            |v| (v.mime_type.clone(), v.backend_id.clone()),
        );
        let policy = self
            .get_effective_policy_internal(file.tenant_id, file.owner_id)
            .await?;
        let backend = if backend_id.is_empty() {
            self.backends.default_backend()
        } else {
            self.backends.get(&backend_id)?
        };
        let effective_max = PolicyResolver::compute_effective_max_bytes(
            &policy,
            &version_mime,
            backend.capabilities().max_size_bytes,
        );
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
        // Actor is "sidecar" with nil UUID — no user identity is available in
        // a token-authenticated callback.
        let audit = AuditEntry::success(
            file.tenant_id,
            "sidecar",
            Uuid::nil(),
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

    /// Delete a backend blob, logging (not failing) on error. A failed delete
    /// degrades to an orphan reconciled by the P2 cleanup engine.
    pub(super) async fn best_effort_blob_delete(&self, backend_id: &str, path: &str) {
        let Ok(backend) = self.backends.get(backend_id) else {
            return;
        };
        if let Err(err) = backend.delete(path).await {
            tracing::warn!(?err, path, "best-effort backend delete failed");
        }
    }
}
