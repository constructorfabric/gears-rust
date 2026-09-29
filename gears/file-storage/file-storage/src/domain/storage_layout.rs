//! The single definition of the backend object path layout.
//!
//! Content bytes for a version are addressed by a deterministic path derived
//! from `(file_id, version_id)` alone, with no other inputs.

use uuid::Uuid;

/// Backend object path for a version's content: `/{file_id}/{version_id}`.
///
/// The single definition of the layout: every place that needs a version's
/// object path derives it from here rather than formatting it inline.
pub fn backend_path(file_id: Uuid, version_id: Uuid) -> String {
    format!("/{file_id}/{version_id}")
}
