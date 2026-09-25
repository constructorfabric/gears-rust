use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::*;
use crate::models::{
    CreateFileOutcome, EffectivePolicy, FileFetch, MetadataLimits, MultipartCompleteOutcome,
    MultipartPlan, MultipartStatus, Policy, RetentionRule, Storage, UploadTicket, VersionRecord,
};

/// Minimal in-test implementation to prove the trait is object-safe and usable
/// through a trait object (which is how `ClientHub` stores it). Every method
/// returns a canned `NotFound`-shaped error or an empty/default value — this
/// only exercises the trait's shape, not any real behavior.
struct StubClient;

fn stub_err() -> FileStorageError {
    toolkit_canonical_errors::CanonicalError::internal("stub").create()
}

#[async_trait]
impl FileStorageClientV1 for StubClient {
    async fn create_file(
        &self,
        _ctx: &SecurityContext,
        _new: NewFile,
        _idempotency_key: Option<String>,
        _auto_bind: bool,
        _multipart: Option<MultipartIntent>,
    ) -> Result<CreateFileOutcome, FileStorageError> {
        Err(stub_err())
    }

    async fn get_file(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _if_none_match: Option<&str>,
    ) -> Result<FileFetch, FileStorageError> {
        Err(stub_err())
    }

    async fn list_files(
        &self,
        _ctx: &SecurityContext,
        _owner: OwnerFilter,
        _limit: Option<u64>,
        _offset: u64,
    ) -> Result<Page<FileRecord>, FileStorageError> {
        Ok(Page {
            items: vec![],
            next_offset: None,
        })
    }

    async fn update_metadata(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _patch: CustomMetadataPatch,
        _if_match_metadata: Option<i64>,
    ) -> Result<FileRecord, FileStorageError> {
        Err(stub_err())
    }

    async fn delete_file(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _if_match: Option<&str>,
    ) -> Result<(), FileStorageError> {
        Err(stub_err())
    }

    async fn download_url(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _version_id: Option<VersionId>,
    ) -> Result<crate::models::DownloadTicket, FileStorageError> {
        Err(stub_err())
    }

    async fn list_versions(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _limit: Option<u64>,
        _offset: u64,
    ) -> Result<Page<VersionRecord>, FileStorageError> {
        Ok(Page {
            items: vec![],
            next_offset: None,
        })
    }

    async fn presign_version(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
    ) -> Result<UploadTicket, FileStorageError> {
        Err(stub_err())
    }

    async fn bind(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _version_id: VersionId,
        _if_match: Option<&str>,
    ) -> Result<FileRecord, FileStorageError> {
        Err(stub_err())
    }

    async fn delete_version(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _version_id: VersionId,
    ) -> Result<(), FileStorageError> {
        Err(stub_err())
    }

    async fn initiate_multipart(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _declared_mime: &str,
        _declared_size: u64,
        _preferred_part_size: Option<u64>,
        _concurrency: Option<u32>,
    ) -> Result<MultipartPlan, FileStorageError> {
        Err(stub_err())
    }

    async fn introspect_multipart(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _upload_id: Uuid,
    ) -> Result<MultipartStatus, FileStorageError> {
        Err(stub_err())
    }

    async fn complete_multipart(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _upload_id: Uuid,
        _if_match: Option<&str>,
    ) -> Result<MultipartCompleteOutcome, FileStorageError> {
        Err(stub_err())
    }

    async fn abort_multipart(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _upload_id: Uuid,
    ) -> Result<(), FileStorageError> {
        Err(stub_err())
    }

    async fn transfer_ownership(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _new_owner_kind: OwnerKind,
        _new_owner_id: Uuid,
    ) -> Result<FileRecord, FileStorageError> {
        Err(stub_err())
    }

    async fn migrate_backend(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _target_backend_id: &str,
    ) -> Result<(), FileStorageError> {
        Err(stub_err())
    }

    async fn list_storages(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<Storage>, FileStorageError> {
        Ok(vec![])
    }

    async fn get_storage(
        &self,
        _ctx: &SecurityContext,
        _storage_id: &str,
    ) -> Result<Storage, FileStorageError> {
        Err(stub_err())
    }

    async fn get_policy(
        &self,
        _ctx: &SecurityContext,
        _scope: PolicyScope,
        _scope_owner_id: Option<Uuid>,
    ) -> Result<Option<Policy>, FileStorageError> {
        Ok(None)
    }

    async fn get_effective_policy(
        &self,
        _ctx: &SecurityContext,
        _user_owner_id: Option<Uuid>,
    ) -> Result<EffectivePolicy, FileStorageError> {
        Ok(EffectivePolicy {
            allowed_mime_types: None,
            max_bytes: None,
            per_mime_max_bytes: vec![],
            metadata_limits: MetadataLimits::default(),
        })
    }

    async fn put_policy(
        &self,
        _ctx: &SecurityContext,
        _scope: PolicyScope,
        _scope_owner_id: Option<Uuid>,
        _body: PolicyBody,
    ) -> Result<Policy, FileStorageError> {
        Err(stub_err())
    }

    async fn list_retention_rules(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<RetentionRule>, FileStorageError> {
        Ok(vec![])
    }

    async fn create_retention_rule(
        &self,
        _ctx: &SecurityContext,
        _scope: RetentionScope,
        _scope_target_id: Option<Uuid>,
        _body: RetentionRuleBody,
    ) -> Result<RetentionRule, FileStorageError> {
        Err(stub_err())
    }

    async fn delete_retention_rule(
        &self,
        _ctx: &SecurityContext,
        _rule_id: Uuid,
    ) -> Result<(), FileStorageError> {
        Err(stub_err())
    }
}

#[test]
fn client_trait_is_object_safe() {
    // Constructing the trait object alone (no runtime needed — the
    // `#[async_trait]`-generated methods return boxed futures, not `impl
    // Trait`) is enough to prove the trait is object-safe, which is how
    // `ClientHub` stores it.
    let client: Box<dyn FileStorageClientV1> = Box::new(StubClient);
    drop(client);
}

#[test]
fn client_trait_object_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Box<dyn FileStorageClientV1>>();
}
