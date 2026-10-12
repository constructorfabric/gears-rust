//! GTS resource-type constants for the file-storage gear.

use toolkit_gts::gts_id;

/// GTS file-type resource family used by the Authorization Service for per-type decisions.
pub const FILE_TYPE_RESOURCE: &str = gts_id!("cf.fstorage.file.type.v1~");

#[cfg(test)]
#[path = "gts_tests.rs"]
mod gts_tests;
