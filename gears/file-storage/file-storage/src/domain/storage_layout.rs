//! The single definition of the backend object path layout.
//!
//! Content bytes for a version are addressed by a deterministic path derived
//! from `(file_id, version_id)` alone, with no other inputs.

use uuid::Uuid;

/// Backend object path for a version's content: `/{file_id}/{version_id}`.
///
/// The single definition of the layout. The path is computed once when a
/// version is created and persisted in `file_versions.backend_path` (and,
/// since `m20260924_000001_upload_flow_redesign`, mirrored onto the session
/// row in `multipart_uploads.backend_path` at initiate time); every later
/// reader uses one of those stored values instead of calling this function
/// again. The only remaining callers are the expired-multipart-session
/// cleanup and a couple of `MultipartService` call sites, and only for a
/// *legacy* session predating that column — see
/// `MultipartUploadSession::backend_path_or_default` and
/// `CleanupEngine::cleanup_expired_session_version_with_file`.
pub fn backend_path(file_id: Uuid, version_id: Uuid) -> String {
    format!("/{file_id}/{version_id}")
}

#[cfg(test)]
mod tests {
    use super::backend_path;
    use uuid::Uuid;

    #[test]
    fn formats_as_slash_file_id_slash_version_id() {
        let file_id = Uuid::from_u128(1);
        let version_id = Uuid::from_u128(2);
        assert_eq!(
            backend_path(file_id, version_id),
            "/00000000-0000-0000-0000-000000000001/00000000-0000-0000-0000-000000000002"
        );
    }
}
