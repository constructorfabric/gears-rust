//! FileStorage service — orchestrates upload lifecycle, read/update, batch
//! presigned downloads, and the backend roster.

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use modkit_db::DBProvider;
use modkit_security::pep_properties;
use modkit_security::{AccessScope, SecurityContext};
use time::OffsetDateTime;
use tracing::{debug, warn};
use uuid::Uuid;

use file_storage_sdk::{
    Backend, BackendCapability, BackendId, Etag, FileByteStream, FileId, FileInfo, FileList,
    FileMeta, FileMetaUpdate, FileReadHandle, FileStatus, ListFilesQuery, OwnerRef,
    PresignDownloadItem, PresignDownloadOutcome, PresignedDownload, PresignedUploadHandle,
    UrlParams,
};

use crate::config::FileStorageConfig;
use crate::infra::backends::registry::BackendRegistry;
use crate::infra::backends::r#trait::{
    BackendReadResult, PresignedGetItem, PresignedGetOutcome, derive_s3_key,
};

use super::error::DomainError;
use super::etag::compose;
use super::repo::{
    ChangeStatusOutcome, DeleteOutcome, FilesRepo, InsertPendingArgs, ListFilesArgs,
    MutationOutcome,
};
use super::self_heal;

pub(crate) type DbProvider = DBProvider<modkit_db::DbError>;

pub(crate) const FILE_STORAGE_RESOURCE: ResourceType = ResourceType {
    name: "file-storage.file",
    supported_properties: &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
};

pub(crate) mod actions {
    pub const CREATE: &str = "create";
    pub const READ: &str = "read";
    pub const UPDATE: &str = "update";
    pub const DELETE: &str = "delete";
}

pub(crate) const PROP_GTS_FILE_TYPE: &str = "gts_file_type";

/// Orphan-delete queue entry. Pushed by `delete_file` (and the supersession
/// transaction in `change_status`); drained by the background worker.
#[derive(Debug, Clone)]
pub struct OrphanEntry {
    pub backend_id: BackendId,
    pub file_id: Uuid,
    pub eligible_at: OffsetDateTime,
}

pub type OrphanQueue = Arc<tokio::sync::Mutex<std::collections::VecDeque<OrphanEntry>>>;

pub struct Service<R: FilesRepo + 'static> {
    db: Arc<DbProvider>,
    repo: Arc<R>,
    policy_enforcer: PolicyEnforcer,
    config: Arc<FileStorageConfig>,
    registry: Arc<BackendRegistry>,
    orphan_queue: OrphanQueue,
}

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

    pub async fn list_backends(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Backend>, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        Ok(self.registry.list_visible_to_tenant(tenant_id))
    }

    // ── Upload lifecycle ────────────────────────────────────────────────────

    pub async fn create_presigned_url(
        &self,
        ctx: &SecurityContext,
        backend_id: Option<BackendId>,
        owner: OwnerRef,
        file_path: &str,
        meta: FileMeta,
        params: UrlParams,
    ) -> Result<PresignedUploadHandle, DomainError> {
        validate_owner_against_ctx(ctx, &owner)?;
        validate_meta(&meta)?;
        validate_file_path(file_path)?;

        let _scope = self
            .authz_check(ctx, actions::CREATE, None, &meta.gts_file_type, owner.tenant_id)
            .await?;

        // Resolve the backend (default-private fallback).
        let resolved_id = match backend_id {
            Some(id) => id,
            None => self.config.default_private_storage_id.ok_or_else(|| {
                DomainError::bad_request(
                    "no backend selected and default_private_storage_id is unset",
                )
            })?,
        };
        let backend = self
            .registry
            .resolve_visible(resolved_id, owner.tenant_id)?;

        if let (Some(size), Some(max)) = (meta.size_bytes, backend.descriptor().max_file_size_bytes()) {
            if size > max {
                return Err(DomainError::PayloadTooLarge { max_bytes: max });
            }
        }

        if !backend
            .descriptor()
            .capabilities()
            .contains(&BackendCapability::PresignedUrls)
        {
            return Err(DomainError::capability(
                "backend does not declare PresignedUrls capability",
            ));
        }

        let file_id = Uuid::new_v4();
        let backend_object_key = derive_s3_key(file_id);

        let etag_pinned = compose("", 0);

        let now = OffsetDateTime::now_utc();
        let max_ttl_secs = backend.descriptor().max_signed_url_ttl_seconds();
        let requested_ttl_secs = params.expires_in_seconds.min(max_ttl_secs);
        let expires_at = now + time::Duration::seconds(
            i64::try_from(requested_ttl_secs).unwrap_or(i64::MAX),
        );

        if params.refresh_etag
            && backend
                .descriptor()
                .capabilities()
                .contains(&BackendCapability::PresignedConditionalPut)
        {
            debug!("refresh_etag flag set on initial create — no row to repair");
        }

        let custom_metadata_json =
            serde_json::to_string(&meta.custom_metadata).map_err(|e| {
                DomainError::internal(format!("custom_metadata serialisation failed: {e}"))
            })?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        let inserted = self
            .repo
            .insert_pending(
                &conn,
                InsertPendingArgs {
                    file_id,
                    tenant_id: owner.tenant_id,
                    backend_id: resolved_id,
                    file_path: file_path.to_owned(),
                    owner_id: owner.owner_id,
                    name: meta.name.clone(),
                    gts_file_type: meta.gts_file_type.clone(),
                    mime_type: meta.mime_type.clone(),
                    etag_pinned: etag_pinned.clone(),
                    upload_expires_at: Some(expires_at),
                    custom_metadata_json,
                    now,
                },
            )
            .await?;

        let upload_url = backend
            .issue_presigned_put(
                &backend_object_key,
                &meta,
                &params,
                &etag_pinned,
                requested_ttl_secs,
            )
            .await?;

        Ok(PresignedUploadHandle {
            file_id: inserted.file_id,
            upload_url,
            etag_pinned,
            expires_at,
        })
    }

    pub async fn change_status(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        target: FileStatus,
        old_etag: Etag,
        new_etag: Etag,
    ) -> Result<FileInfo, DomainError> {
        if target != FileStatus::Uploaded {
            return Err(DomainError::InvalidStatusTransition(format!(
                "target {target:?} is not allowed in P1 (only Uploaded)"
            )));
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

        let now = OffsetDateTime::now_utc();
        let outcome = self
            .repo
            .change_status_with_supersession(
                &conn,
                row.owner.tenant_id,
                file_id,
                &old_etag,
                target,
                &new_etag,
                now,
            )
            .await?;

        match outcome {
            ChangeStatusOutcome::Applied(info) => Ok(info),
            ChangeStatusOutcome::NoMatch => {
                let current = self
                    .repo
                    .get_by_id(&conn, ctx.subject_tenant_id(), file_id)
                    .await?
                    .ok_or(DomainError::NotFound)?;

                let candidate = compose(&new_etag, 0);
                if current.etag == candidate {
                    return Ok(current);
                }
                Err(DomainError::EtagMismatch)
            }
        }
    }

    // ── Read & update ───────────────────────────────────────────────────────

    pub async fn get_file_info(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
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

        if let Some(pinned) = etag {
            if *pinned != row.etag {
                return Err(DomainError::EtagMismatch);
            }
        }
        Ok(row)
    }

    pub async fn put_file_info(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        update: FileMetaUpdate,
        etag: Etag,
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
                actions::UPDATE,
                Some(file_id),
                &row.meta.gts_file_type,
                row.owner.tenant_id,
            )
            .await?;

        let now = OffsetDateTime::now_utc();
        let outcome = self
            .repo
            .update_metadata_etag_conditional(
                &conn,
                ctx.subject_tenant_id(),
                file_id,
                &etag,
                &update,
                None,
                now,
            )
            .await?;

        match outcome {
            ChangeStatusOutcome::Applied(info) => Ok(info),
            ChangeStatusOutcome::NoMatch => Err(DomainError::EtagMismatch),
        }
    }

    pub async fn delete_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Etag,
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

        let DeleteOutcome { outcome, backend_id } = self
            .repo
            .delete_etag_conditional(&conn, ctx.subject_tenant_id(), file_id, &etag)
            .await?;

        match outcome {
            MutationOutcome::Applied => {
                if let Some(bid) = backend_id {
                    let eligible_at = OffsetDateTime::now_utc()
                        + time::Duration::seconds(
                            i64::try_from(self.config.orphan_delete_grace_seconds)
                                .unwrap_or(i64::MAX),
                        );
                    self.orphan_queue.lock().await.push_back(OrphanEntry {
                        backend_id: bid,
                        file_id,
                        eligible_at,
                    });
                }
                Ok(())
            }
            MutationOutcome::NoMatch => Err(DomainError::EtagMismatch),
        }
    }

    pub async fn list_files(
        &self,
        ctx: &SecurityContext,
        query: ListFilesQuery,
    ) -> Result<FileList, DomainError> {
        let _scope = self
            .authz_check_no_resource(ctx, actions::READ, ctx.subject_tenant_id())
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        // Default to caller's subject_id when no owner_id filter is set.
        let owner_id = query.owner_id.or(Some(ctx.subject_id()));

        let limit = query.limit.unwrap_or(50).min(200);
        let page = self
            .repo
            .list_paginated(
                &conn,
                ListFilesArgs {
                    tenant_id: ctx.subject_tenant_id(),
                    owner_id,
                    backend_id: query.backend_id,
                    mime_type: query.mime_type,
                    gts_file_type: query.gts_file_type,
                    created_after: query.created_after,
                    created_before: query.created_before,
                    cursor: query.cursor,
                    limit,
                },
            )
            .await?;

        Ok(FileList {
            items: page.items,
            next_cursor: page.next_cursor,
        })
    }

    pub async fn read_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
    ) -> Result<FileReadHandle, DomainError> {
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

        if row.status != FileStatus::Uploaded {
            return Err(DomainError::NotFound);
        }

        let backend = self
            .registry
            .resolve_visible(row.backend_id, row.owner.tenant_id)?;

        let persistence = self
            .repo
            .get_persistence_fields(&conn, row.file_id)
            .await?
            .ok_or(DomainError::NotFound)?;
        let backend_object_key = derive_s3_key(row.file_id);
        let BackendReadResult {
            bytes,
            content_hash,
        } = backend.open_read(&backend_object_key).await?;

        let outcome = self_heal::sync_etag_from_backend(
            &*self.repo,
            &conn,
            file_id,
            &row.etag,
            &content_hash,
            persistence.meta_revision,
        )
        .await?;

        let final_info = match outcome {
            self_heal::SelfHealOutcome::AlreadyConsistent => row,
            self_heal::SelfHealOutcome::Repaired { derived } => {
                if let Some(pinned) = etag {
                    if *pinned != derived {
                        return Err(DomainError::EtagMismatch);
                    }
                }
                self.repo
                    .get_by_id(&conn, ctx.subject_tenant_id(), file_id)
                    .await?
                    .ok_or(DomainError::NotFound)?
            }
            self_heal::SelfHealOutcome::Raced => {
                self.repo
                    .get_by_id(&conn, ctx.subject_tenant_id(), file_id)
                    .await?
                    .ok_or(DomainError::NotFound)?
            }
        };

        if let Some(pinned) = etag {
            if *pinned != final_info.etag {
                return Err(DomainError::EtagMismatch);
            }
        }

        Ok(FileReadHandle {
            info: final_info,
            bytes,
        })
    }

    /// In-process SDK only — gated behind `unimplemented!` in P1.
    /// Per rust-traits.md §SDK Traits, the trait shape is satisfied by
    /// declaring this method even though the streaming PUT body is not
    /// wired in P1.
    pub async fn put_file(
        &self,
        _ctx: &SecurityContext,
        _backend_id: Option<BackendId>,
        _owner: OwnerRef,
        _file_path: &str,
        _meta: FileMeta,
        _bytes: FileByteStream,
        _etag: Option<&Etag>,
    ) -> Result<FileInfo, DomainError> {
        unimplemented!("put_file in P1: see DESIGN §3.x — drives presign+PUT+commit in-process")
    }

    // ── Batch presign downloads ─────────────────────────────────────────────

    pub async fn presign_urls(
        &self,
        ctx: &SecurityContext,
        items: Vec<PresignDownloadItem>,
    ) -> Result<Vec<PresignDownloadOutcome>, DomainError> {
        let mut outcomes = Vec::with_capacity(items.len());

        for item in items {
            let result = self.presign_one(ctx, &item).await;
            outcomes.push(PresignDownloadOutcome {
                file_id: item.file_id,
                result: result.map_err(Into::into),
            });
        }

        Ok(outcomes)
    }

    async fn presign_one(
        &self,
        ctx: &SecurityContext,
        item: &PresignDownloadItem,
    ) -> Result<PresignedDownload, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        let row = self
            .repo
            .get_by_id(&conn, ctx.subject_tenant_id(), item.file_id)
            .await?
            .ok_or(DomainError::NotFound)?;

        if row.status != FileStatus::Uploaded {
            return Err(DomainError::NotFound);
        }

        if let Some(pinned) = &item.etag {
            if *pinned != row.etag {
                return Err(DomainError::EtagMismatch);
            }
        }

        let _scope = self
            .authz_check(
                ctx,
                actions::READ,
                Some(item.file_id),
                &row.meta.gts_file_type,
                row.owner.tenant_id,
            )
            .await?;

        let backend = self
            .registry
            .resolve_visible(row.backend_id, row.owner.tenant_id)?;

        let backend_object_key = derive_s3_key(row.file_id);
        let max_ttl_secs = backend.descriptor().max_signed_url_ttl_seconds();
        let requested_ttl_secs = item.params.expires_in_seconds.min(max_ttl_secs);

        let mut outcomes = backend
            .issue_presigned_gets(vec![PresignedGetItem {
                key: backend_object_key,
                params: item.params.clone(),
                mime_type_hint: Some(row.meta.mime_type.clone()),
                display_name_hint: Some(row.meta.name.clone()),
                expires_in_seconds: requested_ttl_secs,
            }])
            .await?;

        let outcome = outcomes.pop().ok_or_else(|| {
            DomainError::internal("presigned-get backend returned empty outcomes vector")
        })?;
        match outcome {
            PresignedGetOutcome { result, .. } => result,
        }
    }

    // ── AuthZ helpers ───────────────────────────────────────────────────────

    async fn authz_check(
        &self,
        ctx: &SecurityContext,
        action: &'static str,
        resource_id: Option<Uuid>,
        gts_file_type: &str,
        owner_tenant_id: Uuid,
    ) -> Result<AccessScope, DomainError> {
        let req = AccessRequest::new()
            .resource_property(pep_properties::OWNER_TENANT_ID, owner_tenant_id)
            .resource_property(PROP_GTS_FILE_TYPE, gts_file_type);
        let scope = self
            .policy_enforcer
            .access_scope_with(ctx, &FILE_STORAGE_RESOURCE, action, resource_id, &req)
            .await?;
        Ok(scope)
    }

    async fn authz_check_no_resource(
        &self,
        ctx: &SecurityContext,
        action: &'static str,
        owner_tenant_id: Uuid,
    ) -> Result<AccessScope, DomainError> {
        let req = AccessRequest::new()
            .resource_property(pep_properties::OWNER_TENANT_ID, owner_tenant_id);
        let scope = self
            .policy_enforcer
            .access_scope_with(ctx, &FILE_STORAGE_RESOURCE, action, None, &req)
            .await?;
        Ok(scope)
    }
}

// ── Validation helpers ──────────────────────────────────────────────────────

fn validate_owner_against_ctx(
    ctx: &SecurityContext,
    owner: &OwnerRef,
) -> Result<(), DomainError> {
    if owner.tenant_id != ctx.subject_tenant_id() {
        return Err(DomainError::AccessDenied(
            "owner.tenant_id must match the caller's tenant".to_owned(),
        ));
    }
    Ok(())
}

fn validate_meta(meta: &FileMeta) -> Result<(), DomainError> {
    if meta.name.is_empty() {
        return Err(DomainError::bad_request("FileMeta.name must not be empty"));
    }
    if meta.mime_type.is_empty() {
        return Err(DomainError::bad_request("FileMeta.mime_type must not be empty"));
    }
    if !meta.gts_file_type.starts_with("gts.") {
        return Err(DomainError::bad_request(
            "FileMeta.gts_file_type must start with 'gts.'",
        ));
    }
    if meta.name.len() > 512 {
        return Err(DomainError::bad_request("FileMeta.name exceeds 512 chars"));
    }
    if meta.mime_type.len() > 256 {
        return Err(DomainError::bad_request("FileMeta.mime_type exceeds 256 chars"));
    }
    if meta.gts_file_type.len() > 256 {
        return Err(DomainError::bad_request("FileMeta.gts_file_type exceeds 256 chars"));
    }
    Ok(())
}

fn validate_file_path(path: &str) -> Result<(), DomainError> {
    if path.is_empty() {
        return Err(DomainError::bad_request("file_path must not be empty"));
    }
    if path.len() > 1024 {
        return Err(DomainError::bad_request("file_path exceeds 1024 chars"));
    }
    Ok(())
}

#[allow(dead_code)]
fn _unused_warn_marker() {
    warn!("placeholder");
}
