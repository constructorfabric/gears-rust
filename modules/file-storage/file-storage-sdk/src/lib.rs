//! `FileStorage` SDK
//!
//! Public trait, models, and errors for the `FileStorage` module. Mirrors the
//! contract defined in `modules/file-storage/docs/rust-traits.md`.
//!
//! Consumers obtain the client from `ClientHub`:
//!
//! ```ignore
//! let files = hub.get::<dyn FileStorageClient>()?;
//! let backends = files.list_backends(&ctx).await?;
//! ```

#![forbid(unsafe_code)]

pub mod api;
pub mod errors;
pub mod models;

#[cfg(test)]
mod models_test;

pub use api::FileStorageClient;
pub use errors::FileStorageError;
pub use models::{
    Backend, BackendCapability, BackendId, BackendKind, BackendTransport, CustomMetadata, Etag,
    FileByteStream, FileId, FileInfo, FileList, FileMeta, FileMetaUpdate, FileReadHandle,
    FileStatus, ListFilesQuery, OwnerRef, PresignDownloadItem, PresignDownloadOutcome,
    PresignedDownload, PresignedUploadHandle, UrlParams,
};
