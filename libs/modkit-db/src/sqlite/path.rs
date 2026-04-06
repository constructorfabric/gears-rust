//! `SQLite` path preparation utilities.

use std::io;

/// Prepare `SQLite` database path by ensuring parent directories exist.
///
/// This function handles `SQLite` DSN path preparation:
/// - For file-based databases, ensures parent directories exist if `create_dirs` is true
/// - For memory databases, returns the DSN unchanged
/// - Returns the original DSN if no path manipulation is needed
///
/// # Arguments
/// * `dsn` - The `SQLite` DSN (e.g., "<sqlite:///path/to/db.sqlite>" or "`sqlite::memory`:")
/// * `create_dirs` - Whether to create parent directories for file-based databases
///
/// # Returns
/// * `Ok(String)` - The prepared DSN (may be unchanged)
/// * `Err(io::Error)` - If directory creation fails
pub fn prepare_sqlite_path(dsn: &str, create_dirs: bool) -> io::Result<String> {
    // Handle memory databases - no path preparation needed
    if dsn == "sqlite::memory:" || dsn == "sqlite://memory:" {
        return Ok(dsn.to_owned());
    }

    // Check for mode=memory in query parameters
    if let Ok(url) = url::Url::parse(dsn) {
        for (key, value) in url.query_pairs() {
            if key.to_lowercase() == "mode" && value.to_lowercase() == "memory" {
                return Ok(dsn.to_owned());
            }
        }
    }

    // Only create directories if requested
    if !create_dirs {
        return Ok(dsn.to_owned());
    }

    // Extract file path from DSN for directory creation
    let file_path = extract_file_path_from_dsn(dsn);

    if let Some(path) = file_path
        && let Some(parent) = path.parent()
    {
        std::fs::create_dir_all(parent)?;
    }

    Ok(dsn.to_owned())
}

/// Extract the file path from a `SQLite` DSN.
///
/// Handles various `SQLite` DSN formats:
/// - `sqlite:///absolute/path/to/db.sqlite`
/// - `sqlite://./relative/path/to/db.sqlite`
/// - `sqlite:relative/path/to/db.sqlite`
/// - Plain file paths (fallback)
fn extract_file_path_from_dsn(dsn: &str) -> Option<std::path::PathBuf> {
    // Check for memory databases first
    if dsn.contains("::memory:") || dsn.contains("//memory:") || dsn.contains("mode=memory") {
        return None;
    }

    // Try to parse as URL first
    if let Ok(url) = url::Url::parse(dsn)
        && url.scheme() == "sqlite"
    {
        let path_str = url.path();

        // Handle empty path
        if path_str.is_empty() || path_str == "/" {
            return None;
        }

        // On Windows, URL paths like "/C:/path" need the leading "/" stripped
        #[cfg(windows)]
        {
            // Check if this looks like a Windows absolute path: /C:/ or /C:\
            let normalized_path = if path_str.len() > 3
                && path_str.starts_with('/')
                && path_str.chars().nth(2) == Some(':')
            {
                &path_str[1..] // Strip leading /
            } else {
                path_str
            };
            return Some(std::path::PathBuf::from(normalized_path));
        }

        #[cfg(not(windows))]
        return Some(std::path::PathBuf::from(path_str));
    }

    // Handle sqlite: prefix without proper URL format
    if let Some(path_part) = dsn.strip_prefix("sqlite:") {
        // Remove leading slashes for relative paths
        let path_part = path_part.trim_start_matches('/');

        // Remove query parameters if present
        let path_part = if let Some(pos) = path_part.find('?') {
            &path_part[..pos]
        } else {
            path_part
        };

        if !path_part.is_empty() && path_part != "memory:" {
            return Some(std::path::PathBuf::from(path_part));
        }
    }

    // Fallback: treat as plain file path
    Some(std::path::PathBuf::from(dsn))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "path_tests.rs"]
mod tests;
