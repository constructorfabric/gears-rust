//! In-process implementation of `FileStorageClient` — a thin adapter that
//! forwards SDK-level calls to the `Service` and translates `DomainError`
//! into `FileStorageError`.

use std::sync::Arc;

use async_trait::async_trait;
use modkit_security::SecurityContext;

use file_storage_sdk::{
    Backend, BackendId, ByteRange, CapabilityTag, Etag, FileByteStream, FileId, FileInfo,
    FileList, FileMeta, FileMetaUpdate, FileReadHandle, FileStorageClient, FileStorageError,
    ListFilesQuery, OwnerRef, PresignDownloadItem, PresignDownloadOutcome, PresignedUploadHandle,
    UploadedPart, UrlParams, VersionId,
};

use super::repo::FilesRepo;
use super::service::Service;

pub struct LocalClient<R: FilesRepo + 'static> {
    service: Arc<Service<R>>,
}

impl<R: FilesRepo + 'static> LocalClient<R> {
    pub fn new(service: Arc<Service<R>>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl<R: FilesRepo + 'static> FileStorageClient for LocalClient<R> {
    async fn list_backends(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Backend>, FileStorageError> {
        self.service.list_backends(ctx).await.map_err(Into::into)
    }

    async fn create_presigned_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Option<FileId>,
        backend_id: Option<BackendId>,
        owner: OwnerRef,
        meta: FileMeta,
        capability: &CapabilityTag,
        part_count: u32,
        params: UrlParams,
    ) -> Result<PresignedUploadHandle, FileStorageError> {
        self.service
            .create_presigned_upload(
                ctx, file_id, backend_id, owner, meta, capability, part_count, params,
            )
            .await
            .map_err(Into::into)
    }

    async fn complete_upload(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        upload_id: &str,
        parts: Vec<UploadedPart>,
    ) -> Result<FileInfo, FileStorageError> {
        self.service
            .complete_upload(ctx, file_id, upload_id, parts)
            .await
            .map_err(Into::into)
    }

    async fn abort_upload(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        upload_id: &str,
    ) -> Result<(), FileStorageError> {
        self.service
            .abort_upload(ctx, file_id, upload_id)
            .await
            .map_err(Into::into)
    }

    async fn get_file_info(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
    ) -> Result<FileInfo, FileStorageError> {
        self.service
            .get_file_info(ctx, file_id, etag, version_id)
            .await
            .map_err(Into::into)
    }

    async fn put_file_info(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        update: FileMetaUpdate,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
    ) -> Result<FileInfo, FileStorageError> {
        self.service
            .put_file_info(ctx, file_id, update, etag, version_id)
            .await
            .map_err(Into::into)
    }

    async fn delete_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
    ) -> Result<(), FileStorageError> {
        self.service
            .delete_file(ctx, file_id, etag, version_id)
            .await
            .map_err(Into::into)
    }

    async fn list_files(
        &self,
        ctx: &SecurityContext,
        query: ListFilesQuery,
    ) -> Result<FileList, FileStorageError> {
        self.service.list_files(ctx, query).await.map_err(Into::into)
    }

    async fn read_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
        range: Option<ByteRange>,
    ) -> Result<FileReadHandle, FileStorageError> {
        self.service
            .read_file(ctx, file_id, etag, version_id, range)
            .await
            .map_err(Into::into)
    }

    async fn put_file(
        &self,
        ctx: &SecurityContext,
        file_id: Option<FileId>,
        backend_id: Option<BackendId>,
        owner: OwnerRef,
        meta: FileMeta,
        bytes: FileByteStream,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
    ) -> Result<FileInfo, FileStorageError> {
        self.service
            .put_file(ctx, file_id, backend_id, owner, meta, bytes, etag, version_id)
            .await
            .map_err(Into::into)
    }

    async fn presign_urls(
        &self,
        ctx: &SecurityContext,
        items: Vec<PresignDownloadItem>,
    ) -> Result<Vec<PresignDownloadOutcome>, FileStorageError> {
        self.service.presign_urls(ctx, items).await.map_err(Into::into)
    }
}
