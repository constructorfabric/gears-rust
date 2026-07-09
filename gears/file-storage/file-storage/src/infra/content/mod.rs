//! Content pipeline: hashing, content-type (magic-byte) validation, and HTTP
//! `Range` parsing. These are the data-plane primitives the sidecar uses while
//! streaming bytes (`cpt-cf-file-storage-component-content-pipeline`).

pub mod hash;
pub mod mime;
pub mod range;
