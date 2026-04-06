use super::*;
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn test_extract_file_path_from_dsn() {
    // Absolute path
    assert_eq!(
        extract_file_path_from_dsn("sqlite:///absolute/path/to/db.sqlite"),
        Some(PathBuf::from("/absolute/path/to/db.sqlite"))
    );

    // Relative path (URL parsing normalizes ./ to /)
    assert_eq!(
        extract_file_path_from_dsn("sqlite://./relative/path/to/db.sqlite"),
        Some(PathBuf::from("/relative/path/to/db.sqlite"))
    );

    // Simple sqlite: prefix
    assert_eq!(
        extract_file_path_from_dsn("sqlite:test.db"),
        Some(PathBuf::from("test.db"))
    );

    // With query parameters
    assert_eq!(
        extract_file_path_from_dsn("sqlite:///path/to/db.sqlite?wal=true"),
        Some(PathBuf::from("/path/to/db.sqlite"))
    );

    // Memory databases
    assert_eq!(extract_file_path_from_dsn("sqlite::memory:"), None);
    assert_eq!(extract_file_path_from_dsn("sqlite://memory:"), None);
    assert_eq!(
        extract_file_path_from_dsn("sqlite:///test.db?mode=memory"),
        None
    );

    // Plain file path
    assert_eq!(
        extract_file_path_from_dsn("/plain/file/path.db"),
        Some(PathBuf::from("/plain/file/path.db"))
    );
}

#[test]
fn test_prepare_sqlite_path_memory() {
    // Memory databases should be returned unchanged
    assert_eq!(
        prepare_sqlite_path("sqlite::memory:", true).unwrap(),
        "sqlite::memory:"
    );
    assert_eq!(
        prepare_sqlite_path("sqlite://memory:", false).unwrap(),
        "sqlite://memory:"
    );
    assert_eq!(
        prepare_sqlite_path("sqlite:///test.db?mode=memory", true).unwrap(),
        "sqlite:///test.db?mode=memory"
    );
}

#[test]
fn test_prepare_sqlite_path_no_create_dirs() {
    // When create_dirs is false, should return DSN unchanged
    let dsn = "sqlite:///some/path/db.sqlite";
    assert_eq!(prepare_sqlite_path(dsn, false).unwrap(), dsn);
}

#[test]
fn test_prepare_sqlite_path_create_dirs() {
    // Use a unique temp directory to avoid collisions during parallel test execution
    let temp_dir = TempDir::new().unwrap();
    let test_path = temp_dir.path().join("db.sqlite");

    // Convert to string with forward slashes for cross-platform SQLite DSN
    let path_str = test_path.to_string_lossy().replace('\\', "/");
    let dsn = format!("sqlite:///{}", path_str.trim_start_matches('/'));

    let result = prepare_sqlite_path(&dsn, true);
    assert!(result.is_ok(), "Failed to prepare path: {:?}", result.err());
    assert_eq!(result.unwrap(), dsn);

    // Verify the directory was created
    let parent = test_path.parent().unwrap();
    assert!(parent.exists(), "Parent directory should exist");

    // TempDir automatically cleans up when dropped
}
