//! Trait implementations: [`CleanupStore`], [`MultipartStore`], [`PolicyStore`].
//!
//! Each impl is a thin delegation to the `Store` inherent methods defined in
//! the sibling files. This file exists purely to separate the boilerplate from
//! the intent-method implementations.

use time::OffsetDateTime;
use uuid::Uuid;

use file_storage_sdk::{CustomMetadataEntry, File, FileVersion};

use crate::domain::error::DomainError;
use crate::domain::ports::{CleanupStore, MultipartStore};
use crate::infra::storage::store::Store;
use async_trait::async_trait;

#[async_trait]
impl CleanupStore for Store {
    async fn list_abandoned_pending_versions(
        &self,
        older_than: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<Vec<FileVersion>, DomainError> {
        Store::list_abandoned_pending_versions(self, older_than, now).await
    }

    async fn delete_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: crate::domain::audit::AuditEntry,
    ) -> Result<bool, DomainError> {
        Store::delete_version(self, file_id, version_id, audit).await
    }

    async fn delete_pending_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: crate::domain::audit::AuditEntry,
    ) -> Result<bool, DomainError> {
        Store::delete_pending_version(self, file_id, version_id, audit).await
    }

    async fn list_expired_multipart_uploads(
        &self,
        now: OffsetDateTime,
    ) -> Result<Vec<crate::domain::multipart::MultipartUploadSession>, DomainError> {
        Store::list_expired_multipart_uploads(self, now).await
    }

    async fn abort_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: crate::domain::audit::AuditEntry,
    ) -> Result<bool, DomainError> {
        Store::abort_multipart_upload(self, upload_id, audit).await
    }

    async fn get_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<FileVersion>, DomainError> {
        Store::get_version(self, file_id, version_id).await
    }

    async fn list_all_retention_rules(
        &self,
    ) -> Result<Vec<crate::domain::policy::StoredRetentionRule>, DomainError> {
        Store::list_all_retention_rules(self).await
    }

    async fn list_all_files_for_sweep(
        &self,
        after: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<File>, DomainError> {
        Store::list_all_files_for_sweep(self, after, limit).await
    }

    async fn list_metadata(&self, file_id: Uuid) -> Result<Vec<CustomMetadataEntry>, DomainError> {
        Store::list_metadata(self, file_id).await
    }

    async fn list_versions(&self, file_id: Uuid) -> Result<Vec<FileVersion>, DomainError> {
        Store::list_versions(self, file_id).await
    }

    async fn get_file(&self, file_id: Uuid) -> Result<Option<File>, DomainError> {
        Store::get_file(self, &toolkit_security::AccessScope::allow_all(), file_id).await
    }

    async fn has_in_progress_multipart_for_file(&self, file_id: Uuid) -> Result<bool, DomainError> {
        Store::has_in_progress_multipart_for_file(self, file_id).await
    }

    async fn delete_file_with_event(
        &self,
        scope: &toolkit_security::AccessScope,
        file_id: Uuid,
        audit: crate::domain::audit::AuditEntry,
        event: Option<crate::domain::audit::FileEvent>,
    ) -> Result<bool, DomainError> {
        Store::delete_file_with_event(self, scope, file_id, audit, event).await
    }

    async fn delete_orphan_file_with_event(
        &self,
        file_id: Uuid,
        audit: crate::domain::audit::AuditEntry,
        event: Option<crate::domain::audit::FileEvent>,
    ) -> Result<bool, DomainError> {
        Store::delete_orphan_file_with_event(self, file_id, audit, event).await
    }

    async fn delete_expired_idempotency_keys(
        &self,
        now: OffsetDateTime,
    ) -> Result<u64, DomainError> {
        Store::delete_expired_idempotency_keys(self, now).await
    }
}

#[async_trait]
impl MultipartStore for Store {
    async fn require_file(
        &self,
        scope: &toolkit_security::AccessScope,
        file_id: Uuid,
    ) -> Result<File, DomainError> {
        Store::require_file(self, scope, file_id).await
    }

    async fn get_policy(
        &self,
        scope: &toolkit_security::AccessScope,
        tenant_id: Uuid,
        policy_scope: &crate::domain::policy::PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<Option<crate::domain::policy::StoredPolicy>, DomainError> {
        Store::get_policy(self, scope, tenant_id, policy_scope, scope_owner_id).await
    }

    async fn insert_pending_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        mime_type: &str,
        backend_id: &str,
        backend_path: &str,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        Store::insert_pending_version(
            self,
            file_id,
            version_id,
            mime_type,
            backend_id,
            backend_path,
            now,
        )
        .await
    }

    async fn create_multipart_upload(
        &self,
        upload_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
        backend_upload_handle: &str,
        declared_mime: &str,
        declared_size: u64,
        part_size: u64,
        auto_bind: bool,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        Store::create_multipart_upload(
            self,
            upload_id,
            file_id,
            version_id,
            backend_upload_handle,
            declared_mime,
            declared_size,
            part_size,
            auto_bind,
            expires_at,
            now,
        )
        .await
    }

    async fn get_multipart_upload(
        &self,
        upload_id: Uuid,
    ) -> Result<Option<crate::domain::multipart::MultipartUploadSession>, DomainError> {
        Store::get_multipart_upload(self, upload_id).await
    }

    async fn get_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<FileVersion>, DomainError> {
        Store::get_version(self, file_id, version_id).await
    }

    async fn get_version_manifest(&self, version_id: Uuid) -> Result<Option<String>, DomainError> {
        Store::get_version_manifest(self, version_id).await
    }

    async fn upsert_multipart_part(
        &self,
        upload_id: Uuid,
        part_number: i32,
        backend_etag: &str,
        part_hash: Vec<u8>,
        size: i64,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        Store::upsert_multipart_part(
            self,
            upload_id,
            part_number,
            backend_etag,
            part_hash,
            size,
            now,
        )
        .await
    }

    async fn list_multipart_parts(
        &self,
        upload_id: Uuid,
    ) -> Result<Vec<crate::domain::multipart::MultipartPart>, DomainError> {
        Store::list_multipart_parts(self, upload_id).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn finalize_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        hash_value: Vec<u8>,
        hash_mode: crate::infra::content::hash_mode::HashMode,
        part_count: Option<i32>,
        manifest: Option<String>,
        validated_mime: Option<String>,
        audit: crate::domain::audit::AuditEntry,
        auto_bind: Option<crate::domain::ports::AutoBindOnFinalize>,
    ) -> Result<crate::domain::ports::FinalizeVersionOutcome, DomainError> {
        // `validated_mime` is the sniffed/canonical type computed by
        // `complete_multipart_upload` from the assembled object's leading
        // bytes (P2 remediation item 1.10) — persisted in place of the
        // client's declared type, mirroring the single-part finalize paths.
        Store::finalize_version(
            self,
            file_id,
            version_id,
            size,
            hash_value,
            hash_mode,
            part_count,
            manifest,
            validated_mime,
            audit,
            auto_bind,
        )
        .await
    }

    async fn complete_multipart_upload(
        &self,
        upload_id: Uuid,
        result_json: &str,
        audit: crate::domain::audit::AuditEntry,
    ) -> Result<bool, DomainError> {
        Store::complete_multipart_upload(self, upload_id, result_json, audit).await
    }

    async fn acquire_multipart_complete_lease(
        &self,
        upload_id: Uuid,
        owner: &str,
        lease_until: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        Store::acquire_multipart_complete_lease(self, upload_id, owner, lease_until, now).await
    }

    async fn release_multipart_complete_lease(
        &self,
        upload_id: Uuid,
        owner: &str,
    ) -> Result<bool, DomainError> {
        Store::release_multipart_complete_lease(self, upload_id, owner).await
    }

    async fn abort_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: crate::domain::audit::AuditEntry,
    ) -> Result<bool, DomainError> {
        Store::abort_multipart_upload(self, upload_id, audit).await
    }

    async fn delete_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: crate::domain::audit::AuditEntry,
    ) -> Result<bool, DomainError> {
        Store::delete_version(self, file_id, version_id, audit).await
    }
}

#[async_trait]
impl crate::domain::ports::PolicyStore for Store {
    async fn require_file(
        &self,
        scope: &toolkit_security::AccessScope,
        file_id: Uuid,
    ) -> Result<File, DomainError> {
        Store::require_file(self, scope, file_id).await
    }

    async fn get_policy(
        &self,
        scope: &toolkit_security::AccessScope,
        tenant_id: Uuid,
        policy_scope: &crate::domain::policy::PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<Option<crate::domain::policy::StoredPolicy>, DomainError> {
        Store::get_policy(self, scope, tenant_id, policy_scope, scope_owner_id).await
    }

    async fn upsert_policy(
        &self,
        scope: &toolkit_security::AccessScope,
        tenant_id: Uuid,
        policy_scope: &crate::domain::policy::PolicyScope,
        scope_owner_id: Option<Uuid>,
        body: &crate::domain::policy::PolicyBody,
        now: OffsetDateTime,
    ) -> Result<Uuid, DomainError> {
        Store::upsert_policy(
            self,
            scope,
            tenant_id,
            policy_scope,
            scope_owner_id,
            body,
            now,
        )
        .await
    }

    async fn list_retention_rules(
        &self,
        scope: &toolkit_security::AccessScope,
        tenant_id: Uuid,
    ) -> Result<Vec<crate::domain::policy::StoredRetentionRule>, DomainError> {
        Store::list_retention_rules(self, scope, tenant_id).await
    }

    async fn insert_retention_rule(
        &self,
        scope: &toolkit_security::AccessScope,
        tenant_id: Uuid,
        retention_scope: &crate::domain::policy::RetentionScope,
        scope_target_id: Option<Uuid>,
        body: &crate::domain::policy::RetentionRuleBody,
        now: OffsetDateTime,
    ) -> Result<Uuid, DomainError> {
        Store::insert_retention_rule(
            self,
            scope,
            tenant_id,
            retention_scope,
            scope_target_id,
            body,
            now,
        )
        .await
    }

    async fn delete_retention_rule(
        &self,
        scope: &toolkit_security::AccessScope,
        rule_id: Uuid,
    ) -> Result<bool, DomainError> {
        Store::delete_retention_rule(self, scope, rule_id).await
    }

    async fn get_retention_rule(
        &self,
        scope: &toolkit_security::AccessScope,
        rule_id: Uuid,
    ) -> Result<Option<crate::domain::policy::StoredRetentionRule>, DomainError> {
        Store::get_retention_rule(self, scope, rule_id).await
    }
}
