//! FileStorage Module Implementation
//!
//! Public API is defined in `cf-file-storage-sdk` and re-exported here.
//! P1 ships only the in-process SDK trait `FileStorageClient` — the REST
//! surface (`cpt-cf-file-storage-fr-rest-api`) is P2.

pub use file_storage_sdk::{
    Backend, BackendId, ByteRange, CapabilityTag, Etag, FileByteStream, FileId, FileInfo,
    FileList, FileMeta, FileMetaUpdate, FileReadHandle, FileStatus, FileStorageClient,
    FileStorageError, KNOWN_CAPABILITIES, ListFilesQuery, OwnerRef, PresignDownloadItem,
    PresignDownloadOutcome, PresignedDownload, PresignedUploadHandle, ResolvedByteRange,
    UploadedPart, UrlParams, VersionId,
};

pub mod module;
pub use module::FileStorageModule;

#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod errors;
#[doc(hidden)]
pub mod infra;
