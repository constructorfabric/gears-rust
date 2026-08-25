//! `SQLite` path preparation and stable scope-identity utilities.
//!
//! This module is the **single source of truth** for interpreting `SQLite` DSNs:
//! memory detection, file-path extraction, Windows drive handling, lexical
//! normalization, and stable absolute identities used by advisory locks.
//!
//! Helpers here are `pub(crate)` and are **not** re-exported from the crate root.

use std::io;
use std::path::{Component, Path, PathBuf};

use super::dsn::is_memory_dsn;

/// Where a `SQLite` DSN points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SqliteDatabaseLocation {
    /// In-memory database (`sqlite::memory:`, `mode=memory`, …).
    Memory { identity: String },
    /// File-backed database.
    File { path: PathBuf },
}

/// Classify a `SQLite` DSN into memory vs file location.
///
/// Returns [`None`] when the DSN is not a recognized memory DSN and no file path can be
/// extracted (malformed / empty), rather than pretending it is in-memory.
#[must_use]
pub(crate) fn sqlite_database_location(dsn: &str) -> Option<SqliteDatabaseLocation> {
    if is_memory_dsn(dsn) {
        return Some(SqliteDatabaseLocation::Memory {
            identity: dsn.to_owned(),
        });
    }
    extract_file_path_from_dsn(dsn).map(|path| SqliteDatabaseLocation::File { path })
}

/// Stable cross-process identity string for advisory-lock `database_scope`.
///
/// File paths are lexically normalized. When the path (or a parent) exists it is resolved with
/// `canonicalize`, including intermediate symlinks: the nearest existing ancestor is
/// canonicalized and any missing lexical suffix is appended. Relative paths are first resolved
/// against the current directory.
#[must_use]
pub(crate) fn sqlite_scope_identity(dsn: &str) -> String {
    match sqlite_database_location(dsn) {
        Some(SqliteDatabaseLocation::Memory { identity }) => {
            format!("sqlite:memory:{identity}")
        }
        Some(SqliteDatabaseLocation::File { path }) => {
            format!("sqlite:file:{}", stable_file_path_identity(&path).display())
        }
        None => format!("sqlite:dsn:{dsn}"),
    }
}

/// Build a stable absolute path identity for lock scoping.
fn stable_file_path_identity(path: &Path) -> PathBuf {
    let lexical = normalize_path_lexically(path);
    let absolute = if lexical.is_absolute() {
        lexical
    } else {
        match std::env::current_dir() {
            Ok(cwd) => {
                let cwd_base =
                    std::fs::canonicalize(&cwd).unwrap_or_else(|_| normalize_path_lexically(&cwd));
                normalize_path_lexically(&cwd_base.join(lexical))
            }
            Err(_) => return lexical,
        }
    };

    if let Ok(canon) = std::fs::canonicalize(&absolute) {
        return canon;
    }

    canonicalize_nearest_ancestor(&absolute)
}

/// Canonicalize the nearest existing ancestor and append the remaining lexical tail.
///
/// This keeps scope identity stable when an intermediate directory is a symlink and the leaf
/// file does not exist yet (plain `canonicalize(full_path)` would fail and skip symlink resolution).
fn canonicalize_nearest_ancestor(path: &Path) -> PathBuf {
    let mut ancestor = path.to_path_buf();
    let mut missing_suffix: Vec<std::ffi::OsString> = Vec::new();

    loop {
        if let Ok(canon) = std::fs::canonicalize(&ancestor) {
            let mut out = canon;
            for component in missing_suffix.iter().rev() {
                out.push(component);
            }
            return out;
        }

        let Some(name) = ancestor.file_name() else {
            return path.to_path_buf();
        };
        let Some(parent) = ancestor.parent() else {
            return path.to_path_buf();
        };
        if parent.as_os_str().is_empty() || parent == ancestor {
            return path.to_path_buf();
        }

        missing_suffix.push(name.to_os_string());
        ancestor = parent.to_path_buf();
    }
}

/// Collapse `.` / `..` without requiring the path to exist.
///
/// - Removes `.`
/// - Collapses `name/..`
/// - Preserves leading `..` on relative paths (`../../data` stays `../../data`)
/// - Does not climb above [`Component::RootDir`] / prefix on absolute paths
/// - Preserves Windows [`Component::Prefix`]
#[must_use]
pub(crate) fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::Prefix(_) | Component::RootDir) => {
                    // Absolute: ignore `..` that would escape the root.
                }
                Some(Component::ParentDir) | None => {
                    // Relative: keep leading `..`.
                    out.push(component.as_os_str());
                }
                Some(Component::CurDir) => {
                    out.push(component.as_os_str());
                }
            },
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                out.push(component.as_os_str());
            }
        }
    }
    out
}

/// Prepare `SQLite` database path by ensuring parent directories exist.
///
/// This function handles `SQLite` DSN path preparation:
/// - For file-based databases, ensures parent directories exist if `create_dirs` is true
/// - For memory databases, returns the DSN unchanged
/// - Returns the original DSN if no path manipulation is needed
///
/// # Arguments
/// * `dsn` - The `SQLite` DSN (e.g., "`sqlite:///path/to/db.sqlite`" or "`sqlite::memory:`")
/// * `create_dirs` - Whether to create parent directories for file-based databases
///
/// # Returns
/// * `Ok(String)` - The prepared DSN (may be unchanged)
/// * `Err(io::Error)` - If directory creation fails
pub fn prepare_sqlite_path(dsn: &str, create_dirs: bool) -> io::Result<String> {
    if is_memory_dsn(dsn) {
        return Ok(dsn.to_owned());
    }

    if !create_dirs {
        return Ok(dsn.to_owned());
    }

    if let Some(path) = extract_file_path_from_dsn(dsn)
        && let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    Ok(dsn.to_owned())
}

/// Extract the file path from a `SQLite` DSN.
///
/// Handles:
/// - `sqlite:///absolute/path/to/db.sqlite`
/// - `sqlite://./relative/path/to/db.sqlite`
/// - `sqlite:relative/path/to/db.sqlite`
/// - `sqlite:./relative.db`
/// - `sqlite:///C:/data/app.db` / `sqlite:C:/data/app.db` (Windows)
/// - Plain file paths (fallback)
///
/// Returns `None` for memory DSNs.
#[must_use]
pub(crate) fn extract_file_path_from_dsn(dsn: &str) -> Option<PathBuf> {
    if is_memory_dsn(dsn) {
        return None;
    }

    if let Some(path_part) = dsn.strip_prefix("sqlite:") {
        let path_part = strip_query(path_part);

        // `sqlite::memory:` already excluded by is_memory_dsn.
        if path_part == ":memory:" || path_part.is_empty() {
            return None;
        }

        // Absolute URL form: sqlite:///path or sqlite:///C:/path
        if let Some(rest) = path_part.strip_prefix("///") {
            return Some(normalize_url_absolute_path(rest));
        }

        // Authority-ish relative: sqlite://./path → ./path
        if let Some(rest) = path_part.strip_prefix("//./") {
            return Some(PathBuf::from(format!("./{rest}")));
        }

        // sqlite://host/path — uncommon for SQLite files; treat path after host.
        if let Some(rest) = path_part.strip_prefix("//") {
            if let Some((_host, path)) = rest.split_once('/') {
                let path = if path.is_empty() {
                    return None;
                } else {
                    format!("/{path}")
                };
                return Some(normalize_url_absolute_path(path.trim_start_matches('/')));
            }
            return None;
        }

        // sqlite:relative or sqlite:./relative or sqlite:C:/windows
        return Some(PathBuf::from(path_part));
    }

    // Fallback: treat as plain file path (strip query if present).
    let path = strip_query(dsn);
    if path.is_empty() {
        return None;
    }
    Some(PathBuf::from(path))
}

fn strip_query(s: &str) -> &str {
    s.split_once('?').map_or(s, |(path, _)| path)
}

/// Convert a URL path body into a filesystem [`PathBuf`].
///
/// `rest` is the portion after `sqlite:///` (no leading scheme). Unix absolute paths keep a
/// leading `/`. Windows drive paths (`C:/...`) must **not** keep a leading slash.
fn normalize_url_absolute_path(rest: &str) -> PathBuf {
    #[cfg(windows)]
    {
        // "/C:/data" or "C:/data" after strip of ///
        let bytes = rest.as_bytes();
        if rest.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            return PathBuf::from(rest);
        }
        if rest.len() >= 3
            && rest.starts_with('/')
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b':'
        {
            return PathBuf::from(&rest[1..]);
        }
        // Unix-style absolute on Windows (e.g. WSL-ish): keep as given with leading slash.
        if rest.starts_with('/') {
            return PathBuf::from(rest);
        }
        PathBuf::from(format!("/{rest}"))
    }
    #[cfg(not(windows))]
    {
        if rest.starts_with('/') {
            PathBuf::from(rest)
        } else {
            PathBuf::from(format!("/{rest}"))
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn test_extract_file_path_from_dsn() {
        assert_eq!(
            extract_file_path_from_dsn("sqlite:///absolute/path/to/db.sqlite"),
            Some(PathBuf::from("/absolute/path/to/db.sqlite"))
        );

        assert_eq!(
            extract_file_path_from_dsn("sqlite://./relative/path/to/db.sqlite"),
            Some(PathBuf::from("./relative/path/to/db.sqlite"))
        );

        assert_eq!(
            extract_file_path_from_dsn("sqlite:test.db"),
            Some(PathBuf::from("test.db"))
        );

        assert_eq!(
            extract_file_path_from_dsn("sqlite:./test.db"),
            Some(PathBuf::from("./test.db"))
        );

        assert_eq!(
            extract_file_path_from_dsn("sqlite:///path/to/db.sqlite?wal=true"),
            Some(PathBuf::from("/path/to/db.sqlite"))
        );

        assert_eq!(extract_file_path_from_dsn("sqlite::memory:"), None);
        assert_eq!(extract_file_path_from_dsn("sqlite://memory:"), None);
        assert_eq!(
            extract_file_path_from_dsn("sqlite:///test.db?mode=memory"),
            None
        );

        assert_eq!(extract_file_path_from_dsn(""), None);

        assert_eq!(
            extract_file_path_from_dsn("/plain/file/path.db"),
            Some(PathBuf::from("/plain/file/path.db"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn absolute_unix_path_stays_absolute() {
        let path = extract_file_path_from_dsn("sqlite:///absolute/path/app.db").unwrap();
        assert!(path.is_absolute(), "{path:?}");
        assert_eq!(path, PathBuf::from("/absolute/path/app.db"));
    }

    #[cfg(windows)]
    #[test]
    fn url_absolute_path_keeps_root_component() {
        use std::path::Component;
        let path = extract_file_path_from_dsn("sqlite:///absolute/path/app.db").unwrap();
        assert!(
            matches!(
                path.components().next(),
                Some(Component::RootDir | Component::Prefix(_))
            ),
            "expected rooted path, got {path:?}"
        );
    }

    #[test]
    fn leading_dotdot_preserved_lexically() {
        let normalized = normalize_path_lexically(Path::new("../../data/app.db"));
        assert_eq!(normalized, PathBuf::from("../../data/app.db"));
    }

    #[test]
    fn lexical_collapse_normal_parent() {
        let normalized = normalize_path_lexically(Path::new("./data/../data/app.db"));
        assert_eq!(normalized, PathBuf::from("data/app.db"));
    }

    #[test]
    fn lexical_does_not_escape_unix_root() {
        let normalized = normalize_path_lexically(Path::new("/../etc/passwd"));
        assert_eq!(normalized, PathBuf::from("/etc/passwd"));
    }

    #[test]
    fn relative_dot_paths_share_scope_identity() {
        let a = sqlite_scope_identity("sqlite:./test.db");
        let b = sqlite_scope_identity("sqlite:test.db");
        assert_eq!(a, b);
    }

    #[test]
    fn relative_dotdot_paths_share_scope_identity() {
        let a = sqlite_scope_identity("sqlite:./data/../data/app.db");
        let b = sqlite_scope_identity("sqlite:data/app.db");
        assert_eq!(a, b);
    }

    #[cfg(unix)]
    #[test]
    fn scope_identity_stable_when_parent_is_symlink_before_file_exists() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().unwrap();
        let real_data = temp_dir.path().join("mnt").join("storage").join("data");
        std::fs::create_dir_all(&real_data).unwrap();

        let link = temp_dir.path().join("data");
        symlink(&real_data, &link).unwrap();

        let db_path = link.join("app.db");
        let dsn = format!("sqlite:{}", db_path.display());

        let before = sqlite_scope_identity(&dsn);
        assert!(
            before.contains("mnt") || before.contains("storage"),
            "expected symlink-resolved identity before create, got {before}"
        );

        std::fs::File::create(&db_path).unwrap();
        let after = sqlite_scope_identity(&dsn);
        assert_eq!(before, after);
    }

    #[test]
    fn memory_dsns_are_memory_locations() {
        for dsn in [
            "sqlite::memory:",
            "sqlite://memory:",
            "sqlite:///file.db?mode=memory",
        ] {
            assert!(
                matches!(
                    sqlite_database_location(dsn),
                    Some(SqliteDatabaseLocation::Memory { .. })
                ),
                "{dsn}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_paths_keep_prefix() {
        let from_url = extract_file_path_from_dsn("sqlite:///C:/data/app.db").unwrap();
        assert_eq!(from_url, PathBuf::from("C:/data/app.db"));

        let from_bare = extract_file_path_from_dsn("sqlite:C:/data/app.db").unwrap();
        assert_eq!(from_bare, PathBuf::from("C:/data/app.db"));
    }

    #[test]
    fn test_prepare_sqlite_path_memory() {
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
        let dsn = "sqlite:///some/path/db.sqlite";
        assert_eq!(prepare_sqlite_path(dsn, false).unwrap(), dsn);
    }

    #[test]
    fn test_prepare_sqlite_path_create_dirs() {
        let temp_dir = TempDir::new().unwrap();
        let test_path = temp_dir.path().join("nested").join("db.sqlite");

        let path_str = test_path.to_string_lossy().replace('\\', "/");
        let dsn = if path_str.starts_with('/') {
            format!("sqlite://{path_str}")
        } else {
            // Windows drive path: sqlite:///C:/...
            format!("sqlite:///{path_str}")
        };

        let result = prepare_sqlite_path(&dsn, true);
        assert!(result.is_ok(), "Failed to prepare path: {:?}", result.err());
        assert_eq!(result.unwrap(), dsn);

        let parent = test_path.parent().unwrap();
        assert!(parent.exists(), "Parent directory should exist");
    }
}
