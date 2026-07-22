use toolkit::DatabaseCapability;
use toolkit_utils::SecretString;

use super::*;

#[test]
fn gear_provides_p1_and_p2_migrations() {
    // The DatabaseCapability wiring must hand the runtime all current migrations:
    //   1. P1 initial (control-plane metadata tables)
    //   2. P2 initial (policy store + retention rules + multipart + idempotency
    //      keys + audit outbox + file events outbox, in one step)
    //   3. P2 multipart plan columns (declared_size + part_size on multipart_uploads)
    //   4. P2 remediation 0.10: idempotency_keys.subject_id (binds a replay to
    //      the authenticated caller, not just the request-body owner)
    //   5. P2 remediation 2.1: idempotency_keys.request_hash (binds a replay to
    //      the request body that created it, not just the caller)
    //   6. P2 remediation 2.4: policies partial unique indexes (at most one row
    //      per (tenant_id, scope, scope_owner_id), closing the upsert race)
    //   7. ADR-0006 content-hash modes: file_versions.hash_mode/part_count +
    //      the version_hash_manifest table
    // (init()/register_rest() need a live GearCtx — those seams are covered by
    // the E2E suite, not here.)
    let gear = FileStorageGear::default();
    assert_eq!(
        gear.migrations().len(),
        7,
        "gear must provide the P1, P2 initial, P2 multipart plan columns, P2 \
         remediation 0.10 idempotency subject_id, P2 remediation 2.1 \
         idempotency request_hash, P2 remediation 2.4 policies unique \
         scope, and ADR-0006 content-hash-modes migrations"
    );
}

#[test]
fn gear_default_config_excludes_in_memory_backend() {
    // P2 remediation 0.5: the non-durable `memory` backend must not be part
    // of the registry unless a deployment explicitly opts in — otherwise
    // every deployment silently exposes a volatile backend.
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
    // Opting in via `enable_in_memory_backend: true` registers the `memory`
    // backend alongside the always-present `local-fs` default.
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
    // P2 1.7.3 config wiring: one `s3_backends` entry must become one more
    // backend in the registry. Construction (`S3Backend::from_config` ->
    // `S3Backend::new`) performs no I/O — a bogus/unreachable endpoint is
    // fine here, since this test only checks the registry's contents, never
    // dispatching a real request against it.
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
fn gear_default_backend_id_falls_back_to_local_fs_when_unset() {
    // P2 1.7 Stage 6: an S3-configured deployment that does NOT set
    // `default_backend_id` must keep routing new uploads to `local-fs`,
    // preserving today's behavior — only an explicit override changes it.
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

    let registry = build_backend_registry(&cfg).expect("must build a valid registry");
    assert_eq!(registry.default_id(), "local-fs");
}

#[test]
fn gear_default_backend_id_override_selects_configured_backend() {
    // P2 1.7 Stage 6 e2e wiring: setting `default_backend_id` to a configured
    // `s3_backends` entry's id must make that backend the registry's default,
    // so `create`/`initiate_multipart` mint upload URLs whose
    // `claims.backend_id` names the S3 backend instead of `local-fs`.
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
        default_backend_id: Some("s3-primary".to_owned()),
        ..FileStorageConfig::default()
    };

    let registry = build_backend_registry(&cfg).expect("must build a valid registry");
    assert_eq!(registry.default_id(), "s3-primary");
    assert_eq!(registry.default_backend().id(), "s3-primary");
}

#[test]
fn gear_default_backend_id_unknown_id_fails_fast() {
    // An override naming a backend id that isn't among the configured
    // backends must be a clean init-time `Err`, never a panic.
    let cfg = crate::config::FileStorageConfig {
        default_backend_id: Some("does-not-exist".to_owned()),
        ..FileStorageConfig::default()
    };

    let result = build_backend_registry(&cfg);
    let Err(err) = result else {
        panic!("an unknown default_backend_id must fail registry construction");
    };
    let msg = err.to_string();
    assert!(
        msg.contains("does-not-exist"),
        "error must name the offending backend id: {msg}"
    );
}
