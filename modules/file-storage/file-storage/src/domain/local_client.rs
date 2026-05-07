//! `LocalClient` — bridges the public SDK trait to the in-process
//! `Service`.

use std::sync::Arc;

use async_trait::async_trait;
use file_storage_sdk::{
    Backend, BackendId, Etag, FileByteStream, FileId, FileInfo, FileList, FileMeta, FileMetaUpdate,
    FileReadHandle, FileStatus, FileStorageClient, FileStorageError, ListFilesQuery, OwnerRef,
    PresignDownloadItem, PresignDownloadOutcome, PresignedUploadHandle, UrlParams,
};
use modkit_security::SecurityContext;

use super::repo::FilesRepo;
use super::service::Service;

pub struct LocalClient<R: FilesRepo + 'static> {
    service: Arc<Service<R>>,
}

impl<R: FilesRepo + 'static> LocalClient<R> {
    #[must_use]
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

    async fn create_presigned_url(
        &self,
        ctx: &SecurityContext,
        backend_id: Option<BackendId>,
        owner: OwnerRef,
        file_path: &str,
        meta: FileMeta,
        params: UrlParams,
    ) -> Result<PresignedUploadHandle, FileStorageError> {
        self.service
            .create_presigned_url(ctx, backend_id, owner, file_path, meta, params)
            .await
            .map_err(Into::into)
    }

    async fn change_status(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        target: FileStatus,
        old_etag: Etag,
        new_etag: Etag,
    ) -> Result<FileInfo, FileStorageError> {
        self.service
            .change_status(ctx, file_id, target, old_etag, new_etag)
            .await
            .map_err(Into::into)
    }

    async fn get_file_info(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
    ) -> Result<FileInfo, FileStorageError> {
        self.service
            .get_file_info(ctx, file_id, etag)
            .await
            .map_err(Into::into)
    }

    async fn put_file_info(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        update: FileMetaUpdate,
        etag: Etag,
    ) -> Result<FileInfo, FileStorageError> {
        self.service
            .put_file_info(ctx, file_id, update, etag)
            .await
            .map_err(Into::into)
    }

    async fn delete_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Etag,
    ) -> Result<(), FileStorageError> {
        self.service
            .delete_file(ctx, file_id, etag)
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
    ) -> Result<FileReadHandle, FileStorageError> {
        self.service
            .read_file(ctx, file_id, etag)
            .await
            .map_err(Into::into)
    }

    async fn put_file(
        &self,
        ctx: &SecurityContext,
        backend_id: Option<BackendId>,
        owner: OwnerRef,
        file_path: &str,
        meta: FileMeta,
        bytes: FileByteStream,
        etag: Option<&Etag>,
    ) -> Result<FileInfo, FileStorageError> {
        self.service
            .put_file(ctx, backend_id, owner, file_path, meta, bytes, etag)
            .await
            .map_err(Into::into)
    }

    async fn presign_urls(
        &self,
        ctx: &SecurityContext,
        items: Vec<PresignDownloadItem>,
    ) -> Result<Vec<PresignDownloadOutcome>, FileStorageError> {
        self.service
            .presign_urls(ctx, items)
            .await
            .map_err(Into::into)
    }
}
