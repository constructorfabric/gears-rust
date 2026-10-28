use toolkit::DatabaseCapability;
use toolkit_utils::SecretString;

use super::*;

#[test]
fn gear_hands_the_runtime_distinct_migrations() {
    let migrations = FileStorageGear::default().migrations();
    let names: std::collections::HashSet<String> =
        migrations.iter().map(|m| m.name().to_owned()).collect();
    assert!(!names.is_empty(), "the gear must provide its migrations");
    assert_eq!(
        names.len(),
        migrations.len(),
        "migration names must be unique"
    );
}

#[test]
fn gear_default_config_excludes_in_memory_backend() {
    // The non-durable `memory` backend is registered only on explicit opt-in.
    let cfg = FileStorageConfig::default();
    assert!(!cfg.enable_in_memory_backend);

    let registry =
        build_backend_registry(&cfg).expect("default config must build a valid registry");
    assert!(
        registry.list().iter().all(|(id, _)| id != "memory"),
        "memory backend must be absent by default"
    );
}

#[test]
fn gear_dev_flag_enables_in_memory_backend() {
    let cfg = FileStorageConfig {
        enable_in_memory_backend: true,
        ..FileStorageConfig::default()
    };

    let registry = build_backend_registry(&cfg).expect("dev config must build a valid registry");
    assert!(
        registry.list().iter().any(|(id, _)| id == "memory"),
        "memory backend must be present when enable_in_memory_backend is set"
    );
}

#[test]
fn gear_registry_includes_configured_s3_backends() {
    // Construction performs no I/O, so an unreachable endpoint is fine here.
    let cfg = crate::config::FileStorageConfig {
        s3_backends: vec![crate::config::S3BackendConfig {
            id: "s3-primary".to_owned(),
            endpoint: Some("http://127.0.0.1:0".to_owned()),
            region: "us-east-1".to_owned(),
            bucket: "test-bucket".to_owned(),
            access_key_id: Some("test-access-key".to_owned()),
            secret_access_key: Some(SecretString::new("test-secret-key")),
            path_style: true,
        }],
        ..FileStorageConfig::default()
    };

    let registry =
        build_backend_registry(&cfg).expect("config with a valid S3 entry must build a registry");
    let entry = registry
        .list()
        .into_iter()
        .find(|(id, _)| id == "s3-primary")
        .expect("s3-primary must be present in the registry");
    assert!(
        entry.1.multipart_native,
        "S3Backend must advertise multipart_native: true (Stage 2)"
    );
}

#[test]
fn gear_default_backend_id_resolution() {
    // `None` keeps routing new uploads to `local-fs`; naming a configured S3 backend selects it.
    let s3_backends = || {
        vec![crate::config::S3BackendConfig {
            id: "s3-primary".to_owned(),
            endpoint: Some("http://127.0.0.1:0".to_owned()),
            region: "us-east-1".to_owned(),
            bucket: "test-bucket".to_owned(),
            access_key_id: Some("test-access-key".to_owned()),
            secret_access_key: Some(SecretString::new("test-secret-key")),
            path_style: true,
        }]
    };
    for (default_backend_id, expected) in [(None, "local-fs"), (Some("s3-primary"), "s3-primary")] {
        let cfg = crate::config::FileStorageConfig {
            s3_backends: s3_backends(),
            default_backend_id: default_backend_id.map(str::to_owned),
            ..FileStorageConfig::default()
        };
        let registry = build_backend_registry(&cfg).expect("must build a valid registry");
        assert_eq!(registry.default_id(), expected);
        assert_eq!(registry.default_backend().id(), expected);
    }
}

#[test]
fn gear_default_backend_id_unknown_id_fails_fast() {
    // An unknown `default_backend_id` is an init-time `Err`, not a panic.
    let cfg = crate::config::FileStorageConfig {
        default_backend_id: Some("does-not-exist".to_owned()),
        ..FileStorageConfig::default()
    };

    let result = build_backend_registry(&cfg);
    let Err(err) = result else {
        panic!("an unknown default_backend_id must fail registry construction");
    };
    assert!(
        err.to_string().contains("does-not-exist"),
        "error must name the offending backend id: {err}"
    );
}
