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
