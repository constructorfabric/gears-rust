#[cfg(test)]
mod tests {
    use super::super::config::{
        BackendConfig, BackendKindCfg, FileStorageConfig, validate_config,
    };
    use uuid::Uuid;

    fn make_backend(id: Uuid) -> BackendConfig {
        BackendConfig {
            id,
            kind: BackendKindCfg::S3Compatible,
            default_public: false,
            default_private: false,
            endpoint: "http://localhost".to_owned(),
            region: "us-east-1".to_owned(),
            bucket: "bucket".to_owned(),
            access_key: "ak".to_owned(),
            secret_key: "sk".to_owned(),
            max_file_size_bytes: None,
            max_signed_url_ttl_seconds: 3600,
            public_read_urls: false,
            presigned_conditional_put: false,
            tenant_access: vec![],
        }
    }

    fn make_cfg(backends: Vec<BackendConfig>) -> FileStorageConfig {
        FileStorageConfig {
            default_public_storage_id: None,
            default_private_storage_id: None,
            orphan_delete_grace_seconds: 86_400,
            signed_url_clock_skew_margin_seconds: 60,
            backends,
        }
    }

    #[test]
    fn empty_roster_is_valid() {
        let cfg = make_cfg(vec![]);
        assert!(validate_config(&cfg).is_ok());
    }

    #[test]
    fn single_backend_with_default_private_is_valid() {
        let mut b = make_backend(Uuid::new_v4());
        b.default_private = true;
        let cfg = make_cfg(vec![b]);
        assert!(validate_config(&cfg).is_ok());
    }

    #[test]
    fn rejects_two_default_public_backends() {
        let mut a = make_backend(Uuid::new_v4());
        a.default_public = true;
        a.default_private = true;
        let mut b = make_backend(Uuid::new_v4());
        b.default_public = true;
        let cfg = make_cfg(vec![a, b]);
        let err = validate_config(&cfg).unwrap_err();
        assert!(err.contains("default_public"), "got: {err}");
    }

    #[test]
    fn rejects_two_default_private_backends() {
        let mut a = make_backend(Uuid::new_v4());
        a.default_private = true;
        let mut b = make_backend(Uuid::new_v4());
        b.default_private = true;
        let cfg = make_cfg(vec![a, b]);
        let err = validate_config(&cfg).unwrap_err();
        assert!(err.contains("default_private"), "got: {err}");
    }

    #[test]
    fn rejects_non_empty_roster_without_default_private() {
        let cfg = make_cfg(vec![make_backend(Uuid::new_v4())]);
        let err = validate_config(&cfg).unwrap_err();
        assert!(err.contains("default_private"), "got: {err}");
    }

    #[test]
    fn rejects_orphan_grace_below_max_ttl_plus_skew() {
        let mut b = make_backend(Uuid::new_v4());
        b.default_private = true;
        b.max_signed_url_ttl_seconds = 3600;
        let cfg = FileStorageConfig {
            default_public_storage_id: None,
            default_private_storage_id: Some(b.id),
            orphan_delete_grace_seconds: 3000,
            signed_url_clock_skew_margin_seconds: 60,
            backends: vec![b],
        };
        let err = validate_config(&cfg).unwrap_err();
        assert!(err.contains("orphan_delete_grace_seconds"), "got: {err}");
    }

    #[test]
    fn accepts_orphan_grace_exactly_equal_to_max_ttl_plus_skew() {
        let mut b = make_backend(Uuid::new_v4());
        b.default_private = true;
        b.max_signed_url_ttl_seconds = 3600;
        let cfg = FileStorageConfig {
            default_public_storage_id: None,
            default_private_storage_id: Some(b.id),
            orphan_delete_grace_seconds: 3660,
            signed_url_clock_skew_margin_seconds: 60,
            backends: vec![b],
        };
        assert!(validate_config(&cfg).is_ok());
    }

    #[test]
    fn computes_max_ttl_across_multiple_backends() {
        let mut a = make_backend(Uuid::new_v4());
        a.default_private = true;
        a.max_signed_url_ttl_seconds = 100;
        let mut b = make_backend(Uuid::new_v4());
        b.max_signed_url_ttl_seconds = 5000;
        let cfg = FileStorageConfig {
            default_public_storage_id: None,
            default_private_storage_id: Some(a.id),
            orphan_delete_grace_seconds: 4000, // < 5000 + 60
            signed_url_clock_skew_margin_seconds: 60,
            backends: vec![a, b],
        };
        let err = validate_config(&cfg).unwrap_err();
        assert!(err.contains("orphan_delete_grace_seconds"), "got: {err}");
    }

    #[test]
    fn kind_accepts_s3_compatible_string() {
        let id = Uuid::new_v4();
        let json = format!(r#"{{
            "default_private_storage_id": "{id}",
            "backends": [{{
                "id": "{id}",
                "kind": "s3-compatible",
                "default_private": true,
                "endpoint": "http://localhost",
                "region": "us-east-1",
                "bucket": "b",
                "access_key": "ak",
                "secret_key": "sk"
            }}]
        }}"#);
        let cfg: FileStorageConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg.backends.len(), 1);
        assert_eq!(cfg.backends[0].kind, BackendKindCfg::S3Compatible);
    }

    #[test]
    fn kind_rejects_non_s3_compatible_string() {
        let id = Uuid::new_v4();
        let json = format!(r#"{{
            "default_private_storage_id": "{id}",
            "backends": [{{
                "id": "{id}",
                "kind": "webdav",
                "default_private": true,
                "endpoint": "http://localhost",
                "region": "us-east-1",
                "bucket": "b",
                "access_key": "ak",
                "secret_key": "sk"
            }}]
        }}"#);
        let res: Result<FileStorageConfig, _> = serde_json::from_str(&json);
        assert!(res.is_err(), "non-s3 kind must be rejected");
    }

    #[test]
    fn defaults_apply_when_optional_fields_missing() {
        let json = r#"{}"#;
        let cfg: FileStorageConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.orphan_delete_grace_seconds, 86_400);
        assert_eq!(cfg.signed_url_clock_skew_margin_seconds, 60);
        assert!(cfg.backends.is_empty());
        assert!(cfg.default_public_storage_id.is_none());
    }

    #[test]
    fn default_impl_yields_known_field_values() {
        let cfg = FileStorageConfig::default();
        assert_eq!(cfg.orphan_delete_grace_seconds, 86_400);
        assert_eq!(cfg.signed_url_clock_skew_margin_seconds, 60);
        assert!(cfg.backends.is_empty());
        assert!(cfg.default_private_storage_id.is_none());
    }
}
