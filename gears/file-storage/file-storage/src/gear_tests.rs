use toolkit::DatabaseCapability;
use toolkit_utils::SecretString;

use crate::config::S3BackendConfig;

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

fn registry_ids(cfg: &FileStorageConfig) -> Vec<String> {
    let registry = build_backend_registry(cfg).expect("config must build a valid registry");
    registry.list().into_iter().map(|(id, _)| id).collect()
}

#[test]
fn gear_in_memory_backend_is_opt_in() {
    // The non-durable `memory` backend is registered only on explicit opt-in.
    for (enabled, expected) in [(false, false), (true, true)] {
        let cfg = FileStorageConfig {
            enable_in_memory_backend: enabled,
            ..FileStorageConfig::default()
        };
        assert_eq!(
            registry_ids(&cfg).iter().any(|id| id == "memory"),
            expected,
            "enable_in_memory_backend={enabled}"
        );
    }
}

#[test]
fn gear_registry_includes_configured_s3_backends_and_resolves_the_default() {
    // Construction performs no I/O, so an unreachable endpoint is fine here.
    // `None` keeps routing new uploads to `local-fs`; naming a configured S3 backend selects it.
    for (default_backend_id, expected) in [(None, "local-fs"), (Some("s3-primary"), "s3-primary")] {
        let cfg = FileStorageConfig {
            s3_backends: vec![S3BackendConfig {
                id: "s3-primary".to_owned(),
                endpoint: Some("http://127.0.0.1:0".to_owned()),
                region: "us-east-1".to_owned(),
                bucket: "test-bucket".to_owned(),
                access_key_id: Some("test-access-key".to_owned()),
                secret_access_key: Some(SecretString::new("test-secret-key")),
                path_style: true,
            }],
            default_backend_id: default_backend_id.map(str::to_owned),
            ..FileStorageConfig::default()
        };
        let registry = build_backend_registry(&cfg).expect("must build a valid registry");
        assert_eq!(registry.default_id(), expected);
        assert_eq!(registry.default_backend().id(), expected);
        let (_, caps) = registry
            .list()
            .into_iter()
            .find(|(id, _)| id == "s3-primary")
            .expect("s3-primary must be present in the registry");
        assert!(
            caps.multipart_native,
            "S3Backend must advertise multipart_native"
        );
    }
}

#[test]
fn gear_default_backend_id_unknown_id_fails_fast() {
    // An unknown `default_backend_id` is an init-time `Err`, not a panic.
    let cfg = FileStorageConfig {
        default_backend_id: Some("does-not-exist".to_owned()),
        ..FileStorageConfig::default()
    };

    let Err(err) = build_backend_registry(&cfg) else {
        panic!("an unknown default_backend_id must fail registry construction");
    };
    assert!(
        err.to_string().contains("does-not-exist"),
        "error must name the offending backend id: {err}"
    );
}
