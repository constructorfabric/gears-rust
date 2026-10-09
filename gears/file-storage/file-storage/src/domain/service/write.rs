//! Write-path operations: finalize upload, bind (CAS), metadata update, and ownership transfer.

use std::collections::HashMap;

use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage_sdk::{ByteRange, CustomMetadataPatch, File};

use crate::domain::audit::{AuditEntry, AuditOperation};
use crate::domain::authz::actions;
use crate::domain::error::DomainError;
use crate::domain::etag;
use crate::domain::policy::PolicyResolver;
use crate::domain::service::{FileService, VersionRef};
use crate::infra::backend::StorageBackend;
use crate::infra::content::mime::{
    MIME_SNIFF_PREFIX_BYTES, enforce_size_ceiling_for_validated_mime, validate_and_resolve_mime,
};
use crate::infra::external_clients::UsageDelta;
use crate::infra::signed_url::{Claims, Op, UploadConstraints};

/// Verify the object the sidecar reported as uploaded, without reading it back.
///
/// Finalize trust model: the callback is authenticated by the mandatory internal credential
/// and the sidecar measured the size and SHA-256 while streaming the PUT, so those are
/// trusted. This only checks the stored length via backend `size` and reads the bounded
/// prefix (`MIME_SNIFF_PREFIX_BYTES`) needed to sniff the MIME type. A missing object is a
/// `validation("content")` error; other backend failures propagate unchanged.
///
/// Returns the MIME sniff prefix (empty for a zero-length object).
async fn check_uploaded_object(
    backend: &dyn StorageBackend,
    backend_path: &str,
    claimed_size: i64,
) -> Result<Vec<u8>, DomainError> {
    let actual_size = match backend.size(backend_path).await {
        Ok(n) => i64::try_from(n).unwrap_or(i64::MAX),
        // `exists` is the authoritative absent signal; if it fails too, surface `size`'s error.
        Err(size_err) => {
            return Err(match backend.exists(backend_path).await {
                Ok(false) => DomainError::validation(
                    "content",
                    "no uploaded content found at the backend path; PUT was not completed",
                ),
                _ => size_err,
            });
        }
    };
    if actual_size != claimed_size {
        return Err(DomainError::validation(
            "size",
            "claimed size does not match the uploaded content",
        ));
    }
    if actual_size == 0 {
        return Ok(Vec::new());
    }
    let prefix_len = u64::try_from(MIME_SNIFF_PREFIX_BYTES).unwrap_or(u64::MAX);
    let end = actual_size.cast_unsigned().min(prefix_len) - 1;
    let prefix = backend
        .get_range(backend_path, ByteRange::Inclusive { start: 0, end })
        .await?;
    Ok(prefix.to_vec())
}

impl FileService {
    /// Authorize a write to `file_id` without mutating anything. The data plane calls this
    /// **before** writing bytes, so a rejected request never overwrites blob content
    /// (`finalize_upload` re-checks afterwards).
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

    /// Record an uploaded version's size and hash and mark it available (see
    /// `check_uploaded_object` for what is verified).
    #[tracing::instrument(skip_all)]
    pub async fn finalize_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        hash_value: Vec<u8>,
    ) -> Result<(), DomainError> {
        if size < 0 {
            return Err(DomainError::validation("size", "must be non-negative"));
        }

        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        // Defense in depth: re-enforce the policy size ceiling (the signed URL already did).
        let version = self
            .store
            .get_version(file_id, version_id)
            .await?
            .ok_or_else(|| DomainError::version_not_found(file_id, version_id))?;
        let version_mime = version.mime_type.clone();
        let backend_id = version.backend_id.clone();
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

        let mime_sniff_prefix =
            check_uploaded_object(backend.as_ref(), &version.backend_path, size).await?;
        let actual_size = size;
        let actual_hash = hash_value;

        // The declared MIME is untrusted: the sniffed type wins when the bytes carry a
        // recognizable signature, a mismatch is rejected.
        let validated_mime = validate_and_resolve_mime(&version_mime, &mime_sniff_prefix)?;
        enforce_size_ceiling_for_validated_mime(
            &policy,
            &version_mime,
            &validated_mime,
            backend.capabilities().max_size_bytes,
            actual_size,
        )?;

        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::FinalizeVersion,
            serde_json::json!({ "version_id": version_id, "size": size }),
        );

        // `validated_mime` replaces the client's declaration.
        let ok = self
            .store
            .finalize_version(
                file_id,
                version_id,
                actual_size,
                actual_hash,
                // Single-part upload: whole-object SHA-256, no manifest.
                crate::infra::content::hash_mode::HashMode::WholeSha256,
                None,
                None,
                Some(validated_mime),
                audit,
            )
            .await?;
        if !ok {
            // Already finalized (409) vs row gone (404), from the earlier `version` snapshot.
            return Err(
                if version.status == file_storage_sdk::VersionStatus::Available {
                    DomainError::conflict("version already finalized")
                } else {
                    DomainError::version_not_found(file_id, version_id)
                },
            );
        }

        // `create_file` already counted the file (bytes unknown then); credit the bytes here.
        self.report_usage(UsageDelta {
            tenant_id: file.tenant_id,
            owner_id: file.owner_id,
            bytes_delta: actual_size,
            file_count_delta: 0,
        });

        self.metrics.record_operation("finalize_upload", "ok");
        Ok(())
    }

    /// `POST /files/{id}/bind`: swap the content pointer to `version_id` under optimistic
    /// CAS guarded by the `If-Match` content ETag; `PreconditionFailed` on conflict.
    ///
    /// `if_match` is the opaque content ETag, `*`, or `None` for the first bind. The server
    /// recomputes the current ETag and compares; it never decodes an ETag.
    #[tracing::instrument(skip_all)]
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

        let expected_content_id = file.content_id;
        let current_etag = expected_content_id.map(|c| etag::content_etag(file_id, c));
        match if_match {
            // Only the first bind may omit `If-Match`; a rebind without it would be an
            // unconditional overwrite.
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

        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::PatchContent,
            serde_json::json!({ "version_id": version_id }),
        );

        let event = Some(Self::make_file_event(
            file.tenant_id,
            file.owner_id,
            file_id,
            "file.content_updated",
            serde_json::json!({ "version_id": version_id }),
        ));

        // One transaction, so `files.content_id` and `file_versions.is_current` never diverge.
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

        let bound = self.store.require_file(&scope, file_id).await?;
        self.metrics.record_operation("bind", "ok");
        Ok(bound)
    }

    /// Signed download URL for a version; `download_meta` is `(content_type, etag)`.
    pub(super) fn build_download_url(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        backend_id: String,
        backend_path: String,
        download_meta: Option<(String, String)>,
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
            download_meta,
        )
    }

    /// `PATCH /files/{id}`: JSON-merge-patch the custom metadata and bump
    /// `meta_version`, optionally guarded by `If-Match-Metadata`.
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

        // Validate the metadata as it will be after the patch.
        let policy = self
            .get_effective_policy_internal(ctx.subject_tenant_id(), file.owner_id)
            .await?;
        let existing = self.store.list_metadata(file_id).await?;
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

        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::PatchMetadata,
            serde_json::json!({ "expected_meta_version": expected_meta_version }),
        );

        // CAS and patch run in one transaction: a stale `expected_meta_version` aborts
        // first, and a failed insert rolls back the per-key delete-then-insert upsert.
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

    /// `POST /files/{id}/transfer`: replace the file's owner kind and id, with audit and event
    /// in the same transaction. `tenant_id` comes from the stored file, never the request.
    /// The existence of `new_owner_id` is not verified (the gear has no principal directory).
    pub async fn transfer_ownership(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        new_owner_kind: file_storage_sdk::OwnerKind,
        new_owner_id: Uuid,
    ) -> Result<File, DomainError> {
        if new_owner_id.is_nil() {
            return Err(DomainError::validation(
                "new_owner_id",
                "must not be the nil UUID",
            ));
        }

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

    /// Like `finalize_upload`, but authorized by the sidecar's signed upload token (minted at
    /// presign time) instead of a user `SecurityContext`.
    ///
    /// The caller has already verified `claims` (signature, expiry, `op == Put`, ids). The
    /// audit actor is `"sidecar"` with the nil UUID.
    #[tracing::instrument(skip_all)]
    pub async fn finalize_upload_by_token(
        &self,
        claims: &Claims,
        size: i64,
        hash_value: Vec<u8>,
    ) -> Result<(), DomainError> {
        if size < 0 {
            return Err(DomainError::validation("size", "must be non-negative"));
        }

        let file_id = claims.file_id;
        let version_id = claims.version_id;

        // `allow_all`: the `(file_id, version_id)` pair was minted by the control plane.
        let file = self
            .store
            .require_file(&AccessScope::allow_all(), file_id)
            .await?;

        // Defense in depth: re-enforce the policy size ceiling (the signed URL already did).
        let version = self
            .store
            .get_version(file_id, version_id)
            .await?
            .ok_or_else(|| DomainError::version_not_found(file_id, version_id))?;
        let version_mime = version.mime_type.clone();
        let backend_id = version.backend_id.clone();
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

        let mime_sniff_prefix =
            check_uploaded_object(backend.as_ref(), &version.backend_path, size).await?;
        let actual_size = size;
        let actual_hash = hash_value;

        // The declared MIME is untrusted: the sniffed type wins when the bytes carry a
        // recognizable signature, a mismatch is rejected.
        let validated_mime = validate_and_resolve_mime(&version_mime, &mime_sniff_prefix)?;
        enforce_size_ceiling_for_validated_mime(
            &policy,
            &version_mime,
            &validated_mime,
            backend.capabilities().max_size_bytes,
            actual_size,
        )?;

        let audit = AuditEntry::success(
            file.tenant_id,
            "sidecar",
            Uuid::nil(),
            Some(file_id),
            AuditOperation::FinalizeVersion,
            serde_json::json!({ "version_id": version_id, "size": size }),
        );

        // `validated_mime` replaces the client's declaration.
        let ok = self
            .store
            .finalize_version(
                file_id,
                version_id,
                actual_size,
                actual_hash,
                // Single-part upload: whole-object SHA-256, no manifest.
                crate::infra::content::hash_mode::HashMode::WholeSha256,
                None,
                None,
                Some(validated_mime),
                audit,
            )
            .await?;
        if !ok {
            // Already finalized (409) vs row gone (404), from the earlier `version` snapshot.
            return Err(
                if version.status == file_storage_sdk::VersionStatus::Available {
                    DomainError::conflict("version already finalized")
                } else {
                    DomainError::version_not_found(file_id, version_id)
                },
            );
        }

        self.report_usage(UsageDelta {
            tenant_id: file.tenant_id,
            owner_id: file.owner_id,
            bytes_delta: actual_size,
            file_count_delta: 0,
        });

        self.metrics
            .record_operation("finalize_upload_by_token", "ok");
        Ok(())
    }

    /// Delete a backend blob, logging (not failing) on error; a failure leaves an orphan
    /// for the cleanup engine.
    pub(super) async fn best_effort_blob_delete(&self, backend_id: &str, path: &str) {
        let Ok(backend) = self.backends.get(backend_id) else {
            return;
        };
        if let Err(err) = backend.delete(path).await {
            self.metrics.record_backend_error(backend_id, "delete");
            tracing::warn!(?err, path, "best-effort backend delete failed");
        }
    }
}
