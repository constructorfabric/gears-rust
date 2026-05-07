//! Entity ↔ SDK model mapping.

use file_storage_sdk::{CustomMetadata, FileInfo, FileMeta, FileStatus, OwnerRef};

use super::entity;

pub fn status_str_to_sdk(s: &str) -> FileStatus {
    match s {
        "pending_upload" => FileStatus::PendingUpload,
        "completing" => FileStatus::Completing,
        "uploaded" => FileStatus::Uploaded,
        "meta_updating" => FileStatus::MetaUpdating,
        "deleting" => FileStatus::Deleting,
        // Unknown statuses collapse to PendingUpload — caller-facing path
        // never sees adapter-specific extensions.
        _ => FileStatus::PendingUpload,
    }
}

pub fn status_sdk_to_str(status: FileStatus) -> &'static str {
    match status {
        FileStatus::PendingUpload => "pending_upload",
        FileStatus::Completing => "completing",
        FileStatus::Uploaded => "uploaded",
        FileStatus::MetaUpdating => "meta_updating",
        FileStatus::Deleting => "deleting",
    }
}

pub fn parse_custom_metadata(raw: &str) -> CustomMetadata {
    if raw.is_empty() {
        return CustomMetadata::new();
    }
    serde_json::from_str(raw).unwrap_or_default()
}

pub fn entity_to_file_info(m: entity::Model) -> FileInfo {
    let owner = OwnerRef {
        tenant_id: m.tenant_id,
        owner_id: m.owner_id,
    };
    let meta = FileMeta {
        name: m.name,
        mime_type: m.mime_type,
        gts_file_type: m.gts_file_type,
        custom_metadata: parse_custom_metadata(&m.custom_metadata),
    };
    FileInfo {
        file_id: m.id,
        backend_id: m.backend_id,
        file_path: m.file_path,
        owner,
        meta,
        status: status_str_to_sdk(&m.status),
        etag: m.etag,
        version_id: m.version_id,
        size_bytes: u64::try_from(m.size_bytes).unwrap_or(0),
        created_at: m.created_at,
        updated_at: m.updated_at,
        upload_expires_at: m.upload_expires_at,
    }
}
