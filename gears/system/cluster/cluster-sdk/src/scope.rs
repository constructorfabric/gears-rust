// Created: 2026-06-10 by Constructor Tech
// @cpt-dod:cpt-cf-clst-dod-scoping-polyfill-wrappers:p1
//! Shared prefix translation for the per-primitive scoping wrappers (DESIGN §3.8).
//!
//! Scoping is a stateless name translation: a validated `prefix` is prepended to
//! a coordination name on the write path and stripped on the read path. The four
//! `Scoped*Backend` wrappers (one per primitive) reuse the helpers here so the
//! compose/validate/prepend/strip rules live in exactly one place. Scoping
//! composes by stacking wrappers — each layer prepends and strips its own single
//! prefix, so `scoped("a").scoped("b")` makes the innermost backend observe
//! `"a/b/<name>"` (`cpt-cf-clst-algo-scoping-polyfill-prefix-translate`).

use crate::error::ClusterError;

/// The character rule a scope prefix must satisfy (DESIGN §3.8): between 1 and
/// [`MAX_SCOPE_PREFIX_LEN`] ASCII alphanumerics, `_`, `-`, or `/`, with no empty
/// `/`-delimited segment (so no leading, trailing, or doubled slash). Unlike
/// [`CLUSTER_NAME_RULE`](crate::profile::CLUSTER_NAME_RULE) (profile names), `/`
/// is permitted here because it is the scope separator and a consumer may pass a
/// multi-segment prefix in one call.
pub const SCOPE_PREFIX_RULE: &str = "[a-zA-Z0-9_-]+(/[a-zA-Z0-9_-]+)* (max 255 chars)";

/// The maximum length (in bytes) of a scope prefix as supplied by the consumer
/// (before the trailing separator is appended). Capped so a pathological prefix
/// cannot produce an unbounded backend key; part of the frozen contract so a
/// later tightening is not a breaking change.
pub const MAX_SCOPE_PREFIX_LEN: usize = 255;

/// Validates `prefix` against [`SCOPE_PREFIX_RULE`] and returns the effective
/// prefix to prepend — `prefix` with a trailing `/` separator.
///
/// # Errors
/// Returns [`ClusterError::InvalidName`] if `prefix` is empty, longer than
/// [`MAX_SCOPE_PREFIX_LEN`], contains a character outside the rule, or has an
/// empty `/`-delimited segment (a leading, trailing, or doubled slash) — so an
/// invalid or accident-prone scope is rejected at construction rather than
/// silently producing keys like `/a/` or `a//b/`.
pub fn validated_prefix(prefix: &str) -> Result<String, ClusterError> {
    // @cpt-begin:cpt-cf-clst-flow-scoping-polyfill-scoped-names:p1:inst-sn-validate
    let charset_ok = !prefix.is_empty()
        && prefix.len() <= MAX_SCOPE_PREFIX_LEN
        && prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'/'));
    // Reject empty segments: a leading (`/a`), trailing (`a/`), or doubled
    // (`a//b`) slash would otherwise compose into keys with empty path segments.
    let segments_ok = !prefix.split('/').any(str::is_empty);
    if charset_ok && segments_ok {
        // @cpt-end:cpt-cf-clst-flow-scoping-polyfill-scoped-names:p1:inst-sn-validate
        // @cpt-begin:cpt-cf-clst-algo-scoping-polyfill-prefix-translate:p1:inst-pt-compose
        Ok(format!("{prefix}/"))
        // @cpt-end:cpt-cf-clst-algo-scoping-polyfill-prefix-translate:p1:inst-pt-compose
    } else {
        // @cpt-begin:cpt-cf-clst-flow-scoping-polyfill-scoped-names:p1:inst-sn-reject
        Err(ClusterError::InvalidName {
            name: prefix.to_owned(),
            reason: SCOPE_PREFIX_RULE,
        })
        // @cpt-end:cpt-cf-clst-flow-scoping-polyfill-scoped-names:p1:inst-sn-reject
    }
}

/// Prepends the effective `prefix` to a coordination `name` for the write path.
pub fn apply(prefix: &str, name: &str) -> String {
    // @cpt-begin:cpt-cf-clst-algo-scoping-polyfill-prefix-translate:p1:inst-pt-write
    // @cpt-begin:cpt-cf-clst-flow-scoping-polyfill-scoped-names:p1:inst-sn-prepend
    format!("{prefix}{name}")
    // @cpt-end:cpt-cf-clst-flow-scoping-polyfill-scoped-names:p1:inst-sn-prepend
    // @cpt-end:cpt-cf-clst-algo-scoping-polyfill-prefix-translate:p1:inst-pt-write
}

/// Strips the effective `prefix` from a backend-returned `key` for the read path.
/// A key that does not carry the prefix (a backend that returns something
/// unexpected) is passed through unchanged rather than corrupted.
pub fn strip<'a>(prefix: &str, key: &'a str) -> &'a str {
    // @cpt-begin:cpt-cf-clst-algo-scoping-polyfill-prefix-translate:p1:inst-pt-read
    // @cpt-begin:cpt-cf-clst-flow-scoping-polyfill-scoped-names:p1:inst-sn-strip
    key.strip_prefix(prefix).unwrap_or(key)
    // @cpt-end:cpt-cf-clst-flow-scoping-polyfill-scoped-names:p1:inst-sn-strip
    // @cpt-end:cpt-cf-clst-algo-scoping-polyfill-prefix-translate:p1:inst-pt-read
}

#[cfg(test)]
mod tests {
    use super::{SCOPE_PREFIX_RULE, apply, strip, validated_prefix};
    use crate::error::ClusterError;

    #[test]
    fn valid_prefix_gains_a_trailing_separator() {
        assert_eq!(
            validated_prefix("event-broker").expect("valid"),
            "event-broker/"
        );
        // A multi-segment prefix is permitted (the `/` separator is in the rule).
        assert_eq!(validated_prefix("a/b").expect("valid"), "a/b/");
    }

    #[test]
    fn invalid_prefix_is_rejected_with_invalid_name() {
        assert!(matches!(
            validated_prefix(""),
            Err(ClusterError::InvalidName { reason, .. }) if reason == SCOPE_PREFIX_RULE
        ));
        assert!(matches!(
            validated_prefix("has space"),
            Err(ClusterError::InvalidName { .. })
        ));
        // A `.` is outside the rule.
        assert!(matches!(
            validated_prefix("has.dot"),
            Err(ClusterError::InvalidName { .. })
        ));
    }

    #[test]
    fn empty_segments_are_rejected() {
        // Leading, trailing, and doubled slashes all produce an empty segment.
        for bad in ["/a", "a/", "a//b", "/", "a/b/"] {
            assert!(
                matches!(validated_prefix(bad), Err(ClusterError::InvalidName { .. })),
                "`{bad}` must be rejected for an empty path segment"
            );
        }
    }

    #[test]
    fn prefix_length_is_capped() {
        use super::MAX_SCOPE_PREFIX_LEN;
        let at_cap = "a".repeat(MAX_SCOPE_PREFIX_LEN);
        assert!(
            validated_prefix(&at_cap).is_ok(),
            "a prefix at the cap is valid"
        );
        let over_cap = "a".repeat(MAX_SCOPE_PREFIX_LEN + 1);
        assert!(
            matches!(
                validated_prefix(&over_cap),
                Err(ClusterError::InvalidName { .. })
            ),
            "a prefix past the cap is rejected"
        );
    }

    #[test]
    fn apply_then_strip_round_trips() {
        let prefix = "event-broker/";
        let scoped = apply(prefix, "shard-assignments");
        assert_eq!(scoped, "event-broker/shard-assignments");
        assert_eq!(strip(prefix, &scoped), "shard-assignments");
    }

    #[test]
    fn strip_passes_through_an_unprefixed_key() {
        assert_eq!(strip("event-broker/", "other/key"), "other/key");
    }
}
