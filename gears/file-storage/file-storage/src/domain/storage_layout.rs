//! The single definition of the backend object path layout.
//!
//! A version's path is derived from `(file_id, version_id)` alone.

use uuid::Uuid;

/// Backend object path for a version's content: `/{file_id}/{version_id}`.
pub fn backend_path(file_id: Uuid, version_id: Uuid) -> String {
    format!("/{file_id}/{version_id}")
}
