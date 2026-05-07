#[cfg(test)]
mod tests {
    use super::super::dto::*;
    use file_storage_sdk::{
        Backend, BackendCapability, BackendKind, BackendTransport, FileInfo, FileMeta,
        FileMetaUpdate, FileStatus, OwnerRef, PresignDownloadOutcome, PresignedDownload,
        PresignedUploadHandle, UrlParams,
    };
    use std::collections::BTreeMap;
    use time::OffsetDateTime;
    use uuid::Uuid;

    // ── Backend → BackendDto ────────────────────────────────────────────────

    #[test]
    fn backend_to_dto_maps_kind_and_transport() {
        let id = Uuid::new_v4();
        let b = Backend {
            id,
            kind: BackendKind::S3Compatible,
            default_public: false,
            default_private: true,
            transport: BackendTransport::Redirect,
            capabilities: vec![
                BackendCapability::PresignedUrls,
                BackendCapability::PublicReadUrls,
                BackendCapability::PresignedConditionalPut,
            ],
            max_file_size_bytes: Some(1024),
        };
        let dto: BackendDto = b.into();
        assert_eq!(dto.id, id);
        assert_eq!(dto.kind, "s3-compatible");
        assert!(dto.default_private);
        assert!(!dto.default_public);
        assert_eq!(dto.transport, "redirect");
        assert_eq!(dto.capabilities.len(), 3);
        assert!(dto.capabilities.contains(&"presigned_urls".to_owned()));
        assert!(dto.capabilities.contains(&"public_read_urls".to_owned()));
        assert!(
            dto.capabilities
                .contains(&"presigned_conditional_put".to_owned())
        );
        assert_eq!(dto.max_file_size_bytes, Some(1024));
    }

    // ── OwnerRef ↔ OwnerRefDto round-trip ───────────────────────────────────

    #[test]
    fn owner_ref_round_trip() {
        let tenant_id = Uuid::new_v4();
        let owner_id = Uuid::new_v4();
        let owner = OwnerRef {
            tenant_id,
            owner_id,
        };
        let dto: OwnerRefDto = owner.clone().into();
        assert_eq!(dto.tenant_id, tenant_id);
        assert_eq!(dto.owner_id, owner_id);
        let back: OwnerRef = dto.into();
        assert_eq!(back, owner);
    }

    // ── FileMeta ↔ FileMetaDto round-trip ───────────────────────────────────

    #[test]
    fn file_meta_round_trip() {
        let mut custom = BTreeMap::new();
        custom.insert("k".to_owned(), "v".to_owned());
        let meta = FileMeta {
            name: "doc".to_owned(),
            mime_type: "text/plain".to_owned(),
            gts_file_type: "gts.x.foo".to_owned(),
            size_bytes: Some(42),
            custom_metadata: custom,
        };
        let dto: FileMetaDto = meta.clone().into();
        let back: FileMeta = dto.into();
        assert_eq!(back, meta);
    }

    // ── FileUpdateRequest discrimination tests ──────────────────────────────

    #[test]
    fn file_update_default_has_neither_branch() {
        let dto = FileUpdateRequest::default();
        assert!(!dto.has_status_branch());
        assert!(!dto.has_metadata_branch());
    }

    #[test]
    fn file_update_with_status_only_is_status_branch() {
        let dto = FileUpdateRequest {
            status: Some(FileStatusDto::Uploaded),
            new_etag: Some("e".to_owned()),
            ..Default::default()
        };
        assert!(dto.has_status_branch());
        assert!(!dto.has_metadata_branch());
    }

    #[test]
    fn file_update_with_metadata_only_is_metadata_branch() {
        let dto = FileUpdateRequest {
            name: Some("renamed".to_owned()),
            ..Default::default()
        };
        assert!(!dto.has_status_branch());
        assert!(dto.has_metadata_branch());
        let upd: FileMetaUpdate = dto.into_metadata_update();
        assert_eq!(upd.name.as_deref(), Some("renamed"));
        assert!(upd.mime_type.is_none());
    }

    // ── UrlParams ↔ UrlParamsDto ────────────────────────────────────────────

    #[test]
    fn url_params_dto_into_url_params() {
        let dto = UrlParamsDto {
            expires_in_seconds: 600,
            content_disposition: Some("attachment".to_owned()),
            content_type_override: None,
            allowed_client_cidrs: vec!["10.0.0.0/8".to_owned()],
            refresh_etag: true,
        };
        let p: UrlParams = dto.into();
        assert_eq!(p.expires_in_seconds, 600);
        assert_eq!(p.content_disposition.as_deref(), Some("attachment"));
        assert!(p.refresh_etag);
        assert_eq!(p.allowed_client_cidrs, vec!["10.0.0.0/8".to_owned()]);
    }

    // ── PresignedUploadHandle → DTO ─────────────────────────────────────────

    #[test]
    fn presigned_upload_handle_to_dto() {
        let h = PresignedUploadHandle {
            file_id: Uuid::nil(),
            upload_url: "https://example/u".to_owned(),
            etag_pinned: "etag".to_owned(),
            expires_at: OffsetDateTime::UNIX_EPOCH,
        };
        let dto: PresignedUploadHandleDto = h.into();
        assert_eq!(dto.upload_url, "https://example/u");
        assert_eq!(dto.etag_pinned, "etag");
    }

    // ── FileStatus ↔ FileStatusDto round-trip ───────────────────────────────

    #[test]
    fn file_status_round_trip_through_dto() {
        for s in [FileStatus::PendingUpload, FileStatus::Uploaded] {
            let dto: FileStatusDto = s.into();
            let back: FileStatus = dto.into();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn file_status_dto_serializes_snake_case() {
        let json = serde_json::to_string(&FileStatusDto::PendingUpload).unwrap();
        assert_eq!(json, r#""pending_upload""#);
    }

    // ── FileInfo → DTO ──────────────────────────────────────────────────────

    #[test]
    fn file_info_to_dto_preserves_fields() {
        let owner = OwnerRef {
            tenant_id: Uuid::new_v4(),
            owner_id: Uuid::new_v4(),
        };
        let backend_id = Uuid::new_v4();
        let info = FileInfo {
            file_id: Uuid::new_v4(),
            backend_id,
            file_path: "a/b".to_owned(),
            owner: owner.clone(),
            meta: FileMeta {
                name: "f".to_owned(),
                mime_type: "text/plain".to_owned(),
                gts_file_type: "gts.x".to_owned(),
                size_bytes: Some(10),
                custom_metadata: BTreeMap::new(),
            },
            status: FileStatus::Uploaded,
            etag: "etag-1".to_owned(),
            size_bytes: 10,
            created_at: OffsetDateTime::UNIX_EPOCH,
            modified_at: OffsetDateTime::UNIX_EPOCH,
            upload_expires_at: None,
        };
        let dto: FileInfoDto = info.clone().into();
        assert_eq!(dto.file_id, info.file_id);
        assert_eq!(dto.backend_id, backend_id);
        assert_eq!(dto.etag, "etag-1");
        assert_eq!(dto.size_bytes, 10);
        assert!(matches!(dto.status, FileStatusDto::Uploaded));
    }

    // ── PresignDownloadOutcome → PresignOutcomeDto ──────────────────────────

    #[test]
    fn presign_download_outcome_ok_path() {
        let file_id = Uuid::new_v4();
        let outcome = PresignDownloadOutcome {
            file_id,
            result: Ok(PresignedDownload {
                url: "https://example/d".to_owned(),
                expires_at: OffsetDateTime::UNIX_EPOCH,
                is_public: false,
            }),
        };
        let dto: PresignOutcomeDto = outcome.into();
        assert_eq!(dto.kind, "download");
        assert!(dto.ok_download.is_some());
        assert!(dto.error.is_none());
        assert_eq!(dto.ok_download.unwrap().url, "https://example/d");
    }

    #[test]
    fn presign_download_outcome_err_path() {
        let file_id = Uuid::new_v4();
        let outcome = PresignDownloadOutcome {
            file_id,
            result: Err(file_storage_sdk::FileStorageError::NotFound),
        };
        let dto: PresignOutcomeDto = outcome.into();
        assert_eq!(dto.kind, "download");
        assert!(dto.ok_download.is_none());
        assert!(dto.error.is_some());
        assert!(dto.error.unwrap().contains("not found"));
    }

    #[test]
    fn presigned_download_to_dto() {
        let d = PresignedDownload {
            url: "u".to_owned(),
            expires_at: OffsetDateTime::UNIX_EPOCH,
            is_public: true,
        };
        let dto: PresignedDownloadDto = d.into();
        assert_eq!(dto.url, "u");
        assert!(dto.is_public);
    }

    // ── PresignItemDto serde ────────────────────────────────────────────────

    #[test]
    fn presign_item_dto_deserialises_upload_kind() {
        let tenant = Uuid::new_v4();
        let owner = Uuid::new_v4();
        let json = format!(
            r#"{{"kind":"upload","owner":{{"tenant_id":"{tenant}","owner_id":"{owner}"}},"file_path":"p","meta":{{"name":"n","mime_type":"m","gts_file_type":"gts.x","custom_metadata":{{}}}},"params":{{"expires_in_seconds":600,"content_disposition":null,"content_type_override":null,"allowed_client_cidrs":[],"refresh_etag":false}}}}"#
        );
        let dto: PresignItemDto = serde_json::from_str(&json).unwrap();
        assert!(matches!(dto, PresignItemDto::Upload(_)));
    }

    #[test]
    fn presign_item_dto_deserialises_download_kind() {
        let id = Uuid::new_v4();
        let json = format!(
            r#"{{"kind":"download","file_id":"{id}","params":{{"expires_in_seconds":600,"content_disposition":null,"content_type_override":null,"allowed_client_cidrs":[],"refresh_etag":false}},"etag":null}}"#
        );
        let dto: PresignItemDto = serde_json::from_str(&json).unwrap();
        assert!(matches!(dto, PresignItemDto::Download(_)));
    }
}
