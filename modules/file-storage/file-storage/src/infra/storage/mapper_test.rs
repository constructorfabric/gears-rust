#[cfg(test)]
mod tests {
    use super::super::entity;
    use super::super::mapper::{
        entity_to_file_info, parse_custom_metadata, status_sdk_to_str, status_str_to_sdk,
    };
    use file_storage_sdk::FileStatus;
    use time::OffsetDateTime;
    use uuid::Uuid;

    fn make_model(status: &str, custom_metadata: &str, size: i64) -> entity::Model {
        entity::Model {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            backend_id: Uuid::new_v4(),
            file_path: "a/b.bin".to_owned(),
            owner_id: Uuid::new_v4(),
            name: "doc.bin".to_owned(),
            gts_file_type: "gts.x.foo".to_owned(),
            mime_type: "application/octet-stream".to_owned(),
            size_bytes: size,
            etag: "etag-1".to_owned(),
            meta_revision: 7,
            status: status.to_owned(),
            custom_metadata: custom_metadata.to_owned(),
            upload_expires_at: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            modified_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    // ── status_str_to_sdk ───────────────────────────────────────────────────

    #[test]
    fn status_str_to_sdk_known_values() {
        assert_eq!(status_str_to_sdk("uploaded"), FileStatus::Uploaded);
        assert_eq!(status_str_to_sdk("pending_upload"), FileStatus::PendingUpload);
    }

    #[test]
    fn status_str_to_sdk_unknown_collapses_to_pending_upload() {
        assert_eq!(status_str_to_sdk("anything-else"), FileStatus::PendingUpload);
        assert_eq!(status_str_to_sdk(""), FileStatus::PendingUpload);
    }

    #[test]
    fn status_sdk_to_str_round_trip() {
        for s in [FileStatus::PendingUpload, FileStatus::Uploaded] {
            let str_form = status_sdk_to_str(s);
            assert_eq!(status_str_to_sdk(str_form), s, "round-trip for {s:?}");
        }
    }

    #[test]
    fn status_sdk_to_str_uses_snake_case_strings() {
        assert_eq!(status_sdk_to_str(FileStatus::PendingUpload), "pending_upload");
        assert_eq!(status_sdk_to_str(FileStatus::Uploaded), "uploaded");
    }

    #[test]
    fn parse_custom_metadata_empty_string_returns_empty_map() {
        let m = parse_custom_metadata("");
        assert!(m.is_empty());
    }

    #[test]
    fn parse_custom_metadata_valid_json() {
        let m = parse_custom_metadata(r#"{"a":"1","b":"2"}"#);
        assert_eq!(m.get("a"), Some(&"1".to_owned()));
        assert_eq!(m.get("b"), Some(&"2".to_owned()));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn parse_custom_metadata_invalid_json_returns_empty_map() {
        let m = parse_custom_metadata("not json");
        assert!(m.is_empty(), "invalid JSON must fall back to empty map");
    }

    #[test]
    fn entity_to_file_info_owner_id_is_propagated() {
        let m = make_model("uploaded", "{}", 42);
        let owner_id = m.owner_id;
        let tenant_id = m.tenant_id;
        let info = entity_to_file_info(m);
        assert_eq!(info.owner.owner_id, owner_id);
        assert_eq!(info.owner.tenant_id, tenant_id);
        assert_eq!(info.size_bytes, 42);
        assert_eq!(info.status, FileStatus::Uploaded);
    }

    #[test]
    fn entity_to_file_info_pending_status() {
        let m = make_model("pending_upload", "{}", 0);
        let info = entity_to_file_info(m);
        assert_eq!(info.status, FileStatus::PendingUpload);
    }

    #[test]
    fn entity_to_file_info_meta_size_present_when_size_nonneg() {
        let m = make_model("uploaded", "{}", 100);
        let info = entity_to_file_info(m);
        assert_eq!(info.meta.size_bytes, Some(100));
    }

    #[test]
    fn entity_to_file_info_propagates_mime_and_name() {
        let m = make_model("uploaded", "{}", 0);
        let info = entity_to_file_info(m);
        assert_eq!(info.meta.mime_type, "application/octet-stream");
        assert_eq!(info.meta.name, "doc.bin");
        assert_eq!(info.meta.gts_file_type, "gts.x.foo");
    }

    #[test]
    fn entity_to_file_info_parses_custom_metadata_json() {
        let m = make_model("uploaded", r#"{"k":"v"}"#, 0);
        let info = entity_to_file_info(m);
        assert_eq!(info.meta.custom_metadata.get("k"), Some(&"v".to_owned()));
    }

    #[test]
    fn entity_to_file_info_empty_custom_metadata_string_yields_empty_map() {
        let m = make_model("uploaded", "", 0);
        let info = entity_to_file_info(m);
        assert!(info.meta.custom_metadata.is_empty());
    }
}
