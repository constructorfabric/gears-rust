//! GTS resource-type constants for the file-storage gear.

/// GTS file-type resource family used by the Authorization Service to make
/// per-type access decisions (PRD `cpt-cf-file-storage-fr-file-type-classification`).
pub const FILE_TYPE_RESOURCE: &str = "gts.cf.fstorage.file.type.v1~";

#[cfg(test)]
#[path = "gts_tests.rs"]
mod gts_tests;
