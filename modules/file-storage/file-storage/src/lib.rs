//! FileStorage Module Implementation
//!
//! Public API is defined in `cf-file-storage-sdk` and re-exported here.

pub use file_storage_sdk::{
    Backend, BackendCapability, BackendId, BackendKind, BackendTransport, FileByteStream, FileId,
    FileInfo, FileList, FileMeta, FileMetaUpdate, FileReadHandle, FileStatus, FileStorageClient,
    FileStorageError, ListFilesQuery, OwnerRef, PresignDownloadItem, PresignDownloadOutcome,
    PresignedDownload, PresignedUploadHandle, UrlParams,
};

pub mod module;
pub use module::FileStorageModule;

#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod errors;
#[doc(hidden)]
pub mod infra;

#[cfg(test)]
mod config_test;
