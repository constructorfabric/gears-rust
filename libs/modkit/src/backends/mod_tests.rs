use super::*;
use std::path::PathBuf;

#[test]
fn test_oop_module_config_builder() {
    let mut cfg = OopModuleConfig::new("my_module", BackendKind::LocalProcess);
    cfg.binary = Some(PathBuf::from("/usr/bin/myapp"));
    cfg.args = vec!["--port".to_owned(), "8080".to_owned()];
    cfg.env.insert("LOG_LEVEL".to_owned(), "debug".to_owned());
    cfg.version = Some("1.0.0".to_owned());

    assert_eq!(cfg.name, "my_module");
    assert_eq!(cfg.backend, BackendKind::LocalProcess);
    assert_eq!(cfg.binary, Some(PathBuf::from("/usr/bin/myapp")));
    assert_eq!(cfg.args.len(), 2);
    assert_eq!(cfg.env.len(), 1);
    assert_eq!(cfg.version, Some("1.0.0".to_owned()));
}

#[test]
fn test_backend_kind_equality() {
    assert_eq!(BackendKind::LocalProcess, BackendKind::LocalProcess);
    assert_ne!(BackendKind::LocalProcess, BackendKind::K8s);
    assert_ne!(BackendKind::K8s, BackendKind::Static);
    assert_ne!(BackendKind::Static, BackendKind::Mock);
}

#[test]
fn test_instance_handle_debug() {
    let instance_id = Uuid::new_v4();
    let handle = InstanceHandle {
        module: "test_module".to_owned(),
        instance_id,
        backend: BackendKind::LocalProcess,
        pid: Some(12345),
        created_at: Instant::now(),
    };

    let debug_str = format!("{handle:?}");
    assert!(debug_str.contains("test_module"));
    assert!(debug_str.contains(&instance_id.to_string()));
    assert!(debug_str.contains("LocalProcess"));
    assert!(debug_str.contains("12345"));
}
