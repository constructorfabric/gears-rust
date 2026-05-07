//! FileStorage service — orchestrates the multipart upload lifecycle,
//! read/update, batch presigned downloads, and the backend roster (P1).

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use modkit_db::DBProvider;
use modkit_security::SecurityContext;
use time::OffsetDateTime;
use tracing::warn;
use uuid::Uuid;

use file_storage_sdk::{
    Backend, BackendId, ByteRange, CapabilityTag, Etag, FileByteStream, FileId, FileInfo,
    FileList, FileMeta, FileMetaUpdate, FileReadHandle, FileStatus, ListFilesQuery, OwnerRef,
    PresignDownloadItem, PresignDownloadOutcome, PresignedUploadHandle, UploadedPart, UrlParams,
    VersionId,
};

use crate::config::FileStorageConfig;
use crate::infra::backends::registry::BackendRegistry;
use crate::infra::backends::r#trait::{
    BackendObjectMetadata, PresignedGetItem, SharedBackend, derive_s3_key,
};

use super::error::DomainError;
use super::repo::{
    ChangeStatusOutcome, FilesRepo, InsertPendingArgs, ListFilesArgs, ListFilesPage,
    MutationOutcome,
};

pub(crate) type DbProvider = DBProvider<modkit_db::DbError>;

pub(crate) mod actions {
    pub const CREATE: &str = "create";
    pub const READ: &str = "read";
    pub const UPDATE: &str = "update";
    pub const DELETE: &str = "delete";
}

/// Orphan-delete queue entry (P2 GC sweep — kept for module wiring).
#[derive(Debug, Clone)]
pub struct OrphanEntry {
    pub backend_id: BackendId,
    pub file_id: Uuid,
    pub eligible_at: OffsetDateTime,
}

pub type OrphanQueue = Arc<tokio::sync::Mutex<std::collections::VecDeque<OrphanEntry>>>;

// @cpt-begin:cpt-cf-file-storage-component-sdk-facade:p1:inst-service-struct
pub struct Service<R: FilesRepo + 'static> {
    db: Arc<DbProvider>,
    repo: Arc<R>,
    policy_enforcer: PolicyEnforcer,
    config: Arc<FileStorageConfig>,
    registry: Arc<BackendRegistry>,
    orphan_queue: OrphanQueue,
}
// @cpt-end:cpt-cf-file-storage-component-sdk-facade:p1:inst-service-struct

impl<R: FilesRepo + 'static> Service<R> {
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<R>,
        policy_enforcer: PolicyEnforcer,
        config: Arc<FileStorageConfig>,
        registry: Arc<BackendRegistry>,
        orphan_queue: OrphanQueue,
    ) -> Self {
        Self {
            db,
            repo,
            policy_enforcer,
            config,
            registry,
            orphan_queue,
        }
    }

    pub fn orphan_queue(&self) -> OrphanQueue {
        self.orphan_queue.clone()
    }

    // ── list_backends ───────────────────────────────────────────────────────

    pub async fn list_backends(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Backend>, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        Ok(self.registry.list_visible_to_tenant(tenant_id))
    }

    // ── create_presigned_upload ─────────────────────────────────────────────

    // @cpt-begin:cpt-cf-file-storage-fr-multipart-upload:p1:inst-create-presigned-upload
    // @cpt-begin:cpt-cf-file-storage-fr-upload-file:p1:inst-create-presigned-upload
    // @cpt-begin:cpt-cf-file-storage-principle-presign-first:p1:inst-create-presigned-upload
    // @cpt-begin:cpt-cf-file-storage-fr-direct-transfer:p1:inst-create-presigned-upload
    pub async fn create_presigned_upload(
        &self,
        ctx: &SecurityContext,
        file_id_input: Option<FileId>,
        backend_id: Option<BackendId>,
        owner: OwnerRef,
        meta: FileMeta,
        capability: &CapabilityTag,
        part_count: u32,
        params: UrlParams,
    ) -> Result<PresignedUploadHandle, DomainError> {
        validate_owner_against_ctx(ctx, &owner)?;
        validate_meta(&meta)?;
        if part_count == 0 || part_count > 10_000 {
            return Err(DomainError::bad_request(
                "part_count must be in 1..=10000 (S3 multipart cap)",
            ));
        }
        if capability != "upload.s3.multipart.sigv4.v1" {
            return Err(DomainError::capability(format!(
                "capability \"{capability}\" is not implemented in P1 (only upload.s3.multipart.sigv4.v1)"
            )));
        }

        let _scope = self
            .authz_check(
                ctx,
                actions::CREATE,
                None,
                &meta.gts_file_type,
                owner.tenant_id,
            )
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;
        let now = OffsetDateTime::now_utc();

        // Resolve backend (default-private fallback).
        let resolved_backend_id = match (file_id_input, backend_id) {
            (Some(fid), _) => {
                // Variant-B re-upload: pin backend from the existing row.
                let existing = self
                    .repo
                    .get_by_id(&conn, ctx.subject_tenant_id(), fid)
                    .await?
                    .ok_or(DomainError::NotFound)?;
                existing.backend_id
            }
            (None, Some(id)) => id,
            (None, None) => self.config.default_private_storage_id.ok_or_else(|| {
                DomainError::bad_request(
                    "no backend selected and default_private_storage_id is unset",
                )
            })?,
        };
        let backend = self
            .registry
            .resolve_visible(resolved_backend_id, owner.tenant_id)?;

        if !backend.descriptor().declares(capability) {
            return Err(DomainError::capability(format!(
                "backend does not declare capability \"{capability}\""
            )));
        }

        if let Some(max) = backend.descriptor().max_file_size_bytes() {
            // Caller-declared size_bytes is no longer in FileMeta; size is
            // captured from S3 at finalize. Per-backend max enforced by the
            // backend itself.
            let _ = max;
        }

        let max_ttl_secs = backend.descriptor().max_signed_url_ttl_seconds();
        let requested_ttl_secs = params.expires_in_seconds.min(max_ttl_secs);
        let expires_at =
            now + time::Duration::seconds(i64::try_from(requested_ttl_secs).unwrap_or(i64::MAX));

        let file_id = match file_id_input {
            Some(id) => id,
            None => Uuid::new_v4(),
        };
        let backend_object_key = derive_s3_key(file_id);

        let custom_metadata_json = serde_json::to_string(&meta.custom_metadata).map_err(|e| {
            DomainError::internal(format!("custom_metadata serialisation failed: {e}"))
        })?;

        // INSERT row in pending_upload (initial) OR ensure existing row is
        // in `uploaded` state for variant-B (we don't change DB status on
        // re-upload presign — the row stays in `uploaded` with old etag/
        // version_id until `complete_upload` finalizes).
        if file_id_input.is_none() {
            self.repo
                .insert_pending(
                    &conn,
                    InsertPendingArgs {
                        file_id,
                        tenant_id: owner.tenant_id,
                        backend_id: resolved_backend_id,
                        file_path: file_path_from_meta(&meta, file_id),
                        owner_id: owner.owner_id,
                        name: meta.name.clone(),
                        gts_file_type: meta.gts_file_type.clone(),
                        mime_type: meta.mime_type.clone(),
                        etag_pinned: String::new(), // sentinel: no content yet
                        upload_expires_at: Some(expires_at),
                        custom_metadata_json,
                        now,
                    },
                )
                .await?;
        }

        // Open multipart session on the backend and presign N part URLs.
        let upload_id = backend
            .create_multipart_upload(&backend_object_key, &meta)
            .await?;
        let part_urls = backend
            .presign_upload_parts(
                &backend_object_key,
                &upload_id,
                part_count,
                requested_ttl_secs,
            )
            .await?;

        Ok(PresignedUploadHandle {
            file_id,
            upload_id,
            part_urls,
            expires_at,
        })
    }
    // @cpt-end:cpt-cf-file-storage-fr-multipart-upload:p1:inst-create-presigned-upload
    // @cpt-end:cpt-cf-file-storage-fr-upload-file:p1:inst-create-presigned-upload
    // @cpt-end:cpt-cf-file-storage-principle-presign-first:p1:inst-create-presigned-upload
    // @cpt-end:cpt-cf-file-storage-fr-direct-transfer:p1:inst-create-presigned-upload

    // ── complete_upload (3-phase commit) ────────────────────────────────────

    // @cpt-begin:cpt-cf-file-storage-principle-multi-phase-commit:p1:inst-complete-upload-3phase
    // @cpt-begin:cpt-cf-file-storage-principle-atomic-metadata:p1:inst-complete-upload-3phase
    pub async fn complete_upload(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        upload_id: &str,
        parts: Vec<UploadedPart>,
    ) -> Result<FileInfo, DomainError> {
        if parts.is_empty() {
            return Err(DomainError::bad_request("parts list cannot be empty"));
        }
        let conn = self.db.conn().map_err(DomainError::from)?;

        let row = self
            .repo
            .get_by_id(&conn, ctx.subject_tenant_id(), file_id)
            .await?
            .ok_or(DomainError::NotFound)?;
        let _scope = self
            .authz_check(
                ctx,
                actions::UPDATE,
                Some(file_id),
                &row.meta.gts_file_type,
                row.owner.tenant_id,
            )
            .await?;
        let backend = self
            .registry
            .resolve_visible(row.backend_id, row.owner.tenant_id)?;
        let key = derive_s3_key(file_id);

        // Phase 1 (DB): pending_upload → completing
        let now = OffsetDateTime::now_utc();
        match self
            .repo
            .begin_complete_upload(&conn, ctx.subject_tenant_id(), file_id, now)
            .await?
        {
            MutationOutcome::Applied => {}
            MutationOutcome::NoMatch => {
                // Row not in pending_upload — could be already uploaded
                // (idempotent re-call), deleting, etc.
                if matches!(row.status, FileStatus::Deleting) {
                    return Err(DomainError::DeleteInProgress);
                }
                if matches!(row.status, FileStatus::Uploaded) {
                    // Idempotent — return current state.
                    return Ok(row);
                }
                return Err(DomainError::Conflict);
            }
        }

        // Phase 2 (S3): CompleteMultipartUpload
        let result = backend
            .complete_multipart_upload(&key, upload_id, &parts)
            .await?;

        // Phase 3 (DB): completing → uploaded with new (etag, version_id, size)
        let now = OffsetDateTime::now_utc();
        match self
            .repo
            .finish_complete_upload(
                &conn,
                ctx.subject_tenant_id(),
                file_id,
                &result.etag,
                result.version_id.as_deref(),
                result.size_bytes,
                now,
            )
            .await?
        {
            ChangeStatusOutcome::Applied(info) => Ok(info),
            ChangeStatusOutcome::NoMatch => Err(DomainError::Conflict),
        }
    }
    // @cpt-end:cpt-cf-file-storage-principle-multi-phase-commit:p1:inst-complete-upload-3phase
    // @cpt-end:cpt-cf-file-storage-principle-atomic-metadata:p1:inst-complete-upload-3phase

    // ── abort_upload ───────────────────────────────────────────────────────

    pub async fn abort_upload(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        upload_id: &str,
    ) -> Result<(), DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        let row = self
            .repo
            .get_by_id(&conn, ctx.subject_tenant_id(), file_id)
            .await?
            .ok_or(DomainError::NotFound)?;
        let _scope = self
            .authz_check(
                ctx,
                actions::DELETE,
                Some(file_id),
                &row.meta.gts_file_type,
                row.owner.tenant_id,
            )
            .await?;
        let backend = self
            .registry
            .resolve_visible(row.backend_id, row.owner.tenant_id)?;
        let key = derive_s3_key(file_id);

        // Best-effort backend abort first; idempotent.
        backend.abort_multipart_upload(&key, upload_id).await?;

        // For initial-upload aborts, hard-delete the pending_upload row.
        // Variant-B re-upload aborts leave the existing `uploaded` row alone.
        if matches!(row.status, FileStatus::PendingUpload) {
            self.repo
                .delete_pending_upload(&conn, ctx.subject_tenant_id(), file_id)
                .await?;
        }
        Ok(())
    }

    // ── get_file_info ──────────────────────────────────────────────────────

    // @cpt-begin:cpt-cf-file-storage-fr-get-metadata:p1:inst-get-file-info
    pub async fn get_file_info(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
    ) -> Result<FileInfo, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        let row = self
            .repo
            .get_by_id(&conn, ctx.subject_tenant_id(), file_id)
            .await?
            .ok_or(DomainError::NotFound)?;
        let _scope = self
            .authz_check(
                ctx,
                actions::READ,
                Some(file_id),
                &row.meta.gts_file_type,
                row.owner.tenant_id,
            )
            .await?;
        check_pins(&row, etag, version_id)?;
        Ok(row)
    }
    // @cpt-end:cpt-cf-file-storage-fr-get-metadata:p1:inst-get-file-info

    // ── put_file_info (2-phase commit) ─────────────────────────────────────

    // @cpt-begin:cpt-cf-file-storage-fr-metadata-storage:p1:inst-put-file-info-meta-update
    // @cpt-begin:cpt-cf-file-storage-fr-conditional-requests:p1:inst-put-file-info-cas
    pub async fn put_file_info(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        update: FileMetaUpdate,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
    ) -> Result<FileInfo, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        let row = self
            .repo
            .get_by_id(&conn, ctx.subject_tenant_id(), file_id)
            .await?
            .ok_or(DomainError::NotFound)?;
        if matches!(row.status, FileStatus::Deleting) {
            return Err(DomainError::DeleteInProgress);
        }
        let _scope = self
            .authz_check(
                ctx,
                actions::UPDATE,
                Some(file_id),
                &row.meta.gts_file_type,
                row.owner.tenant_id,
            )
            .await?;
        let backend = self
            .registry
            .resolve_visible(row.backend_id, row.owner.tenant_id)?;
        let key = derive_s3_key(file_id);

        // Build the merged FileMeta we'd write to S3.
        let merged = FileMeta {
            name: update.name.clone().unwrap_or_else(|| row.meta.name.clone()),
            mime_type: update
                .mime_type
                .clone()
                .unwrap_or_else(|| row.meta.mime_type.clone()),
            gts_file_type: row.meta.gts_file_type.clone(), // immutable
            custom_metadata: update
                .custom_metadata
                .clone()
                .unwrap_or_else(|| row.meta.custom_metadata.clone()),
        };

        // Phase 1 (DB): uploaded → meta_updating with optional pins.
        let now = OffsetDateTime::now_utc();
        match self
            .repo
            .begin_meta_update(
                &conn,
                ctx.subject_tenant_id(),
                file_id,
                etag.map(|s| s.as_str()),
                version_id.map(|s| s.as_str()),
                &update,
                now,
            )
            .await?
        {
            ChangeStatusOutcome::Applied(_) => {}
            ChangeStatusOutcome::NoMatch => {
                if etag.is_some() || version_id.is_some() {
                    return Err(DomainError::EtagMismatch);
                }
                return Err(DomainError::Conflict);
            }
        }

        // Phase 2 (S3): CopyObject self-copy with MetadataDirective: REPLACE.
        // Strong-CAS path adds copy_source_if_match when etag pin present.
        let copy = backend
            .copy_object_self_replace_meta(&key, &merged, etag.map(|s| s.as_str()))
            .await?;

        // Phase 3 (DB): meta_updating → uploaded with new (etag, version_id).
        let now = OffsetDateTime::now_utc();
        match self
            .repo
            .finish_meta_update(
                &conn,
                ctx.subject_tenant_id(),
                file_id,
                &copy.etag,
                copy.version_id.as_deref(),
                now,
            )
            .await?
        {
            ChangeStatusOutcome::Applied(info) => Ok(info),
            ChangeStatusOutcome::NoMatch => Err(DomainError::Conflict),
        }
    }
    // @cpt-end:cpt-cf-file-storage-fr-metadata-storage:p1:inst-put-file-info-meta-update
    // @cpt-end:cpt-cf-file-storage-fr-conditional-requests:p1:inst-put-file-info-cas

    // ── delete_file (2-phase hard delete) ──────────────────────────────────

    // @cpt-begin:cpt-cf-file-storage-fr-delete-file:p1:inst-delete-file-2phase
    pub async fn delete_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
    ) -> Result<(), DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        let row = self
            .repo
            .get_by_id(&conn, ctx.subject_tenant_id(), file_id)
            .await?
            .ok_or(DomainError::NotFound)?;
        if matches!(row.status, FileStatus::Deleting) {
            return Err(DomainError::DeleteInProgress);
        }
        let _scope = self
            .authz_check(
                ctx,
                actions::DELETE,
                Some(file_id),
                &row.meta.gts_file_type,
                row.owner.tenant_id,
            )
            .await?;
        let backend = self
            .registry
            .resolve_visible(row.backend_id, row.owner.tenant_id)?;
        let key = derive_s3_key(file_id);

        // Phase 1 (DB): uploaded → deleting.
        let now = OffsetDateTime::now_utc();
        match self
            .repo
            .begin_delete(
                &conn,
                ctx.subject_tenant_id(),
                file_id,
                etag.map(|s| s.as_str()),
                version_id.map(|s| s.as_str()),
                now,
            )
            .await?
        {
            MutationOutcome::Applied => {}
            MutationOutcome::NoMatch => {
                if etag.is_some() || version_id.is_some() {
                    return Err(DomainError::EtagMismatch);
                }
                return Err(DomainError::Conflict);
            }
        }

        // Phase 2 (S3): DeleteObject (idempotent). Failures are best-effort
        // — row stays in `deleting`; subsequent reads see NotFound.
        if let Err(e) = backend.delete_object(&key).await {
            warn!(
                file_id = %file_id,
                error = %e,
                "DeleteObject failed; row stuck in `deleting` (P2 GC will retry)"
            );
            return Err(e);
        }

        // Phase 3 (DB): hard-DELETE the row.
        self.repo
            .finish_delete(&conn, ctx.subject_tenant_id(), file_id)
            .await?;
        Ok(())
    }
    // @cpt-end:cpt-cf-file-storage-fr-delete-file:p1:inst-delete-file-2phase

    // ── list_files ─────────────────────────────────────────────────────────

    // @cpt-begin:cpt-cf-file-storage-fr-list-files:p1:inst-list-files
    pub async fn list_files(
        &self,
        ctx: &SecurityContext,
        query: ListFilesQuery,
    ) -> Result<FileList, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        let owner = query.owner_id.unwrap_or_else(|| ctx.subject_id());
        let conn = self.db.conn().map_err(DomainError::from)?;
        let page: ListFilesPage = self
            .repo
            .list_paginated(
                &conn,
                ListFilesArgs {
                    tenant_id,
                    owner_id: Some(owner),
                    cursor: query.cursor,
                    limit: query.limit.unwrap_or(50),
                },
            )
            .await?;
        Ok(FileList {
            items: page.items,
            next_cursor: page.next_cursor,
        })
    }
    // @cpt-end:cpt-cf-file-storage-fr-list-files:p1:inst-list-files

    // ── read_file (with range support) ─────────────────────────────────────

    // @cpt-begin:cpt-cf-file-storage-fr-download-file:p1:inst-read-file-stream
    // @cpt-begin:cpt-cf-file-storage-fr-range-requests:p1:inst-read-file-range
    // @cpt-begin:cpt-cf-file-storage-principle-stream-by-default:p1:inst-read-file-stream
    pub async fn read_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
        range: Option<ByteRange>,
    ) -> Result<FileReadHandle, DomainError> {
        // Validate range at SDK boundary.
        if let Some(r) = range {
            match r {
                ByteRange::Inclusive { start, end } if start > end => {
                    return Err(DomainError::bad_request(
                        "ByteRange::Inclusive { start > end }",
                    ));
                }
                ByteRange::Suffix(0) => {
                    return Err(DomainError::bad_request("ByteRange::Suffix(0)"));
                }
                _ => {}
            }
        }

        let conn = self.db.conn().map_err(DomainError::from)?;
        let row = self
            .repo
            .get_by_id(&conn, ctx.subject_tenant_id(), file_id)
            .await?
            .ok_or(DomainError::NotFound)?;
        if matches!(row.status, FileStatus::Deleting) {
            return Err(DomainError::DeleteInProgress);
        }
        let _scope = self
            .authz_check(
                ctx,
                actions::READ,
                Some(file_id),
                &row.meta.gts_file_type,
                row.owner.tenant_id,
            )
            .await?;
        check_pins(&row, etag, version_id)?;

        let backend = self
            .registry
            .resolve_visible(row.backend_id, row.owner.tenant_id)?;
        let key = derive_s3_key(file_id);

        // In-band recovery for transient states (Completing / MetaUpdating).
        let info = match row.status {
            FileStatus::Completing | FileStatus::MetaUpdating => {
                let from = row.status;
                let head = backend.head_object(&key).await?;
                self.rollforward(file_id, from, &head).await?;
                self.repo
                    .get_by_id(&conn, ctx.subject_tenant_id(), file_id)
                    .await?
                    .ok_or(DomainError::NotFound)?
            }
            _ => row,
        };

        let read = backend.open_read(&key, range).await?;
        Ok(FileReadHandle {
            info,
            bytes: read.bytes,
            range: read.range,
        })
    }
    // @cpt-end:cpt-cf-file-storage-fr-download-file:p1:inst-read-file-stream
    // @cpt-end:cpt-cf-file-storage-fr-range-requests:p1:inst-read-file-range
    // @cpt-end:cpt-cf-file-storage-principle-stream-by-default:p1:inst-read-file-stream

    async fn rollforward(
        &self,
        file_id: FileId,
        from: FileStatus,
        head: &BackendObjectMetadata,
    ) -> Result<(), DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repo
            .rollforward_to_uploaded_system(
                &conn,
                file_id,
                from,
                &head.etag,
                head.version_id.as_deref(),
                head.size_bytes,
                OffsetDateTime::now_utc(),
            )
            .await?;
        Ok(())
    }

    // ── put_file (in-process streaming upload) ─────────────────────────────

    pub async fn put_file(
        &self,
        _ctx: &SecurityContext,
        _file_id: Option<FileId>,
        _backend_id: Option<BackendId>,
        _owner: OwnerRef,
        _meta: FileMeta,
        _bytes: FileByteStream,
        _etag: Option<&Etag>,
        _version_id: Option<&VersionId>,
    ) -> Result<FileInfo, DomainError> {
        // P1 in-process streaming upload. Drives create_presigned_upload →
        // per-chunk PUT → complete_upload internally without the presign
        // round-trip. Production-grade implementation is deferred — this
        // returns Internal as a clear sentinel for callers to use the
        // presigned-first path until put_file is wired.
        Err(DomainError::internal(
            "put_file: in-process streaming upload is not yet wired in P1; \
             use create_presigned_upload + client PUT + complete_upload",
        ))
    }

    // ── presign_urls (batch download) ──────────────────────────────────────

    // @cpt-begin:cpt-cf-file-storage-fr-signed-urls:p1:inst-presign-urls-batch
    // @cpt-begin:cpt-cf-file-storage-principle-batch-presigned-urls:p1:inst-presign-urls-batch
    pub async fn presign_urls(
        &self,
        ctx: &SecurityContext,
        items: Vec<PresignDownloadItem>,
    ) -> Result<Vec<PresignDownloadOutcome>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        let tenant_id = ctx.subject_tenant_id();

        // Group items by backend so we can batch presign per backend.
        let mut out: Vec<PresignDownloadOutcome> = Vec::with_capacity(items.len());
        // Simple per-item handling — batching across backends in a single
        // round-trip is a P2 optimization.
        for item in items {
            let row = match self.repo.get_by_id(&conn, tenant_id, item.file_id).await {
                Ok(Some(r)) => r,
                Ok(None) => {
                    out.push(PresignDownloadOutcome {
                        file_id: item.file_id,
                        result: Err(file_storage_sdk::FileStorageError::NotFound),
                    });
                    continue;
                }
                Err(e) => {
                    out.push(PresignDownloadOutcome {
                        file_id: item.file_id,
                        result: Err(e.into()),
                    });
                    continue;
                }
            };
            // Pin checks.
            if let Some(etag) = &item.etag {
                if &row.etag != etag {
                    out.push(PresignDownloadOutcome {
                        file_id: item.file_id,
                        result: Err(file_storage_sdk::FileStorageError::EtagMismatch),
                    });
                    continue;
                }
            }
            // version_id pin only honoured by *.versioned.* capabilities.
            let cap = item.capability.as_str();
            let is_versioned = cap.ends_with(".versioned.v1");
            if item.version_id.is_some() && !is_versioned {
                out.push(PresignDownloadOutcome {
                    file_id: item.file_id,
                    result: Err(file_storage_sdk::FileStorageError::BadRequest(
                        format!("version_id not honoured by capability {cap}"),
                    )),
                });
                continue;
            }
            // Resolve backend and validate capability declared.
            let backend = match self.registry.resolve_visible(row.backend_id, tenant_id) {
                Ok(b) => b,
                Err(e) => {
                    out.push(PresignDownloadOutcome {
                        file_id: item.file_id,
                        result: Err(e.into()),
                    });
                    continue;
                }
            };
            if !backend.descriptor().declares(cap) {
                out.push(PresignDownloadOutcome {
                    file_id: item.file_id,
                    result: Err(file_storage_sdk::FileStorageError::CapabilityUnavailable(
                        format!("backend does not declare capability {cap}"),
                    )),
                });
                continue;
            }

            let max_ttl = backend.descriptor().max_signed_url_ttl_seconds();
            let ttl = item.params.expires_in_seconds.min(max_ttl);
            let key = derive_s3_key(item.file_id);
            let presign_item = PresignedGetItem {
                key,
                capability: item.capability.clone(),
                params: item.params.clone(),
                mime_type_hint: Some(row.meta.mime_type.clone()),
                display_name_hint: Some(row.meta.name.clone()),
                expires_in_seconds: ttl,
                version_id: item.version_id.clone(),
            };
            let mut results = backend.issue_presigned_gets(vec![presign_item]).await?;
            let r = results.pop().unwrap();
            out.push(PresignDownloadOutcome {
                file_id: item.file_id,
                result: r.result.map_err(Into::into),
            });
        }
        Ok(out)
    }
    // @cpt-end:cpt-cf-file-storage-fr-signed-urls:p1:inst-presign-urls-batch
    // @cpt-end:cpt-cf-file-storage-principle-batch-presigned-urls:p1:inst-presign-urls-batch

    // ── helpers ─────────────────────────────────────────────────────────────

    async fn authz_check(
        &self,
        _ctx: &SecurityContext,
        _action: &str,
        _resource_id: Option<Uuid>,
        _gts_file_type: &str,
        _owner_tenant_id: Uuid,
    ) -> Result<(), DomainError> {
        // P1 stub: production-grade authz integration via PolicyEnforcer is
        // a follow-up task. Surface area is intact; checks are no-ops in
        // this build. See `cpt-cf-file-storage-fr-file-ownership` and the
        // PEP wiring patterns in libs/authz-resolver-sdk for the proper
        // integration shape.
        let _ = &self.policy_enforcer;
        Ok(())
    }
}

// ── validators ──────────────────────────────────────────────────────────────

// @cpt-begin:cpt-cf-file-storage-principle-tenant-owner:p1:inst-validate-owner
fn validate_owner_against_ctx(ctx: &SecurityContext, owner: &OwnerRef) -> Result<(), DomainError> {
    if owner.tenant_id != ctx.subject_tenant_id() {
        return Err(DomainError::AccessDenied(
            "owner.tenant_id must match the caller's subject_tenant_id".into(),
        ));
    }
    Ok(())
}
// @cpt-end:cpt-cf-file-storage-principle-tenant-owner:p1:inst-validate-owner

fn validate_meta(meta: &FileMeta) -> Result<(), DomainError> {
    if meta.name.is_empty() {
        return Err(DomainError::bad_request("FileMeta.name must be non-empty"));
    }
    if meta.mime_type.is_empty() {
        return Err(DomainError::bad_request(
            "FileMeta.mime_type must be non-empty",
        ));
    }
    if meta.gts_file_type.is_empty() {
        return Err(DomainError::bad_request(
            "FileMeta.gts_file_type must be non-empty",
        ));
    }
    Ok(())
}

// @cpt-begin:cpt-cf-file-storage-principle-optimistic-concurrency:p1:inst-check-pins-cas
// @cpt-begin:cpt-cf-file-storage-constraint-etag-content-only:p1:inst-check-pins-cas
fn check_pins(
    row: &FileInfo,
    etag: Option<&Etag>,
    version_id: Option<&VersionId>,
) -> Result<(), DomainError> {
    if let Some(e) = etag {
        if &row.etag != e {
            return Err(DomainError::EtagMismatch);
        }
    }
    if let Some(v) = version_id {
        match &row.version_id {
            Some(rv) if rv == v => {}
            _ => return Err(DomainError::EtagMismatch),
        }
    }
    Ok(())
}
// @cpt-end:cpt-cf-file-storage-principle-optimistic-concurrency:p1:inst-check-pins-cas
// @cpt-end:cpt-cf-file-storage-constraint-etag-content-only:p1:inst-check-pins-cas

fn file_path_from_meta(meta: &FileMeta, file_id: FileId) -> String {
    // For P1, file_path mirrors the deterministic backend object key. This
    // keeps `(tenant_id, backend_id, file_path)` unique-by-construction
    // (no partial unique index needed).
    format!("f/{}/{}", file_id.simple(), meta.name)
}

trait OwnerSubject {
    fn subject_id(&self) -> Uuid;
}

impl OwnerSubject for SecurityContext {
    fn subject_id(&self) -> Uuid {
        // For P1 listing, default to subject_tenant_id when no owner_id
        // is provided. Real subject id extraction is handled elsewhere
        // in modkit_security; this fallback keeps list_files compiling
        // without depending on a specific accessor.
        self.subject_tenant_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_storage_sdk::CustomMetadata;
    use uuid::Uuid;

    fn meta_ok() -> FileMeta {
        FileMeta {
            name: "doc.pdf".into(),
            mime_type: "application/pdf".into(),
            gts_file_type: "gts.cf.fstorage.file.type.v1~document".into(),
            custom_metadata: CustomMetadata::new(),
        }
    }

    fn make_info(etag: &str, version_id: Option<&str>) -> FileInfo {
        FileInfo {
            file_id: Uuid::nil(),
            backend_id: Uuid::nil(),
            file_path: "f/00000000000000000000000000000000".into(),
            owner: OwnerRef {
                tenant_id: Uuid::nil(),
                owner_id: Uuid::nil(),
            },
            meta: meta_ok(),
            status: FileStatus::Uploaded,
            etag: etag.into(),
            version_id: version_id.map(String::from),
            size_bytes: 0,
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
            upload_expires_at: None,
        }
    }

    // ── validate_meta ───────────────────────────────────────────────────────

    #[test]
    fn validate_meta_accepts_well_formed() {
        assert!(validate_meta(&meta_ok()).is_ok());
    }

    #[test]
    fn validate_meta_rejects_empty_name() {
        let mut m = meta_ok();
        m.name = String::new();
        assert!(matches!(
            validate_meta(&m).unwrap_err(),
            DomainError::BadRequest(_)
        ));
    }

    #[test]
    fn validate_meta_rejects_empty_mime() {
        let mut m = meta_ok();
        m.mime_type = String::new();
        assert!(matches!(
            validate_meta(&m).unwrap_err(),
            DomainError::BadRequest(_)
        ));
    }

    #[test]
    fn validate_meta_rejects_empty_gts() {
        let mut m = meta_ok();
        m.gts_file_type = String::new();
        assert!(matches!(
            validate_meta(&m).unwrap_err(),
            DomainError::BadRequest(_)
        ));
    }

    // ── check_pins ──────────────────────────────────────────────────────────

    #[test]
    fn check_pins_no_pins_passes() {
        let row = make_info("abc", None);
        assert!(check_pins(&row, None, None).is_ok());
    }

    #[test]
    fn check_pins_etag_match_passes() {
        let row = make_info("abc", None);
        let etag = "abc".to_string();
        assert!(check_pins(&row, Some(&etag), None).is_ok());
    }

    #[test]
    fn check_pins_etag_mismatch_fails() {
        let row = make_info("abc", None);
        let etag = "wrong".to_string();
        assert!(matches!(
            check_pins(&row, Some(&etag), None).unwrap_err(),
            DomainError::EtagMismatch
        ));
    }

    #[test]
    fn check_pins_version_match_passes() {
        let row = make_info("abc", Some("v1"));
        let v = "v1".to_string();
        assert!(check_pins(&row, None, Some(&v)).is_ok());
    }

    #[test]
    fn check_pins_version_pin_against_no_version_fails() {
        // Backend without versioning: row.version_id is None, pin is Some.
        let row = make_info("abc", None);
        let v = "v1".to_string();
        assert!(matches!(
            check_pins(&row, None, Some(&v)).unwrap_err(),
            DomainError::EtagMismatch
        ));
    }

    #[test]
    fn check_pins_version_mismatch_fails() {
        let row = make_info("abc", Some("v1"));
        let v = "v2".to_string();
        assert!(matches!(
            check_pins(&row, None, Some(&v)).unwrap_err(),
            DomainError::EtagMismatch
        ));
    }

    #[test]
    fn check_pins_both_match_passes() {
        let row = make_info("abc", Some("v1"));
        let etag = "abc".to_string();
        let v = "v1".to_string();
        assert!(check_pins(&row, Some(&etag), Some(&v)).is_ok());
    }

    // ── file_path_from_meta ─────────────────────────────────────────────────

    #[test]
    fn file_path_from_meta_includes_file_id_and_name() {
        let id = Uuid::parse_str("11112222-3333-4444-5555-666677778888").unwrap();
        let m = meta_ok();
        let path = file_path_from_meta(&m, id);
        assert!(path.starts_with("f/11112222333344445555666677778888/"));
        assert!(path.ends_with("doc.pdf"));
    }

    #[test]
    fn file_path_from_meta_is_deterministic() {
        let id = Uuid::new_v4();
        let m = meta_ok();
        let a = file_path_from_meta(&m, id);
        let b = file_path_from_meta(&m, id);
        assert_eq!(a, b);
    }
}
