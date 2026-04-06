use super::*;

#[test]
fn exact_match_returns_root() {
    assert_eq!(strip_path_prefix("/cf", "/cf"), Some("/".to_owned()));
}

#[test]
fn segment_boundary_strips_correctly() {
    assert_eq!(
        strip_path_prefix("/cf/users", "/cf"),
        Some("/users".to_owned())
    );
}

#[test]
fn partial_segment_overlap_rejected() {
    assert_eq!(strip_path_prefix("/cfish", "/cf"), None);
}

#[test]
fn no_prefix_match_returns_none() {
    assert_eq!(strip_path_prefix("/other/path", "/cf"), None);
}

#[test]
fn nested_prefix_strips_correctly() {
    assert_eq!(
        strip_path_prefix("/api/v1/users", "/api/v1"),
        Some("/users".to_owned())
    );
}

#[test]
fn path_with_params_strips_correctly() {
    assert_eq!(
        strip_path_prefix("/cf/users/{id}", "/cf"),
        Some("/users/{id}".to_owned())
    );
}

#[test]
fn empty_prefix_returns_full_path() {
    assert_eq!(strip_path_prefix("/users", ""), Some("/users".to_owned()));
}
