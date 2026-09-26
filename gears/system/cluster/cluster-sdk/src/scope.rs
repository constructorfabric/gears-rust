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
    let charset_ok = !prefix.is_empty()
        && prefix.len() <= MAX_SCOPE_PREFIX_LEN
        && prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'/'));
    // Reject empty segments: a leading (`/a`), trailing (`a/`), or doubled
    // (`a//b`) slash would otherwise compose into keys with empty path segments.
    let segments_ok = !prefix.split('/').any(str::is_empty);
    if charset_ok && segments_ok {
        Ok(format!("{prefix}/"))
    } else {
        Err(ClusterError::InvalidName {
            name: prefix.to_owned(),
            reason: SCOPE_PREFIX_RULE,
        })
    }
}

/// The character rule a cache key must satisfy: slash-separated
/// `[a-zA-Z0-9_-]` segments, no empty segment (no leading, trailing, or
/// doubled slash), max 255 bytes. Slashes are permitted because cache keys may
/// be compound paths (e.g. `shard/42/state`), unlike
/// [`CLUSTER_NAME_RULE`](crate::profile::CLUSTER_NAME_RULE) which applies to
/// single-segment names (profiles, elections, locks).
pub const CACHE_KEY_RULE: &str = SCOPE_PREFIX_RULE;

/// Validates a consumer-supplied cache key against [`CACHE_KEY_RULE`].
///
/// # Errors
/// Returns [`ClusterError::InvalidName`] if `key` is invalid.
pub fn validate_cache_key(key: &str) -> Result<(), ClusterError> {
    let charset_ok = !key.is_empty()
        && key.len() <= MAX_SCOPE_PREFIX_LEN
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'/'));
    let segments_ok = !key.split('/').any(str::is_empty);
    if charset_ok && segments_ok {
        Ok(())
    } else {
        Err(ClusterError::InvalidName {
            name: key.to_owned(),
            reason: CACHE_KEY_RULE,
        })
    }
}

/// The sigil that opens a **reserved** keyspace: one the cluster gear keeps for
/// its own records, not part of the keyspace the cache API serves.
///
/// Deliberately outside `CACHE_KEY_RULE` and `SCOPE_PREFIX_RULE`, which is the
/// whole point: a reserved key is not merely undocumented, it is *inexpressible*
/// under the rule the cache API validates against, so no caller can name one
/// *even knowing the prefix*. Both consumer-facing surfaces close on that:
/// in-process, [`ClusterCacheV1`](crate::ClusterCacheV1) runs
/// `validate_cache_key` on every key, and refuses a reserved *prefix* with
/// [`is_reserved_key`] on `scan_prefix`/`watch_prefix`/`watch_prefix_polling`
/// (where the key rule cannot be used, because `""` is a legitimate prefix);
/// remotely, the cache RPC applies the same [`is_reserved_key`] check at its
/// boundary, because it holds the raw backend and runs no validator at all.
///
/// What this does **not** bound is the [`ClusterCacheBackend`] trait itself,
/// which validates nothing by design and is reachable in-process through
/// [`ClusterClient::cache_backend`](crate::ClusterClient::cache_backend): code
/// holding one writes whatever key it likes, reserved or not. That is not a gap
/// this sigil was meant to close. It bounds the cache *API*, whose callers may
/// be remote and merely authenticated; code already inside the process with a
/// backend handle is inside the trust boundary either way.
///
/// [`ClusterCacheBackend`]: crate::cache::ClusterCacheBackend
pub const RESERVED_KEY_SIGIL: char = '$';

/// The reserved keyspace the cache-backed default lock and leader-election
/// backends store their [`LeaseRecord`](crate::LeaseRecord)s in
/// (`cpt-cf-clst-algo-scoping-polyfill-prefix-translate`, ADR-001).
///
/// Already separator-terminated, so it is the effective prefix rather than a
/// consumer-supplied one: it never passes through `validated_prefix`, which
/// would reject it. Paired with
/// [`reserved_lease_cache`](crate::reserved_lease_cache), the only way to open a
/// view onto it.
///
/// Both defaults share one cache handle in an omit-primitive profile, so their
/// keys stay apart by their own `election/` and `lock/` prefixes *inside* this
/// space; what this prefix separates is coordination state from consumer data.
pub const RESERVED_LEASE_PREFIX: &str = "$lease/";

/// The rejection reason a request naming a reserved keyspace is refused with —
/// the `CACHE_KEY_RULE`'s counterpart for the one thing that rule cannot
/// express.
pub const RESERVED_KEY_RULE: &str = "a key opening with `$` names a cluster-internal reserved \
                                     keyspace and is not addressable through the cache API";

/// Whether `key` names anything inside a reserved keyspace.
///
/// Total by construction: it tests the [`sigil`](RESERVED_KEY_SIGIL) rather than
/// any particular reserved prefix, so a boundary check built on it covers every
/// reserved space — including ones added later — and cannot be sidestepped by a
/// longer or differently-spelled prefix.
#[must_use]
pub fn is_reserved_key(key: &str) -> bool {
    key.starts_with(RESERVED_KEY_SIGIL)
}

/// The rejection reason a lease token presented to the wrong scoped view is
/// refused with. A scoped wrapper pushes the prefix it minted a token under onto
/// [`LeaseToken::scope`](crate::lease::LeaseToken::scope) and, on
/// renew/release/resign, requires the *top* layer to equal its own prefix; a token
/// from a *different* view (a sibling scope, the full chain handed to a sub-wrapper,
/// or a raw-backend token whose scope is empty) fails this check. Reported as
/// [`ClusterError::InvalidName`](crate::error::ClusterError::InvalidName) — not
/// `LockExpired` — so the caller can tell "wrong scope" from "lease lapsed".
pub const SCOPE_MISMATCH: &str = "lease token was minted under a different scope than this view";

/// Verifies `token` was minted by the view whose effective `prefix` this is, then
/// returns a clone ready to delegate to the inner backend: the prefix re-applied to
/// [`LeaseToken::name`](crate::lease::LeaseToken::name) and this view's layer popped
/// off [`LeaseToken::scope`](crate::lease::LeaseToken::scope).
///
/// The check is **exact top-of-stack equality**, not a byte-suffix match on a
/// flattened scope string: a token minted under `.scoped("eu/team")` (scope
/// `["eu/team/"]`) is rejected by a sibling `.scoped("team")` view, where a
/// `"eu/team/".strip_suffix("team/")` on a flat string would have wrongly accepted
/// it and re-derived the phantom key `team/<name>` that reports `LockExpired` — the
/// exact silent failure [`SCOPE_MISMATCH`] exists to replace. A token from a
/// different view — or a raw-backend token whose scope stack is empty — fails with
/// [`ClusterError::InvalidName`](crate::error::ClusterError::InvalidName)/[`SCOPE_MISMATCH`]
/// rather than being re-prefixed into a phantom key.
///
/// # Errors
/// [`ClusterError::InvalidName`] carrying [`SCOPE_MISMATCH`] when `token`'s top
/// scope layer is not this view's `prefix`.
pub fn reapply_for_token(
    prefix: &str,
    token: &crate::lease::LeaseToken,
) -> Result<crate::lease::LeaseToken, ClusterError> {
    if token.scope.last().map(String::as_str) != Some(prefix) {
        return Err(ClusterError::InvalidName {
            name: token.name.clone(),
            reason: SCOPE_MISMATCH,
        });
    }
    let mut scoped = token.clone();
    scoped.name = apply(prefix, &token.name);
    scoped.scope.pop();
    Ok(scoped)
}

/// Prepends the effective `prefix` to a coordination `name` for the write path.
pub fn apply(prefix: &str, name: &str) -> String {
    format!("{prefix}{name}")
}

/// Strips the effective `prefix` from a backend-returned `key` for the read path.
/// A key that does not carry the prefix (a backend that returns something
/// unexpected) is passed through unchanged rather than corrupted.
pub fn strip<'a>(prefix: &str, key: &'a str) -> &'a str {
    key.strip_prefix(prefix).unwrap_or(key)
}

/// Strips the effective `prefix` from the `name` carried by a name-bearing
/// error, so the scoped path returns the *bare* consumer name on the error path
/// exactly as it does on the success path.
///
/// Every variant that carries a coordination `name` is rewritten:
/// [`LockContended`](crate::error::ClusterError::LockContended),
/// [`LockTimeout`](crate::error::ClusterError::LockTimeout),
/// [`LockExpired`](crate::error::ClusterError::LockExpired), and
/// [`InvalidName`](crate::error::ClusterError::InvalidName) (which an inner backend
/// can raise for a scoped key that only becomes invalid *after* the prefix is
/// applied — e.g. one pushed past the length limit); every other variant is passed
/// through untouched. The rewrite is lenient — it reuses [`strip`], so a name that
/// does not carry the prefix is left as-is rather than corrupted — which means
/// chaining wrappers each peel their own layer and a foreign error is never mangled.
///
/// Note this does *not* touch the wrapper's own [`SCOPE_MISMATCH`] rejection: that
/// is minted with the *bare* `token.name` and returned by `?` before delegating, so
/// it never reaches this function.
#[must_use]
pub fn strip_error_name(prefix: &str, error: ClusterError) -> ClusterError {
    match error {
        ClusterError::LockContended { name } => ClusterError::LockContended {
            name: strip(prefix, &name).to_owned(),
        },
        ClusterError::LockTimeout { name, waited } => ClusterError::LockTimeout {
            name: strip(prefix, &name).to_owned(),
            waited,
        },
        ClusterError::LockExpired { name } => ClusterError::LockExpired {
            name: strip(prefix, &name).to_owned(),
        },
        ClusterError::InvalidName { name, reason } => ClusterError::InvalidName {
            name: strip(prefix, &name).to_owned(),
            reason,
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RESERVED_KEY_SIGIL, RESERVED_LEASE_PREFIX, SCOPE_PREFIX_RULE, apply, is_reserved_key,
        strip, strip_error_name, validate_cache_key, validated_prefix,
    };
    use crate::error::ClusterError;

    #[test]
    fn strip_error_name_peels_the_prefix_from_every_name_bearing_variant() {
        let p = "event-broker/";
        // All four name-bearing variants return the bare consumer name.
        assert!(matches!(
            strip_error_name(p, ClusterError::LockContended { name: "event-broker/ledger".into() }),
            ClusterError::LockContended { name } if name == "ledger"
        ));
        assert!(matches!(
            strip_error_name(p, ClusterError::LockExpired { name: "event-broker/ledger".into() }),
            ClusterError::LockExpired { name } if name == "ledger"
        ));
        assert!(matches!(
            strip_error_name(
                p,
                ClusterError::LockTimeout {
                    name: "event-broker/ledger".into(),
                    waited: std::time::Duration::from_secs(1),
                },
            ),
            ClusterError::LockTimeout { name, .. } if name == "ledger"
        ));
        // InvalidName carries a name too — an inner backend can raise it for a
        // scoped key that only becomes invalid after the prefix is applied.
        assert!(matches!(
            strip_error_name(
                p,
                ClusterError::InvalidName { name: "event-broker/ledger".into(), reason: "rule" },
            ),
            ClusterError::InvalidName { name, .. } if name == "ledger"
        ));
        // A variant that carries no name is passed through untouched.
        assert!(matches!(
            strip_error_name(p, ClusterError::Shutdown),
            ClusterError::Shutdown
        ));
        // Lenient: a name without the prefix is left as-is, not corrupted.
        assert!(matches!(
            strip_error_name(p, ClusterError::LockContended { name: "other/key".into() }),
            ClusterError::LockContended { name } if name == "other/key"
        ));
    }

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

    /// The load-bearing property of the reserved keyspace, asserted rather than
    /// assumed: a consumer cannot *name* a key inside it. The two public
    /// validators are the only gates a consumer-supplied name passes, so if both
    /// refuse the prefix — and the sigil on its own — the separation holds by
    /// construction and does not depend on any boundary check remembering to run.
    #[test]
    fn the_reserved_prefix_is_inexpressible_as_a_consumer_key_or_scope() {
        assert!(
            RESERVED_LEASE_PREFIX.starts_with(RESERVED_KEY_SIGIL),
            "the reserved prefix must carry the sigil the boundary check tests"
        );
        assert!(
            RESERVED_LEASE_PREFIX.ends_with('/'),
            "the reserved prefix is the *effective* prefix, so it is already separator-terminated"
        );
        for spelling in [RESERVED_LEASE_PREFIX, "$lease/lock/ledger", "$lease", "$"] {
            assert!(
                matches!(
                    validate_cache_key(spelling),
                    Err(ClusterError::InvalidName { .. })
                ),
                "`{spelling}` must not be a legal cache key"
            );
            assert!(
                matches!(
                    validated_prefix(spelling),
                    Err(ClusterError::InvalidName { .. })
                ),
                "`{spelling}` must not be a legal consumer scope prefix"
            );
        }
    }

    /// The boundary check tests the sigil, not one prefix — so a reserved space
    /// added later is covered by the same line, and no alternative spelling of an
    /// existing one slips past. Its complement matters just as much: a key that
    /// merely *mentions* the sigil later on is ordinary consumer data (and is not
    /// a legal key anyway), so the check must not over-reach into the public
    /// keyspace.
    #[test]
    fn only_a_leading_sigil_marks_a_key_reserved() {
        assert!(is_reserved_key(RESERVED_LEASE_PREFIX));
        assert!(is_reserved_key("$lease/lock/ledger"));
        assert!(is_reserved_key("$something-else/k"));
        assert!(!is_reserved_key("lease/lock/ledger"));
        assert!(!is_reserved_key("ledger"));
        assert!(!is_reserved_key("ledger$"));
        assert!(!is_reserved_key(""));
    }

    #[test]
    fn strip_passes_through_an_unprefixed_key() {
        assert_eq!(strip("event-broker/", "other/key"), "other/key");
    }
}
