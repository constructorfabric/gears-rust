#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the `migrate` command
//!
//! These tests verify that the migrate CLI command works correctly
//! by invoking the cf-gears-server binary and checking its output.

use std::process::Command;

/// Helper to get the path to the cf-gears-server binary
fn cf_gears_binary() -> String {
    std::env::var("CARGO_BIN_EXE_cf-gears-example-server")
        .or_else(|_| std::env::var("CARGO_BIN_EXE_CF_GEARS_EXAMPLE_SERVER"))
        .expect("CARGO_BIN_EXE_cf-gears-example-server must be set for tests")
}

#[test]
fn test_migrate_command_help_text() {
    let output = Command::new(cf_gears_binary())
        .args(["migrate", "--help"])
        .output()
        .expect("failed to execute cf-gears-server");

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Run database migrations and exit"),
        "Help text should describe migrate command"
    );
}

#[test]
fn test_migrate_command_runs_migration_phases() {
    // A fresh data root: the default is the developer's home directory, where a
    // database left by an older migration chain makes this test fail, and where
    // a test has no business creating or migrating anything.
    let home = tempfile::tempdir().expect("temporary data root");
    let output = Command::new(cf_gears_binary())
        .env("APP__SERVER__HOME_DIR", home.path())
        .arg("--config")
        .arg("../../config/e2e-local.yaml")
        .arg("migrate")
        .output()
        .expect("failed to execute cf-gears-server");

    // Should complete successfully (with or without actual database)
    assert!(
        output.status.success(),
        "migrate command should exit successfully. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
