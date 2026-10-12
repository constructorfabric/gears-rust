//! Read-only queries and version-lifecycle operations (download URL, restore, delete).

use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage_sdk::{CustomMetadataEntry, File, FileVersion, OwnerFilter};

use crate::domain::audit::AuditOperation;
use crate::domain::authz::actions;
use crate::domain::error::DomainError;
use crate::domain::etag;
use crate::domain::service::{DownloadTicket, FileService};
use crate::infra::external_clients::UsageDelta;

impl FileService {
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
        // The query is always tenant-scoped, regardless of the PDP's returned constraints.
        self.authorizer
            .authorize(ctx, actions::READ, "", None)
            .await?;
        // The READ check above is resource-less and `owner` comes from the request, so
        // listing another owner's files requires `ADMIN_POLICY` (else any tenant member
        // could enumerate a victim's files).
        if owner.owner_id != ctx.subject_id() {
            self.authorizer
                .authorize(ctx, actions::ADMIN_POLICY, "", None)
                .await?;
        }
        let limit = limit
            .unwrap_or(self.cfg.default_page_size)
            .min(self.cfg.max_page_size);
        self.store
            .list_files(&Self::tenant_scope(ctx), owner, limit, offset)
            .await
    }

    /// Fetch a single version (the data plane has no direct `Store` reference).
    pub(crate) async fn get_version(
        &self,
        file_id: uuid::Uuid,
        version_id: uuid::Uuid,
    ) -> Result<Option<file_storage_sdk::FileVersion>, crate::domain::error::DomainError> {
        self.store.get_version(file_id, version_id).await
    }

    /// `GET /files/{id}/download-url`: signed download URL for the current (or given) version.
    #[tracing::instrument(skip_all)]
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

        // One ETag source for both the GET token claims and the returned ticket.
        let content_etag = etag::content_etag(file_id, target);
        let download_url = self.build_download_url(
            file_id,
            target,
            version.backend_id,
            version.backend_path,
            Some((version.mime_type, content_etag.clone())),
        )?;
        self.metrics.record_operation("download_url", "ok");
        Ok(DownloadTicket {
            download_url,
            etag: content_etag,
            version_id: target,
        })
    }

    /// `GET /files/{id}/versions`: newest first, offset-paginated, capped at
    /// `ServiceConfig::max_page_size`.
    pub async fn list_versions(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        limit: Option<u64>,
        offset: u64,
    ) -> Result<Vec<FileVersion>, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::READ, &file.gts_file_type, Some(file_id))
            .await?;
        let limit = limit
            .unwrap_or(self.cfg.default_page_size)
            .min(self.cfg.max_page_size);
        self.store.list_versions_page(file_id, limit, offset).await
    }

    /// Restore a prior version as current (a rebind: pointer swap, no re-upload).
    pub async fn restore_version(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<file_storage_sdk::File, DomainError> {
        let file = self.get_file(ctx, file_id).await?;
        let if_match = etag::etag_for(&file);
        self.bind(ctx, file_id, version_id, if_match.as_deref())
            .await
    }

    /// `DELETE /files/{id}`: remove the file and all versions (FK cascade), then
    /// best-effort delete the backend blobs. `If-Match` is **required**; `"*"` deletes
    /// unconditionally.
    #[tracing::instrument(skip_all)]
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

        self.delete_file_inner(ctx, file_id).await?;
        self.metrics.record_operation("delete_file", "ok");
        Ok(())
    }

    /// Unconditional file deletion; the caller must have checked authorization and `If-Match`.
    pub(super) async fn delete_file_inner(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
    ) -> Result<(), DomainError> {
        // Callers already authorized and enforced the tenant boundary via `require_file`.
        let scope = AccessScope::allow_all();

        // Collected before the rows (and FK children) vanish.
        let versions = self.store.list_versions(file_id).await?;

        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::DeleteFile,
            serde_json::json!({ "version_count": versions.len() }),
        );

        // Tenant/owner for the event payload must be read before deletion.
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

        let total_bytes: i64 = versions.iter().map(|v| v.size).sum();
        self.report_usage(UsageDelta {
            tenant_id: event_tenant,
            owner_id: event_owner,
            bytes_delta: -total_bytes,
            file_count_delta: -1,
        });

        // A failed blob delete leaves an orphan for the cleanup engine.
        for v in versions {
            self.best_effort_blob_delete(&v.backend_id, &v.backend_path)
                .await;
        }
        Ok(())
    }

    /// Delete a single version and its blob; deleting the only version deletes the file.
    #[tracing::instrument(skip_all)]
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
            if !all.iter().any(|v| v.version_id == version_id) {
                return Err(DomainError::version_not_found(file_id, version_id));
            }
            // Last version: delete the whole file (no `If-Match` on this endpoint).
            self.delete_file_inner(ctx, file_id).await?;
            self.metrics.record_operation("delete_version", "ok");
            return Ok(());
        }
        let Some(version) = all.into_iter().find(|v| v.version_id == version_id) else {
            return Err(DomainError::version_not_found(file_id, version_id));
        };
        if file.content_id == Some(version_id) {
            return Err(DomainError::conflict(
                "cannot delete the current version; bind another version first",
            ));
        }

        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::DeleteVersion,
            serde_json::json!({ "version_id": version_id }),
        );

        let removed = self
            .store
            .delete_version(file_id, version_id, audit)
            .await?;
        if !removed {
            // The `content_id` check above used a pre-transaction snapshot; the store re-checks
            // transactionally, so `false` means a concurrent `bind` made this version current
            // (or it was deleted). Re-fetch only to report the accurate error.
            return Err(match self.store.get_version(file_id, version_id).await? {
                Some(_) => DomainError::conflict(
                    "cannot delete the current version; bind another version first",
                ),
                None => DomainError::version_not_found(file_id, version_id),
            });
        }
        // The single-version branch reports its own debit via `delete_file_inner`.
        self.report_usage(UsageDelta {
            tenant_id: file.tenant_id,
            owner_id: file.owner_id,
            bytes_delta: -version.size,
            file_count_delta: 0,
        });

        self.best_effort_blob_delete(&version.backend_id, &version.backend_path)
            .await;
        self.metrics.record_operation("delete_version", "ok");
        Ok(())
    }
}
