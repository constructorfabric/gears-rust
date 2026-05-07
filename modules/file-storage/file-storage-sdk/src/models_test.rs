#[cfg(test)]
mod tests {
    use super::super::models::*;
    use std::collections::BTreeMap;
    use time::OffsetDateTime;
    use uuid::Uuid;

    #[test]
    fn url_params_default_has_expected_values() {
        let p = UrlParams::default();
        assert_eq!(p.expires_in_seconds, 600);
        assert!(p.content_disposition.is_none());
        assert!(p.content_type_override.is_none());
        assert!(p.allowed_client_cidrs.is_empty());
        assert!(!p.refresh_etag);
    }

    #[test]
    fn file_meta_update_default_is_all_none() {
        let u = FileMetaUpdate::default();
        assert!(u.name.is_none());
        assert!(u.mime_type.is_none());
        assert!(u.custom_metadata.is_none());
    }

    #[test]
    fn owner_ref_eq_when_fields_match() {
        let tenant = Uuid::new_v4();
        let owner = Uuid::new_v4();
        let a = OwnerRef {
            tenant_id: tenant,
            owner_id: owner,
        };
        let b = OwnerRef {
            tenant_id: tenant,
            owner_id: owner,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn file_status_equality() {
        assert_eq!(FileStatus::PendingUpload, FileStatus::PendingUpload);
        assert_ne!(FileStatus::PendingUpload, FileStatus::Uploaded);
    }

    #[test]
    fn backend_capability_equality() {
        let pu = BackendCapability::PresignedUrls;
        let pr = BackendCapability::PublicReadUrls;
        assert_eq!(pu, BackendCapability::PresignedUrls);
        assert_ne!(pu, pr);
    }

    #[test]
    fn backend_kind_equality() {
        assert_eq!(BackendKind::S3Compatible, BackendKind::S3Compatible);
    }

    #[test]
    fn backend_transport_equality() {
        assert_eq!(BackendTransport::Redirect, BackendTransport::Redirect);
    }

    #[test]
    fn file_info_round_trip_via_clone_eq() {
        let owner = OwnerRef {
            tenant_id: Uuid::new_v4(),
            owner_id: Uuid::new_v4(),
        };
        let info = FileInfo {
            file_id: Uuid::new_v4(),
            backend_id: Uuid::new_v4(),
            file_path: "p".to_owned(),
            owner: owner.clone(),
            meta: FileMeta {
                name: "n".to_owned(),
                mime_type: "m".to_owned(),
                gts_file_type: "gts.x".to_owned(),
                size_bytes: None,
                custom_metadata: BTreeMap::new(),
            },
            status: FileStatus::Uploaded,
            etag: "e".to_owned(),
            size_bytes: 0,
            created_at: OffsetDateTime::UNIX_EPOCH,
            modified_at: OffsetDateTime::UNIX_EPOCH,
            upload_expires_at: None,
        };
        let copy = info.clone();
        assert_eq!(info, copy);
    }

    #[test]
    fn file_read_handle_debug_is_non_panicking() {
        let owner = OwnerRef {
            tenant_id: Uuid::new_v4(),
            owner_id: Uuid::new_v4(),
        };
        let info = FileInfo {
            file_id: Uuid::new_v4(),
            backend_id: Uuid::new_v4(),
            file_path: "p".to_owned(),
            owner,
            meta: FileMeta {
                name: "n".to_owned(),
                mime_type: "m".to_owned(),
                gts_file_type: "gts.x".to_owned(),
                size_bytes: None,
                custom_metadata: BTreeMap::new(),
            },
            status: FileStatus::Uploaded,
            etag: "e".to_owned(),
            size_bytes: 0,
            created_at: OffsetDateTime::UNIX_EPOCH,
            modified_at: OffsetDateTime::UNIX_EPOCH,
            upload_expires_at: None,
        };
        let bytes: FileByteStream =
            Box::pin(futures::stream::iter(Vec::<Result<bytes::Bytes, _>>::new()));
        let handle = FileReadHandle { info, bytes };
        let s = format!("{handle:?}");
        assert!(s.contains("FileReadHandle"));
    }
}
