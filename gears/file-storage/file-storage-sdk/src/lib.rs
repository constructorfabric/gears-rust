//! File Storage SDK
//!
//! Public API surface for the `file-storage` gear (control plane). The P1
//! operations land incrementally; this crate currently pins the stable types
//! other gears consume:
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
    ByteRange, CustomMetadataEntry, CustomMetadataPatch, File, FileId, FileVersion, NewFile,
    OwnerFilter, OwnerKind, VersionId, VersionStatus,
};

pub use toolkit_canonical_errors::CanonicalError as FileStorageError;
pub use toolkit_canonical_errors::{self, CanonicalError, Problem};
