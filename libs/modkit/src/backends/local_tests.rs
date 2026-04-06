use super::*;
use std::path::PathBuf;
use std::time::Instant;

fn test_backend() -> LocalProcessBackend {
    LocalProcessBackend::new(CancellationToken::new())
}

#[tokio::test]
async fn test_spawn_instance_requires_binary() {
    let backend = test_backend();
    let cfg = OopModuleConfig::new("test_module", BackendKind::LocalProcess);

    let result = backend.spawn_instance(&cfg).await;
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("executable_path must be set")
    );
}

#[tokio::test]
async fn test_spawn_instance_requires_correct_backend() {
    let backend = test_backend();
    let mut cfg = OopModuleConfig::new("test_module", BackendKind::K8s);
    cfg.binary = Some(PathBuf::from("/bin/echo"));

    let result = backend.spawn_instance(&cfg).await;
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("can only spawn LocalProcess")
    );
}

#[tokio::test]
async fn test_spawn_list_stop_lifecycle() {
    let backend = test_backend();

    // Create config with a valid binary that exists on most systems
    let mut cfg = OopModuleConfig::new("test_module", BackendKind::LocalProcess);

    // Use a simple command that exists cross-platform
    #[cfg(windows)]
    let binary = PathBuf::from("C:\\Windows\\System32\\cmd.exe");
    #[cfg(not(windows))]
    let binary = PathBuf::from("/bin/sleep");

    cfg.binary = Some(binary);
    cfg.args = vec!["10".to_owned()]; // sleep for 10 seconds

    // Spawn instance
    let handle = backend
        .spawn_instance(&cfg)
        .await
        .expect("should spawn instance");

    assert_eq!(handle.module, "test_module");
    assert!(!handle.instance_id.is_nil());
    assert_eq!(handle.backend, BackendKind::LocalProcess);

    // List instances
    let instances = backend
        .list_instances("test_module")
        .await
        .expect("should list instances");
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].module, "test_module");
    assert_eq!(instances[0].instance_id, handle.instance_id);

    // Stop instance
    backend
        .stop_instance(&handle)
        .await
        .expect("should stop instance");

    // Verify it's removed
    let instances = backend
        .list_instances("test_module")
        .await
        .expect("should list instances");
    assert_eq!(instances.len(), 0);
}

#[tokio::test]
async fn test_list_instances_filters_by_module() {
    let backend = test_backend();

    #[cfg(windows)]
    let binary = PathBuf::from("C:\\Windows\\System32\\cmd.exe");
    #[cfg(not(windows))]
    let binary = PathBuf::from("/bin/sleep");

    // Spawn instance for module_a
    let mut cfg_a = OopModuleConfig::new("module_a", BackendKind::LocalProcess);
    cfg_a.binary = Some(binary.clone());
    cfg_a.args = vec!["10".to_owned()];

    let handle_a = backend
        .spawn_instance(&cfg_a)
        .await
        .expect("should spawn module_a");

    // Spawn instance for module_b
    let mut cfg_b = OopModuleConfig::new("module_b", BackendKind::LocalProcess);
    cfg_b.binary = Some(binary);
    cfg_b.args = vec!["10".to_owned()];

    let handle_b = backend
        .spawn_instance(&cfg_b)
        .await
        .expect("should spawn module_b");

    // List module_a instances
    let instances_a = backend
        .list_instances("module_a")
        .await
        .expect("should list module_a");
    assert_eq!(instances_a.len(), 1);
    assert_eq!(instances_a[0].module, "module_a");

    // List module_b instances
    let instances_b = backend
        .list_instances("module_b")
        .await
        .expect("should list module_b");
    assert_eq!(instances_b.len(), 1);
    assert_eq!(instances_b[0].module, "module_b");

    // Clean up
    backend.stop_instance(&handle_a).await.ok();
    backend.stop_instance(&handle_b).await.ok();
}

#[tokio::test]
async fn test_stop_nonexistent_instance() {
    let backend = test_backend();
    let handle = InstanceHandle {
        module: "test_module".to_owned(),
        instance_id: Uuid::new_v4(),
        backend: BackendKind::LocalProcess,
        pid: None,
        created_at: Instant::now(),
    };

    // Should not error even if instance doesn't exist
    let result = backend.stop_instance(&handle).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_list_instances_empty() {
    let backend = test_backend();
    let instances = backend
        .list_instances("nonexistent_module")
        .await
        .expect("should list instances");
    assert_eq!(instances.len(), 0);
}

mod send_terminate_signal_tests {
    #[cfg(unix)]
    use {super::send_terminate_signal, std::time::Duration};

    #[cfg(unix)]
    #[tokio::test]
    async fn test_send_terminate_signal_to_valid_process() {
        // Spawn a long-running process
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.args(["30"]);

        let mut child = cmd.spawn().expect("should spawn test process");

        // Send termination signal
        let result = send_terminate_signal(&child);

        // Should return true indicating signal was sent
        assert!(result, "Should successfully send SIGTERM to valid process");

        // Wait briefly for graceful shutdown
        tokio::time::timeout(Duration::from_secs(1), child.wait())
            .await
            .expect("process should exit within timeout")
            .expect("wait should succeed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_send_terminate_signal_to_exited_process() {
        // Spawn a process that exits immediately using sh -c 'exit 0'
        // This works on all Unix systems (Linux, macOS, BSD)
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", "exit 0"]);
        let mut child = cmd.spawn().expect("should spawn test process");

        // Wait for it to exit
        tokio::time::timeout(Duration::from_millis(100), child.wait())
            .await
            .expect("process should exit within timeout")
            .expect("wait should succeed");

        // Try to send termination signal to exited process
        let result = send_terminate_signal(&child);

        // Should return false because PID is no longer available
        assert!(!result, "Should return false for already-exited process");
    }

    #[cfg(unix)]
    #[test]
    fn test_pid_conversion_edge_case_documentation() {
        // This test documents the edge case behavior for PIDs > i32::MAX
        // In practice, this is extremely rare as it would require:
        // 1. System uptime of weeks/months without reboot
        // 2. PID counter to wrap around multiple times
        // 3. Specific kernel configuration

        // The maximum value a u32 PID can have
        let max_u32_pid: u32 = u32::MAX;

        // This would fail to convert to i32
        let result = i32::try_from(max_u32_pid);
        assert!(result.is_err(), "u32::MAX should not fit in i32");

        // Our code handles this by logging a warning and returning false
        // preventing the dangerous unwrap_or(0) that would signal PID 0
    }

    #[cfg(unix)]
    #[test]
    fn test_pid_conversion_normal_range() {
        // Test that normal PIDs convert successfully
        let normal_pid: u32 = 12345;
        let result = i32::try_from(normal_pid);
        assert!(result.is_ok(), "Normal PID should convert to i32");
        assert_eq!(result.unwrap(), 12345);
    }
}
