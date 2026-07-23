//! Read-only queries (file, metadata, versions) and version-lifecycle operations
//! (download URL issuance, version listing, restore, and deletion).

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

    /// Batched custom-metadata lookup for `list_files`'s page of results:
    /// one `IN (...)` query for every file id in `file_ids` instead of one
    /// `list_metadata` call per file (`GET /files` was previously built with
    /// an empty `custom_metadata` on every item -- see `handlers::list_files`).
    ///
    /// Takes raw file ids with **no `SecurityContext`/authorization check of
    /// its own** -- it trusts the caller to have already authorized every id
    /// in `file_ids` (e.g. via a preceding `list_files` call, which
    /// tenant-scopes and owner-gates its results before returning them). It
    /// is `pub(crate)`, not `pub`, precisely to keep its only intended caller
    /// (`handlers::list_files`, same crate) from becoming a foot-gun for a
    /// future caller reachable from outside this crate that might pass
    /// unauthorized ids straight through to an unscoped batched DB read.
    /// Do **not** call this with file ids that have not already come out of
    /// an authorized listing for the current `SecurityContext`.
    pub(crate) async fn list_metadata_for_files(
        &self,
        file_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, Vec<CustomMetadataEntry>>, DomainError> {
        self.store.list_metadata_for_files(file_ids).await
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
        // Ownership gate: the coarse READ check above is resource-less (see
        // module docs) — it only answers "may this subject read files at
        // all," not "whose files." `owner` is attacker-controlled (built from
        // the request query), so without this gate any tenant member could
        // enumerate another subject's file listing via
        // `?owner_kind=user&owner_id=<victim>`. A caller listing their own
        // files (`owner.owner_id == ctx.subject_id()`, whether `owner_kind`
        // is `user` or another kind the caller itself holds) proceeds
        // unconditionally; any other owner requires `ADMIN_POLICY` — on
        // `Forbidden` this propagates via `?` instead of listing.
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

    // ── pub(crate) accessors for DataPlaneService ─────────────────────────────

    /// Fetch a single version by `(file_id, version_id)` — delegated to the
    /// data plane so it does not need to hold a direct `Store` reference.
    pub(crate) async fn get_version(
        &self,
        file_id: uuid::Uuid,
        version_id: uuid::Uuid,
    ) -> Result<Option<file_storage_sdk::FileVersion>, crate::domain::error::DomainError> {
        self.store.get_version(file_id, version_id).await
    }

    // ── download + versioning ─────────────────────────────────────────────────

    /// `GET /files/{id}/download-url`: issue a signed download URL pinned to the
    /// current content (or a specific `version_id`).
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

        // P2 1.11: the content ETag is computed once and threaded both into
        // the GET token's claims (so the sidecar can echo it as a real
        // `ETag` header with no DB lookup) and into the ticket returned here
        // — one source of truth (`etag::content_etag`).
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

    /// `GET /files/{id}/versions`: list a page of a file's versions, newest
    /// first, offset-paginated and capped at `ServiceConfig::max_page_size`
    /// (P2 2.2 — closes the unbounded-listing amplification surface).
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

    /// Batched manifest lookup for `list_versions`'s page of results: one
    /// `IN (...)` query for every version id in `version_ids` instead of one
    /// `get_version_manifest` call per version. Only
    /// `multipart-composite-sha256` versions have a manifest row, so
    /// `whole-sha256` version ids are simply absent from the map.
    ///
    /// Takes raw version ids with **no `SecurityContext`/authorization check
    /// of its own** — it trusts the caller to have already authorized every
    /// id in `version_ids` (e.g. via a preceding `list_versions` call, which
    /// tenant-scopes and `read`-authorizes the file before returning its
    /// versions). It is `pub(crate)`, not `pub`, for exactly the same
    /// foot-gun-avoidance reason as [`Self::list_metadata_for_files`] above.
    pub(crate) async fn manifests_for_versions(
        &self,
        version_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, String>, DomainError> {
        self.store.get_version_manifests(version_ids).await
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

    // ── delete ──────────────────────────────────────────────────────────────────

    /// `DELETE /files/{id}`: remove the file and all versions (FK cascade) under
    /// an `If-Match` content-ETag precondition, then best-effort delete the
    /// backend blobs. `If-Match` is **required** (see api.md §DELETE); pass `"*"`
    /// to delete unconditionally when the ETag is unknown.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
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

        self.delete_file_inner(ctx, file_id).await?;
        self.metrics.record_operation("delete_file", "ok");
        Ok(())
    }

    /// Inner (unconditional) file deletion: authorization and If-Match must have
    /// already been checked by the caller. Collects versions, removes the DB row
    /// (and FK children via cascade), then best-effort-deletes all backend blobs.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    pub(super) async fn delete_file_inner(
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

    /// Delete a single version (and its backend blob). Deleting the only version
    /// is equivalent to deleting the file.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
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
            // Last version → delete the whole file. Authorization has already been
            // checked above; skip the If-Match gate (delete_version has its own
            // contract — no If-Match on DELETE /files/{id}/versions/{vid}).
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

        // @cpt-cf-file-storage-fr-audit-trail
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
            // P2 2.7: our `content_id == version_id` check above ran against a
            // pre-transaction snapshot; the store re-checks transactionally and
            // guards the delete at the DB level, so `false` here means a
            // concurrent `bind` promoted this exact version to current (or
            // deleted it outright) in the window between that snapshot and the
            // transactional delete. Re-fetch (outside the tx, for error-message
            // purposes only — the dangle itself was already prevented by the
            // DB-level guard) to report the more accurate error.
            return Err(match self.store.get_version(file_id, version_id).await? {
                Some(_) => DomainError::conflict(
                    "cannot delete the current version; bind another version first",
                ),
                None => DomainError::version_not_found(file_id, version_id),
            });
        }
        // @cpt-cf-file-storage-fr-usage-reporting
        // Debit this non-current version's bytes. The `all.len() <= 1` branch
        // above already delegated to `delete_file_inner` (which reports its
        // own whole-file debit), so this arm only runs when at least one
        // other version remains -- no double-count with that path.
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

#[cfg(test)]
#[path = "read_ops_tests.rs"]
mod read_ops_tests;
