//! Content pipeline: hashing, content-type (magic-byte) validation and HTTP `Range` parsing.

pub mod hash;
pub mod hash_mode;
pub mod mime;
pub mod range;
