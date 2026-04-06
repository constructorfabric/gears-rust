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
#[path = "home_dir_tests.rs"]
mod tests;
