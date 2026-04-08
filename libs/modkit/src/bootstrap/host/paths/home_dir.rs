use std::{
    env,
    path::{Path, PathBuf},
};

/// Errors for resolving the home directory
#[derive(Debug, thiserror::Error)]
pub enum HomeDirError {
    #[error("HOME environment variable is not set")]
    HomeMissing,
    #[error("failed to get executable path: {0}")]
    ExecutablePathError(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[must_use]
pub fn default_home_dir() -> PathBuf {
    env::home_dir()
        .or_else(|| env::current_dir().ok())
        .unwrap_or_else(env::temp_dir)
}

/// Expand `~` prefix to user home directory.
///
/// Returns the path unchanged if no tilde prefix is present.
/// On Windows, uses `USERPROFILE` or `HOME` environment variable.
/// On Unix, uses `HOME` environment variable.
///
/// # Errors
/// Returns `HomeDirError::HomeMissing` if the home directory cannot be determined.
pub fn expand_tilde(raw: &str) -> Result<PathBuf, HomeDirError> {
    #[cfg(target_os = "windows")]
    {
        if raw.starts_with('~') {
            let user_home = env::home_dir()
                .ok_or_else(|| env::var("HOME"))
                .map_err(|_| HomeDirError::HomeMissing)?;
            if raw == "~" {
                Ok(user_home)
            } else if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
                Ok(Path::new(&user_home).join(rest))
            } else {
                // Patterns like "~username" are not supported; treat as user home + rest
                let rest = raw.trim_start_matches('~');
                let rest = rest.trim_start_matches(['/', '\\']);
                Ok(Path::new(&user_home).join(rest))
            }
        } else {
            Ok(PathBuf::from(raw))
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Some(stripped) = raw.strip_prefix("~/") {
            let home = env::home_dir().ok_or(HomeDirError::HomeMissing)?;
            Ok(Path::new(&home).join(stripped))
        } else if raw == "~" {
            let home = env::home_dir().ok_or(HomeDirError::HomeMissing)?;
            Ok(home)
        } else {
            Ok(PathBuf::from(raw))
        }
    }
}

/// Normalize a path.
///
/// Rules:
/// - `~` prefix: expand to user home directory
/// - Absolute path: use as-is
/// - Other: prepend CWD
///
/// # Errors
/// Returns `HomeDirError` if path normalization fails.
pub fn normalize_path(raw: &str) -> Result<PathBuf, HomeDirError> {
    // First, expand tilde if present
    let expanded = expand_tilde(raw)?;

    // If already absolute, return as-is
    if expanded.is_absolute() {
        return Ok(expanded);
    }

    std::path::absolute(raw).map_err(|err| {
        HomeDirError::ExecutablePathError(format!("path '{raw}' is invalid due to: {err}"))
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // -------------------------
    // expand_tilde tests
    // -------------------------
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn expand_tilde_with_path() {
        let tmp = tempdir().unwrap();
        let tmp_path = tmp.path().to_str().unwrap();

        temp_env::with_var("HOME", Some(tmp_path), || {
            let result = super::expand_tilde("~/bin/app").unwrap();
            assert!(result.is_absolute());
            assert!(result.ends_with("bin/app"));
        });
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn expand_tilde_only() {
        let tmp = tempdir().unwrap();
        let tmp_path = tmp.path().to_str().unwrap();

        temp_env::with_var("HOME", Some(tmp_path), || {
            let result = expand_tilde("~").unwrap();
            assert_eq!(result, tmp.path());
        });
    }

    #[test]
    fn expand_tilde_no_tilde() {
        let result = expand_tilde("/usr/bin/app").unwrap();
        assert_eq!(result, PathBuf::from("/usr/bin/app"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_expand_tilde_with_path() {
        let tmp = tempdir().unwrap();
        let tmp_path = tmp.path().to_str().unwrap();

        temp_env::with_var("USERPROFILE", Some(tmp_path), || {
            let result = expand_tilde("~/bin/app").unwrap();
            assert!(result.is_absolute());
            assert!(result.ends_with("bin\\app") || result.ends_with("bin/app"));
        });
    }

    // -------------------------
    // normalize_executable_path tests
    // -------------------------
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn normalize_absolute_path() {
        let result = normalize_path("/usr/bin/myapp").unwrap();
        assert_eq!(result, PathBuf::from("/usr/bin/myapp"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_normalize_absolute_path() {
        let result = normalize_path("C:\\bin\\myapp.exe").unwrap();
        assert_eq!(result, PathBuf::from("C:\\bin\\myapp.exe"));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn normalize_tilde_path() {
        let tmp = tempdir().unwrap();
        let tmp_path = tmp.path().to_str().unwrap();

        temp_env::with_var("HOME", Some(tmp_path), || {
            let result = normalize_path("~/bin/myapp").unwrap();
            assert!(result.is_absolute());
            assert!(result.starts_with(tmp_path));
            assert!(result.ends_with("bin/myapp"));
        });
    }

    #[test]
    fn normalize_filename_only() {
        let result = normalize_path("myapp.exe").unwrap();
        let cwd = env::current_dir().unwrap();
        assert_eq!(result, cwd.join("myapp.exe"));
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn normalize_relative_path_resolves_to_absolute() {
        let err = normalize_path("./bin/myapp").unwrap();
        assert!(err.is_absolute());
        assert!(err.ends_with("bin/myapp"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_normalize_relative_path_resolves_to_absolute() {
        let err = normalize_path(".\\bin\\myapp").unwrap();
        assert!(err.is_absolute());
        assert!(err.ends_with("bin\\myapp"));
    }
}
