//! Gate 2 down-scope: intersect the adapter's registry allowlist with the cap
//! token's scopes (a cap `*` grants the full allowlist), then optionally narrow
//! to a caller-requested subset. The wildcard `*` is never emitted.

use std::collections::BTreeSet;

use toolkit_macros::domain_model;

/// Errors raised while down-scoping for an OBO re-mint.
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum DownscopeError {
    /// The adapter allowlist and the cap scopes share nothing — the adapter is
    /// not permitted to act on behalf of this cap.
    #[error("adapter not permitted for OBO callbacks (empty intersection)")]
    EmptyIntersection,
    /// The caller requested scopes that are not a subset of what was granted.
    #[error("requested scopes exceed granted")]
    NotSubset,
}

/// Computes the granted scope set for an OBO token.
///
/// `granted = (cap scopes contain "*" ? allowlist : allowlist ∩ cap_scopes)`,
/// sorted and deduped. If `requested` is supplied it must be a subset of
/// `granted`, and the result is narrowed to it. `"*"` is stripped from the
/// allowlist and is never present in the output.
///
/// # Errors
/// - [`DownscopeError::EmptyIntersection`] if `granted` is empty.
/// - [`DownscopeError::NotSubset`] if `requested` is not a subset of `granted`.
pub fn downscope(
    allowlist: &[String],
    cap_scopes: &str,
    requested: Option<&[String]>,
) -> Result<Vec<String>, DownscopeError> {
    let allow: BTreeSet<&str> = allowlist
        .iter()
        .map(String::as_str)
        .filter(|s| *s != "*")
        .collect();
    let cap: BTreeSet<&str> = cap_scopes.split_whitespace().collect();

    let mut granted: Vec<String> = if cap.contains("*") {
        allow.iter().map(|s| (*s).to_owned()).collect()
    } else {
        allow.intersection(&cap).map(|s| (*s).to_owned()).collect()
    };
    granted.sort();
    granted.dedup();
    if granted.is_empty() {
        return Err(DownscopeError::EmptyIntersection);
    }

    if let Some(req) = requested {
        let g: BTreeSet<&str> = granted.iter().map(String::as_str).collect();
        if !req.iter().all(|r| g.contains(r.as_str())) {
            return Err(DownscopeError::NotSubset);
        }
        granted = req.to_vec();
        granted.sort();
        granted.dedup();
        if granted.is_empty() {
            return Err(DownscopeError::EmptyIntersection);
        }
    }

    debug_assert!(
        !granted.iter().any(|s| s == "*"),
        "down-scoped grant must never contain the wildcard"
    );
    Ok(granted)
}

#[cfg(test)]
#[path = "downscope_tests.rs"]
mod tests;
