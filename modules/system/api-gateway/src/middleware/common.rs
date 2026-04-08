use axum::extract::Request;

pub fn resolve_path(req: &Request, matched_path: &str) -> String {
    req.extensions()
        .get::<axum::extract::NestedPath>()
        .and_then(|np| strip_path_prefix(matched_path, np.as_str()))
        .unwrap_or_else(|| matched_path.to_owned())
}

/// Strip `prefix` from `path` only at a segment boundary.
///
/// Returns `None` when the prefix doesn't match.  When it does match the
/// result always starts with `/` (or is `/` when the path equals the prefix).
fn strip_path_prefix(path: &str, prefix: &str) -> Option<String> {
    let rest = path.strip_prefix(prefix)?;
    if rest.is_empty() {
        // path == prefix exactly  →  root
        Some("/".to_owned())
    } else if rest.starts_with('/') {
        // clean segment boundary  →  keep the slash
        Some(rest.to_owned())
    } else {
        // partial segment overlap (e.g. prefix="/cf", path="/cfish")  →  no match
        None
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
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
}
