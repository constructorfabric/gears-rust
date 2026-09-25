//! In-process adapter implementing the SDK client trait.
//!
//! Level 1 SDK: every method below calls the exact same service method the
//! matching REST handler in `api::rest::handlers` calls (same
//! `SecurityContext`, so the same authorization decisions, audit rows, and
//! idempotency behavior), then maps the domain result into the SDK's model
//! types (`domain::sdk_convert`). Where a handler does more than a single
//! service call — the `POST /files` create/multipart-plan branching, the
//! `GET /files` batched custom-metadata attachment, and the
//! `GET .../versions` manifest-budget attachment — that extra logic lives in
//! a shared place both the handler and this client call
//! (`domain::create_flow`, `FileService::list_files_with_metadata`,
//! `FileService::list_versions_with_manifests`) rather than being duplicated
//! here.

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage_sdk::{
    CreateFileOutcome, CustomMetadataPatch, EffectivePolicy, FileFetch, FileId, FileRecord,
    FileStorageClientV1, FileStorageError, MultipartCompleteOutcome, MultipartIntent,
    MultipartPlan, MultipartStatus, NewFile, OwnerFilter, OwnerKind, Page, Policy, PolicyBody,
    PolicyScope, RetentionRule, RetentionRuleBody, RetentionScope, Storage, UploadTicket,
    VersionId, VersionRecord,
};

use crate::domain::create_flow;
use crate::domain::etag;
use crate::domain::multipart_service::MultipartService;
use crate::domain::policy_service::PolicyService;
use crate::domain::sdk_convert;
use crate::domain::service::FileService;

/// Local (same-process) implementation of [`FileStorageClientV1`], resolved
/// from `ClientHub` by other gears. Holds the same three services `gear.rs`
/// wires the REST routes to, so both surfaces run identical business logic.
#[allow(unknown_lints, de0309_must_have_domain_model)]
pub struct FileStorageLocalClient {
    service: Arc<FileService>,
    multipart_service: Arc<MultipartService>,
    policy_service: Arc<PolicyService>,
}

impl FileStorageLocalClient {
    /// Create a new local client over the same service instances registered
    /// with the REST router.
    #[must_use]
    pub fn new(
        service: Arc<FileService>,
        multipart_service: Arc<MultipartService>,
        policy_service: Arc<PolicyService>,
    ) -> Self {
        Self {
            service,
            multipart_service,
            policy_service,
        }
    }
}

fn upload_ticket(t: crate::domain::service::UploadTicket) -> UploadTicket {
    UploadTicket {
        file_id: t.file_id,
        version_id: t.version_id,
        upload_url: t.upload_url,
    }
}

fn download_ticket(t: crate::domain::service::DownloadTicket) -> file_storage_sdk::DownloadTicket {
    file_storage_sdk::DownloadTicket {
        download_url: t.download_url,
        etag: t.etag,
        version_id: t.version_id,
    }
}

#[async_trait]
impl FileStorageClientV1 for FileStorageLocalClient {
    async fn create_file(
        &self,
        ctx: &SecurityContext,
        new: NewFile,
        idempotency_key: Option<String>,
        auto_bind: bool,
        multipart: Option<MultipartIntent>,
    ) -> Result<CreateFileOutcome, FileStorageError> {
        let intent = multipart.map(|m| create_flow::MultipartIntent {
            declared_size: m.declared_size,
            preferred_part_size: m.preferred_part_size,
            concurrency: m.concurrency,
        });
        let outcome = create_flow::create_file(
            &self.service,
            &self.multipart_service,
            ctx,
            new,
            idempotency_key,
            auto_bind,
            intent,
        )
        .await?;
        Ok(match outcome {
            create_flow::CreateFileOutcome::SinglePart(t) => {
                CreateFileOutcome::SinglePart(upload_ticket(t))
            }
            create_flow::CreateFileOutcome::Multipart { file_id, plan } => {
                CreateFileOutcome::Multipart {
                    file_id,
                    plan: sdk_convert::multipart_plan(plan),
                }
            }
        })
    }

    async fn get_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        if_none_match: Option<&str>,
    ) -> Result<FileFetch, FileStorageError> {
        let (file, custom_metadata) = self.service.get_file_with_metadata(ctx, file_id).await?;
        let current_etag = etag::etag_for(&file);
        // Shared with `api::rest::handlers::get_file` — see
        // `etag::if_none_match_satisfied`'s doc.
        if etag::if_none_match_satisfied(if_none_match, current_etag.as_deref()) {
            return Ok(FileFetch::NotModified);
        }
        Ok(FileFetch::Modified {
            record: Box::new(FileRecord {
                file,
                custom_metadata,
            }),
            etag: current_etag,
        })
    }

    async fn list_files(
        &self,
        ctx: &SecurityContext,
        owner: OwnerFilter,
        limit: Option<u64>,
        offset: u64,
    ) -> Result<Page<FileRecord>, FileStorageError> {
        // Shared with `api::rest::handlers::list_files` — same batched
        // custom-metadata attachment, see `FileService::list_files_with_metadata`.
        let items = self
            .service
            .list_files_with_metadata(ctx, owner, limit, offset)
            .await?
            .into_iter()
            .map(|(file, custom_metadata)| FileRecord {
                file,
                custom_metadata,
            })
            .collect::<Vec<_>>();
        Ok(Page::new(items, limit, offset))
    }

    async fn update_metadata(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        patch: CustomMetadataPatch,
        if_match_metadata: Option<i64>,
    ) -> Result<FileRecord, FileStorageError> {
        self.service
            .update_metadata(ctx, file_id, patch, if_match_metadata)
            .await?;
        // Re-read with metadata, exactly like `api::rest::handlers::update_metadata`
        // — the mutation's own return value carries no custom metadata.
        let (file, custom_metadata) = self.service.get_file_with_metadata(ctx, file_id).await?;
        Ok(FileRecord {
            file,
            custom_metadata,
        })
    }

    async fn delete_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        if_match: Option<&str>,
    ) -> Result<(), FileStorageError> {
        self.service.delete_file(ctx, file_id, if_match).await?;
        Ok(())
    }

    async fn download_url(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        version_id: Option<VersionId>,
    ) -> Result<file_storage_sdk::DownloadTicket, FileStorageError> {
        let ticket = self.service.download_url(ctx, file_id, version_id).await?;
        Ok(download_ticket(ticket))
    }

    async fn list_versions(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        limit: Option<u64>,
        offset: u64,
    ) -> Result<Page<VersionRecord>, FileStorageError> {
        // Shared with `api::rest::handlers::list_versions` — same
        // manifest-byte budget and truncation.
        let versions = self
            .service
            .list_versions_with_manifests(ctx, file_id, limit, offset)
            .await?;
        let items = versions
            .into_iter()
            .map(|(version, manifest)| VersionRecord { version, manifest })
            .collect();
        Ok(Page::new(items, limit, offset))
    }

    async fn presign_version(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
    ) -> Result<UploadTicket, FileStorageError> {
        let ticket = self.service.presign_version(ctx, file_id).await?;
        Ok(upload_ticket(ticket))
    }

    async fn bind(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        version_id: VersionId,
        if_match: Option<&str>,
    ) -> Result<FileRecord, FileStorageError> {
        self.service
            .bind(ctx, file_id, version_id, if_match)
            .await?;
        // Re-read with metadata, exactly like `api::rest::handlers::bind`.
        let (file, custom_metadata) = self.service.get_file_with_metadata(ctx, file_id).await?;
        Ok(FileRecord {
            file,
            custom_metadata,
        })
    }

    async fn delete_version(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        version_id: VersionId,
    ) -> Result<(), FileStorageError> {
        self.service
            .delete_version(ctx, file_id, version_id)
            .await?;
        Ok(())
    }

    async fn initiate_multipart(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        declared_mime: &str,
        declared_size: u64,
        preferred_part_size: Option<u64>,
        concurrency: Option<u32>,
    ) -> Result<MultipartPlan, FileStorageError> {
        let plan = self
            .multipart_service
            .initiate_multipart_upload(
                ctx,
                file_id,
                declared_mime,
                declared_size,
                preferred_part_size,
                concurrency,
                // Standalone initiate keeps the staged (manual-bind) behavior
                // — mirrors `api::rest::handlers::initiate_multipart`. The
                // merged create+plan path's `auto_bind` only applies via
                // `create_file`'s own `multipart` intent.
                false,
            )
            .await?;
        Ok(sdk_convert::multipart_plan(plan))
    }

    async fn introspect_multipart(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        upload_id: Uuid,
    ) -> Result<MultipartStatus, FileStorageError> {
        let status = self
            .multipart_service
            .introspect_multipart_upload(ctx, file_id, upload_id)
            .await?;
        Ok(sdk_convert::multipart_status(status))
    }

    async fn complete_multipart(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        upload_id: Uuid,
        if_match: Option<&str>,
    ) -> Result<MultipartCompleteOutcome, FileStorageError> {
        let outcome = self
            .multipart_service
            .complete_multipart_upload(ctx, file_id, upload_id, if_match)
            .await?;
        Ok(sdk_convert::multipart_complete_outcome(outcome))
    }

    async fn abort_multipart(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        upload_id: Uuid,
    ) -> Result<(), FileStorageError> {
        self.multipart_service
            .abort_multipart_upload(ctx, file_id, upload_id)
            .await?;
        Ok(())
    }

    async fn transfer_ownership(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        new_owner_kind: OwnerKind,
        new_owner_id: Uuid,
    ) -> Result<FileRecord, FileStorageError> {
        let (file, custom_metadata) = self
            .service
            .transfer_ownership(ctx, file_id, new_owner_kind, new_owner_id)
            .await?;
        Ok(FileRecord {
            file,
            custom_metadata,
        })
    }

    async fn migrate_backend(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        target_backend_id: &str,
    ) -> Result<(), FileStorageError> {
        self.service
            .migrate_backend(ctx, file_id, target_backend_id)
            .await?;
        Ok(())
    }

    async fn list_storages(&self, ctx: &SecurityContext) -> Result<Vec<Storage>, FileStorageError> {
        // Same authorization gate as `api::rest::handlers::list_storages` —
        // `list_backends` itself is a synchronous, authz-free lookup.
        self.service.authorize_backends_read(ctx).await?;
        Ok(self
            .service
            .list_backends()
            .into_iter()
            .map(|(id, caps)| sdk_convert::storage(id, caps))
            .collect())
    }

    async fn get_storage(
        &self,
        ctx: &SecurityContext,
        storage_id: &str,
    ) -> Result<Storage, FileStorageError> {
        self.service.authorize_backends_read(ctx).await?;
        let (id, caps) = self.service.get_backend(storage_id)?;
        Ok(sdk_convert::storage(id, caps))
    }

    async fn get_policy(
        &self,
        ctx: &SecurityContext,
        scope: PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<Option<Policy>, FileStorageError> {
        let stored = self
            .policy_service
            .get_own_policy(
                ctx,
                sdk_convert::policy_scope_to_domain(scope),
                scope_owner_id,
            )
            .await?;
        Ok(stored.map(sdk_convert::stored_policy))
    }

    async fn get_effective_policy(
        &self,
        ctx: &SecurityContext,
        user_owner_id: Option<Uuid>,
    ) -> Result<EffectivePolicy, FileStorageError> {
        let ep = self
            .policy_service
            .get_effective_policy(ctx, user_owner_id)
            .await?;
        Ok(sdk_convert::effective_policy(ep))
    }

    async fn put_policy(
        &self,
        ctx: &SecurityContext,
        scope: PolicyScope,
        scope_owner_id: Option<Uuid>,
        body: PolicyBody,
    ) -> Result<Policy, FileStorageError> {
        let stored = self
            .policy_service
            .set_policy(
                ctx,
                sdk_convert::policy_scope_to_domain(scope),
                scope_owner_id,
                sdk_convert::policy_body_to_domain(body),
            )
            .await?;
        Ok(sdk_convert::stored_policy(stored))
    }

    async fn list_retention_rules(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<RetentionRule>, FileStorageError> {
        let rules = self.policy_service.list_retention_rules(ctx).await?;
        Ok(rules
            .into_iter()
            .map(sdk_convert::stored_retention_rule)
            .collect())
    }

    async fn create_retention_rule(
        &self,
        ctx: &SecurityContext,
        scope: RetentionScope,
        scope_target_id: Option<Uuid>,
        body: RetentionRuleBody,
    ) -> Result<RetentionRule, FileStorageError> {
        let rule = self
            .policy_service
            .create_retention_rule(
                ctx,
                sdk_convert::retention_scope_to_domain(scope),
                scope_target_id,
                sdk_convert::retention_rule_body_to_domain(body),
            )
            .await?;
        Ok(sdk_convert::stored_retention_rule(rule))
    }

    async fn delete_retention_rule(
        &self,
        ctx: &SecurityContext,
        rule_id: Uuid,
    ) -> Result<(), FileStorageError> {
        let removed = self
            .policy_service
            .delete_retention_rule(ctx, rule_id)
            .await?;
        if removed {
            Ok(())
        } else {
            Err(crate::domain::error::DomainError::retention_rule_not_found(rule_id).into())
        }
    }
}

#[cfg(test)]
#[path = "local_client_tests.rs"]
mod local_client_tests;
