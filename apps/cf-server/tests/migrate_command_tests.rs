#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the `migrate` command
//!
//! These tests verify that the migrate CLI command works correctly
//! by invoking the cf-server binary and checking its output.

use std::process::Command;

/// Helper to get the path to the cf-server binary
fn cyberfabric_binary() -> &'static str {
    env!("CARGO_BIN_EXE_cf-server")
}

#[test]
fn test_migrate_command_help_text() {
    let output = Command::new(cyberfabric_binary())
        .args(["migrate", "--help"])
        .output()
        .expect("failed to execute cf-server");

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Run database migrations and exit"),
        "Help text should describe migrate command"
    );
}

#[test]
fn test_migrate_command_runs_migration_phases() {
    let output = Command::new(cyberfabric_binary())
        .arg("--config")
        .arg("../../config/e2e-local.yaml")
        .arg("migrate")
        .output()
        .expect("failed to execute cf-server");

    // Should complete successfully (with or without actual database)
    assert!(
        output.status.success(),
        "migrate command should exit successfully. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
