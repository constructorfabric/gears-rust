//! File Storage SDK
//!
//! Public API surface for the `file-storage` gear (control plane). Level 1:
//! every control-plane operation runs in-process through
//! [`FileStorageClientV1`] and returns models / signed URLs — the SDK never
//! transfers file bytes itself (see `gears/file-storage/docs/DESIGN.md`'s
//! `sdk-facade` component for what a future Level 2 would add). This crate
//! pins the stable types other gears consume:
//!
//! - [`FileStorageClientV1`] — the inter-gear client trait (resolved from `ClientHub`)
//! - model types ([`models`])
//! - GTS resource-type constants ([`gts`])
//! - [`FileStorageError`] — the canonical error envelope
//!
//! ## Usage
//!
//! ```ignore
//! use file_storage_sdk::FileStorageClientV1;
//!
//! let client = hub.get::<dyn FileStorageClientV1>()?;
//! ```

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod api;
pub mod gts;
pub mod models;

pub use api::FileStorageClientV1;
pub use gts::FILE_TYPE_RESOURCE;
pub use models::{
    AgeRetention, BindState, ByteRange, CompletedMultipartUpload, CreateFileOutcome,
    CustomMetadataEntry, CustomMetadataPatch, DownloadTicket, EffectivePolicy, File, FileFetch,
    FileId, FileRecord, FileVersion, InactivityRetention, MetadataLimits, MetadataRetention,
    MimeSizeOverride, MissingPart, MultipartCompleteOutcome, MultipartIntent, MultipartPartPlan,
    MultipartPlan, MultipartStatus, MultipartUploadState, NewFile, OwnerFilter, OwnerKind, Page,
    Policy, PolicyBody, PolicyScope, ReceivedPart, RetentionRule, RetentionRuleBody,
    RetentionScope, SizeLimits, Storage, StorageCapabilities, UploadTicket, VersionId,
    VersionRecord, VersionStatus,
};

pub use toolkit_canonical_errors::CanonicalError as FileStorageError;
pub use toolkit_canonical_errors::{self, CanonicalError, Problem};
