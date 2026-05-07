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

#[cfg(test)]
mod tests {
    use super::*;
    use file_storage_sdk::FileStatus;

    #[test]
    fn status_round_trip() {
        let cases = vec![
            FileStatus::PendingUpload,
            FileStatus::Completing,
            FileStatus::Uploaded,
            FileStatus::MetaUpdating,
            FileStatus::Deleting,
        ];
        for s in cases {
            let str = status_sdk_to_str(s);
            let back = status_str_to_sdk(str);
            assert_eq!(back, s, "round-trip: {s:?} → {str:?} → {back:?}");
        }
    }

    #[test]
    fn status_str_to_sdk_known_values() {
        assert_eq!(status_str_to_sdk("pending_upload"), FileStatus::PendingUpload);
        assert_eq!(status_str_to_sdk("completing"), FileStatus::Completing);
        assert_eq!(status_str_to_sdk("uploaded"), FileStatus::Uploaded);
        assert_eq!(status_str_to_sdk("meta_updating"), FileStatus::MetaUpdating);
        assert_eq!(status_str_to_sdk("deleting"), FileStatus::Deleting);
    }

    #[test]
    fn status_str_to_sdk_unknown_collapses_to_pending() {
        assert_eq!(status_str_to_sdk(""), FileStatus::PendingUpload);
        assert_eq!(status_str_to_sdk("garbage"), FileStatus::PendingUpload);
        assert_eq!(status_str_to_sdk("UPLOADED"), FileStatus::PendingUpload); // case-sensitive
    }

    #[test]
    fn status_sdk_to_str_yields_lowercase_snake_case() {
        for s in [
            FileStatus::PendingUpload,
            FileStatus::Completing,
            FileStatus::Uploaded,
            FileStatus::MetaUpdating,
            FileStatus::Deleting,
        ] {
            let str = status_sdk_to_str(s);
            assert!(
                str.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "expected snake_case, got: {str:?}"
            );
        }
    }

    #[test]
    fn parse_custom_metadata_empty_returns_empty_map() {
        let m = parse_custom_metadata("");
        assert!(m.is_empty());
    }

    #[test]
    fn parse_custom_metadata_valid_json() {
        let raw = r#"{"key1":"value1","key2":"value2"}"#;
        let m = parse_custom_metadata(raw);
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("key1").map(String::as_str), Some("value1"));
        assert_eq!(m.get("key2").map(String::as_str), Some("value2"));
    }

    #[test]
    fn parse_custom_metadata_malformed_yields_empty_map() {
        let m = parse_custom_metadata("not json");
        assert!(
            m.is_empty(),
            "malformed input should not panic; got: {m:?}"
        );
    }
}
