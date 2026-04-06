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
#[path = "common_tests.rs"]
mod common_tests;
