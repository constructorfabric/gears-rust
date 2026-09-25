//! The wire form of a discovery page's position (D12).
//!
//! `toolkit-odata`'s [`CursorV1`] rather than a bespoke token, because the property
//! the page contract needs is already there: the encoding is versioned base64url and
//! decoding **refuses** an unknown version instead of reading it as a position this
//! build understands. A cursor that outlives a protocol change is then a `400`
//! rather than a silently wrong page.
//!
//! # Why this lives in the transport layer
//!
//! The domain's position is a stored `gts_id` — [`DiscoveryQuery::after`]. The
//! base64url envelope is how one page hands that to the next *over HTTP*, so it is
//! encoding, not policy: `discover` never sees a token, and the bound below is a
//! contract rule a gRPC adapter would restate in its own encoding rather than
//! inherit.
//!
//! # What the cursor binds
//!
//! The query it was issued for: the pattern, `depth`, `kind`, `lifecycle_status`
//! and the canonical [`FieldSelection`]. Replaying a position under any of them
//! changed is refused rather than spliced; an absent `$select` or
//! `lifecycle_status` and its explicit default share one binding.
//!
//! [`DiscoveryQuery::after`]: crate::domain::registry_service::DiscoveryQuery::after

use std::num::NonZeroU8;

use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::pagination::short_filter_hash;
use toolkit_odata::{CursorV1, ODataOrderBy, OrderKey, SortDir, ast, validate_cursor_against};

use super::error::{cursor_not_usable, cursor_too_long};
use crate::domain::enums::{EntityKind, LifecycleFilter};
use crate::domain::registry_service::{DiscoveryQuery, MAX_KEY_LEN, is_canonical};
use crate::domain::selection::FieldSelection;

/// The one keyset column. `gts_id` is unique and immutable, which is what makes the
/// cursor a plain keyset: a page boundary cannot drift or duplicate (§10.2).
const KEY_FIELD: &str = "gts_id";

/// The binding's name for the selection; not an entity column.
const SELECT_FIELD: &str = "$select";
const KIND_FIELD: &str = "kind";
const DEPTH_FIELD: &str = "depth";
const LIFECYCLE_FIELD: &str = "lifecycle_status";

const fn kind_name(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::TypeSchema => "type_schema",
        EntityKind::Instance => "instance",
    }
}

/// `None` for the default, which adds no term.
const fn lifecycle_name(lifecycle: LifecycleFilter) -> Option<&'static str> {
    match lifecycle {
        LifecycleFilter::Active => None,
        LifecycleFilter::Deleted => Some("deleted"),
        LifecycleFilter::All => Some("all"),
    }
}

/// Forward-only. Backward paging is not part of the discovery contract, and a
/// `"bwd"` token would describe a traversal this route does not perform.
const FORWARD: &str = "fwd";

/// The fixed page order: `gts_id` ascending.
fn page_order() -> ODataOrderBy {
    ODataOrderBy(vec![OrderKey {
        field: KEY_FIELD.to_owned(),
        dir: SortDir::Asc,
    }])
}

/// The query a page was taken under, hashed through `toolkit-odata`'s normalizer so
/// the opaque token does not spell it out. Never `None`: the selection is always
/// bound, so `validate_cursor_against` compares every pair of bindings.
fn binding_hash(binding: &Binding<'_>) -> Option<String> {
    let equals = |field: &str, value: &str| {
        ast::Expr::Compare(
            Box::new(ast::Expr::Identifier(field.to_owned())),
            ast::CompareOperator::Eq,
            Box::new(ast::Expr::Value(ast::Value::String(value.to_owned()))),
        )
    };
    let select = equals(SELECT_FIELD, &binding.selection.canonical());
    let base = match binding.pattern {
        Some(pattern) => ast::Expr::And(Box::new(equals(KEY_FIELD, pattern)), Box::new(select)),
        None => select,
    };
    // An absent or default filter adds no term; any other always changes the hash.
    // Earlier tokens resume only while their canonical `$select` is unchanged.
    let terms = [
        binding.kind.map(|kind| equals(KIND_FIELD, kind_name(kind))),
        binding
            .max_chain_depth
            .map(|depth| equals(DEPTH_FIELD, &depth.to_string())),
        lifecycle_name(binding.lifecycle).map(|name| equals(LIFECYCLE_FIELD, name)),
    ];
    let expr = terms.into_iter().flatten().fold(base, |expr, term| {
        ast::Expr::And(Box::new(expr), Box::new(term))
    });
    short_filter_hash(Some(&expr))
}

/// What a discovery cursor is bound to.
pub struct Binding<'a> {
    pub pattern: Option<&'a str>,
    pub kind: Option<EntityKind>,
    pub lifecycle: LifecycleFilter,
    pub max_chain_depth: Option<NonZeroU8>,
    pub selection: FieldSelection,
}

impl<'a> From<&'a DiscoveryQuery> for Binding<'a> {
    /// Exhaustive, so a new query filter cannot be left out of the binding.
    fn from(query: &'a DiscoveryQuery) -> Self {
        let DiscoveryQuery {
            pattern,
            after: _,
            limit: _,
            kind,
            lifecycle,
            max_chain_depth,
            selection,
        } = query;
        Self {
            pattern: pattern.as_deref(),
            kind: *kind,
            lifecycle: *lifecycle,
            max_chain_depth: *max_chain_depth,
            selection: *selection,
        }
    }
}

/// Encode the position a page stopped at, bound to the query that produced it.
///
/// # Errors
/// A canonical internal error if the token will not serialize, which is a bug here
/// rather than anything the caller did.
pub fn encode(after: &str, binding: &Binding<'_>) -> Result<String, CanonicalError> {
    CursorV1 {
        k: vec![after.to_owned()],
        o: SortDir::Asc,
        s: page_order().to_signed_tokens(),
        f: binding_hash(binding),
        d: FORWARD.to_owned(),
    }
    .encode()
    .map_err(|e| {
        tracing::error!(error = %e, "types_registry could not encode a discovery cursor");
        CanonicalError::internal("the registry could not construct a page cursor").create()
    })
}

/// Base64url JSON of one identifier; a real token cannot approach this.
const MAX_TOKEN_LEN: usize = 4096;

/// Read a token, refusing an oversized or undecodable one.
///
/// # Errors
/// A `400` naming `cursor`.
pub fn read(token: &str) -> Result<CursorV1, CanonicalError> {
    if token.len() > MAX_TOKEN_LEN {
        return Err(cursor_too_long(token.len()));
    }
    CursorV1::decode(token).map_err(|e| cursor_not_usable(&e.to_string()))
}

#[cfg(test)]
fn decode(token: &str, binding: &Binding<'_>) -> Result<String, CanonicalError> {
    resume(&read(token)?, binding)
}

/// The stored `gts_id` a [`read`] cursor resumes after.
///
/// # Errors
/// A `400` problem naming `cursor` when the token is of an unsupported shape or
/// bound to a different query than this request asks.
pub fn resume(cursor: &CursorV1, binding: &Binding<'_>) -> Result<String, CanonicalError> {
    let expected = binding_hash(binding);
    validate_cursor_against(cursor, &page_order(), expected.as_deref())
        .map_err(|e| cursor_not_usable(&e.to_string()))?;
    // `validate_cursor_against` skips the comparison when the token has no filter,
    // which only a pre-T22b cursor lacks.
    if cursor.f != expected {
        return Err(cursor_not_usable(
            "it was issued for a different pattern, depth, kind, lifecycle_status or \
             $select than this request names",
        ));
    }
    if cursor.d != FORWARD {
        return Err(cursor_not_usable("discovery pages forward only"));
    }
    match cursor.k.as_slice() {
        [after] if after.len() <= MAX_KEY_LEN && is_canonical(after) => Ok(after.clone()),
        [_] => Err(cursor_not_usable(
            "its position is not a canonical GTS identifier",
        )),
        keys => Err(cursor_not_usable(&format!(
            "a discovery cursor names exactly one key, not {}",
            keys.len()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use toolkit_canonical_errors::{FieldViolation, InvalidArgument};
    use types_registry_sdk::field;

    use super::*;

    const AFTER: &str = "gts.cf.core.example.type.v1~";
    const PATTERN_WILDCARD: &str = "gts.cf.core.example.*";

    fn bound<'a>(pattern: Option<&'a str>, select: &[&str]) -> Binding<'a> {
        filtered(pattern, None, None, select)
    }

    fn filtered<'a>(
        pattern: Option<&'a str>,
        kind: Option<EntityKind>,
        max_chain_depth: Option<u8>,
        select: &[&str],
    ) -> Binding<'a> {
        Binding {
            pattern,
            kind,
            lifecycle: LifecycleFilter::Active,
            max_chain_depth: max_chain_depth.and_then(NonZeroU8::new),
            selection: if select.is_empty() {
                FieldSelection::default()
            } else {
                FieldSelection::parse(select).expect("valid")
            },
        }
    }

    #[test]
    fn a_cursor_round_trips_its_position() -> Result<(), CanonicalError> {
        let token = encode(AFTER, &bound(None, &[]))?;
        assert_eq!(decode(&token, &bound(None, &[]))?, AFTER);
        let filtered = encode(AFTER, &bound(Some(PATTERN_WILDCARD), &["content"]))?;
        assert_eq!(
            decode(&filtered, &bound(Some(PATTERN_WILDCARD), &["content"]))?,
            AFTER
        );
        Ok(())
    }

    /// The token is opaque: neither the position's pattern nor the selection may be
    /// readable off it, or a caller will start editing one.
    #[test]
    fn the_token_does_not_spell_the_query_out() -> Result<(), CanonicalError> {
        let binding = bound(Some(PATTERN_WILDCARD), &["content"]);
        let cursor = read(&encode(AFTER, &binding)?)?;
        let filter = cursor.f.expect("the selection is always bound");
        assert_eq!(Some(&filter), binding_hash(&binding).as_ref());
        for needle in [PATTERN_WILDCARD, "content"] {
            assert!(!filter.contains(needle), "{filter}");
        }
        Ok(())
    }

    /// The one violation a refusal carries.
    fn violation<T: std::fmt::Debug>(result: Result<T, CanonicalError>) -> FieldViolation {
        match result {
            Err(CanonicalError::InvalidArgument {
                ctx: InvalidArgument::FieldViolations { field_violations },
                ..
            }) => match <[FieldViolation; 1]>::try_from(field_violations) {
                Ok([violation]) => violation,
                Err(all) => panic!("expected one violation, got {all:?}"),
            },
            other => panic!("expected a field refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_oversized_token_is_refused_before_decoding() {
        let at_limit = violation(read(&"A".repeat(MAX_TOKEN_LEN)));
        assert!(
            at_limit
                .description
                .starts_with("the cursor cannot be used"),
            "a token at the limit reaches decoding: {at_limit:?}"
        );
        let over = violation(read(&"A".repeat(MAX_TOKEN_LEN + 1)));
        assert_eq!(over.field, "cursor");
        assert_eq!(over.reason, field::VALIDATION_FAILED);
        assert_eq!(
            over.description,
            "cursor must be at most 4096 bytes; this one is 4097"
        );
    }

    #[test]
    fn a_cursor_must_name_exactly_one_key() -> Result<(), CanonicalError> {
        let binding = bound(None, &[]);
        // `CursorV1::decode` refuses an empty key list itself.
        for (k, reason) in [
            (vec![], "invalid cursor: empty or invalid keys"),
            (
                vec![AFTER.to_owned(), AFTER.to_owned()],
                "a discovery cursor names exactly one key, not 2",
            ),
        ] {
            let token = CursorV1 {
                k,
                o: SortDir::Asc,
                s: page_order().to_signed_tokens(),
                f: binding_hash(&binding),
                d: FORWARD.to_owned(),
            }
            .encode()
            .map_err(|e| CanonicalError::internal(e.to_string()).create())?;
            let refused = violation(decode(&token, &binding));
            assert_eq!(refused.field, "cursor");
            assert_eq!(
                refused.description,
                format!("the cursor cannot be used for this request: {reason}")
            );
        }
        Ok(())
    }

    #[test]
    fn a_cursor_from_another_pattern_is_refused() -> Result<(), CanonicalError> {
        let token = encode(AFTER, &bound(Some(PATTERN_WILDCARD), &[]))?;
        assert!(decode(&token, &bound(Some("gts.cf.other.*"), &[])).is_err());
        assert!(decode(&token, &bound(None, &[])).is_err());
        let unfiltered = encode(AFTER, &bound(None, &[]))?;
        assert!(decode(&unfiltered, &bound(Some(PATTERN_WILDCARD), &[])).is_err());
        Ok(())
    }

    #[test]
    fn a_cursor_from_another_selection_is_refused() -> Result<(), CanonicalError> {
        for pattern in [None, Some(PATTERN_WILDCARD)] {
            let token = encode(AFTER, &bound(pattern, &[]))?;
            assert!(decode(&token, &bound(pattern, &["content"])).is_err());
            let content = encode(AFTER, &bound(pattern, &["content"]))?;
            assert!(decode(&content, &bound(pattern, &[])).is_err());
            assert!(decode(&content, &bound(pattern, &["content", "origin"])).is_err());
            // `kind` is mandatory, so naming it leaves the binding unchanged.
            assert_eq!(
                decode(&content, &bound(pattern, &["content", "kind"]))?,
                AFTER
            );
        }
        Ok(())
    }

    /// Absent is its own binding, distinct from every explicit value.
    #[test]
    fn a_cursor_from_another_kind_or_depth_is_refused() -> Result<(), CanonicalError> {
        let kinds = [
            None,
            Some(EntityKind::TypeSchema),
            Some(EntityKind::Instance),
        ];
        let depths = [None, Some(1), Some(2), Some(255)];
        for pattern in [None, Some(PATTERN_WILDCARD)] {
            for issued in kinds.iter().flat_map(|k| depths.map(|d| (*k, d))) {
                let token = encode(AFTER, &filtered(pattern, issued.0, issued.1, &[]))?;
                for resumed in kinds.iter().flat_map(|k| depths.map(|d| (*k, d))) {
                    let result = decode(&token, &filtered(pattern, resumed.0, resumed.1, &[]));
                    assert_eq!(
                        result.is_ok(),
                        issued == resumed,
                        "{issued:?} -> {resumed:?}"
                    );
                }
            }
        }
        Ok(())
    }

    /// `deleted` and `all` are bindings of their own; the default adds no term.
    #[test]
    fn a_cursor_from_another_lifecycle_filter_is_refused() -> Result<(), CanonicalError> {
        let filters = [
            LifecycleFilter::Active,
            LifecycleFilter::Deleted,
            LifecycleFilter::All,
        ];
        let with = |lifecycle| Binding {
            lifecycle,
            ..bound(Some(PATTERN_WILDCARD), &["content"])
        };
        for issued in filters {
            let token = encode(AFTER, &with(issued))?;
            for resumed in filters {
                assert_eq!(
                    decode(&token, &with(resumed)).is_ok(),
                    issued == resumed,
                    "{issued:?} -> {resumed:?}"
                );
            }
        }
        Ok(())
    }

    /// A T22b default-selection token (its canonical spelling is unchanged)
    /// resumes while no `depth`, `kind` or non-default `lifecycle_status` is named.
    #[test]
    fn a_t22b_cursor_resumes_under_the_same_absent_filters() -> Result<(), serde_json::Error> {
        let equals = |field: &str, value: &str| {
            ast::Expr::Compare(
                Box::new(ast::Expr::Identifier(field.to_owned())),
                ast::CompareOperator::Eq,
                Box::new(ast::Expr::Value(ast::Value::String(value.to_owned()))),
            )
        };
        let select = equals("$select", &FieldSelection::default().canonical());
        for (pattern, t22b_filter) in [
            (None, select.clone()),
            (
                Some(PATTERN_WILDCARD),
                ast::Expr::And(
                    Box::new(equals("gts_id", PATTERN_WILDCARD)),
                    Box::new(select),
                ),
            ),
        ] {
            let token = CursorV1 {
                k: vec![AFTER.to_owned()],
                o: SortDir::Asc,
                s: page_order().to_signed_tokens(),
                f: short_filter_hash(Some(&t22b_filter)),
                d: FORWARD.to_owned(),
            }
            .encode()?;
            assert_eq!(
                decode(&token, &bound(pattern, &[])).ok().as_deref(),
                Some(AFTER),
                "{pattern:?}",
            );
            let with_kind = filtered(pattern, Some(EntityKind::Instance), None, &[]);
            assert!(decode(&token, &with_kind).is_err(), "{pattern:?}");
            assert!(decode(&token, &filtered(pattern, None, Some(2), &[])).is_err());
            let deleted = Binding {
                lifecycle: LifecycleFilter::Deleted,
                ..bound(pattern, &[])
            };
            assert!(decode(&token, &deleted).is_err(), "{pattern:?}");
        }
        Ok(())
    }

    /// The binding is the canonical set, never the spelling that produced it.
    #[test]
    fn absent_and_explicit_default_selections_are_interchangeable() -> Result<(), CanonicalError> {
        let explicit = ["origin", "GTS_ID", "kind", "gts_uuid", "lifecycle_status"];
        let token = encode(AFTER, &bound(Some(PATTERN_WILDCARD), &[]))?;
        assert_eq!(
            decode(&token, &bound(Some(PATTERN_WILDCARD), &explicit))?,
            AFTER
        );
        let token = encode(AFTER, &bound(Some(PATTERN_WILDCARD), &explicit))?;
        assert_eq!(decode(&token, &bound(Some(PATTERN_WILDCARD), &[]))?, AFTER);
        let reordered = encode(AFTER, &bound(None, &["kind", "content"]))?;
        assert_eq!(
            decode(&reordered, &bound(None, &["Content", " kind"]))?,
            AFTER
        );
        Ok(())
    }

    /// A T22a token carries no selection binding, so it cannot resume a T22b page.
    #[test]
    fn a_cursor_without_a_selection_binding_is_refused() -> Result<(), serde_json::Error> {
        let token = CursorV1 {
            k: vec![AFTER.to_owned()],
            o: SortDir::Asc,
            s: page_order().to_signed_tokens(),
            f: None,
            d: FORWARD.to_owned(),
        }
        .encode()?;
        assert!(decode(&token, &bound(None, &[])).is_err());
        Ok(())
    }

    #[test]
    fn an_edited_position_is_refused() -> Result<(), CanonicalError> {
        let binding = bound(None, &[]);
        for k in [
            "a".repeat(MAX_KEY_LEN + 1),
            "not a gts id".to_owned(),
            AFTER.replace("example", "ex\u{e9}mple"),
            format!("{AFTER} "),
        ] {
            let token = CursorV1 {
                k: vec![k.clone()],
                o: SortDir::Asc,
                s: page_order().to_signed_tokens(),
                f: binding_hash(&binding),
                d: FORWARD.to_owned(),
            }
            .encode()
            .map_err(|e| CanonicalError::internal(e.to_string()).create())?;
            let refused = violation(decode(&token, &binding));
            assert_eq!(refused.field, "cursor", "{k:?}");
            assert!(
                refused
                    .description
                    .ends_with("not a canonical GTS identifier"),
                "{k:?}: {refused:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn an_unreadable_token_is_refused() {
        // `e30` is base64url of `{}`: JSON without the required fields.
        for token in ["not-base64url-json", "", "e30"] {
            assert_eq!(violation(decode(token, &bound(None, &[]))).field, "cursor");
        }
    }

    #[test]
    fn a_base64url_token_that_is_not_json_is_refused() {
        const NOT_JSON: &str = "bm90IGpzb24"; // base64url of `not json`
        assert_eq!(
            violation(decode(NOT_JSON, &bound(None, &[]))).field,
            "cursor"
        );
    }

    /// The version field is the upgrade path, so a token this build does not know
    /// must be refused rather than read for the fields it recognizes.
    #[test]
    fn an_unknown_cursor_version_is_refused() {
        // `{"v":2,"k":["gts.cf.core.example.type.v1~"],"o":"asc","s":"+gts_id","d":"fwd"}`,
        // which `CursorV1` cannot construct — hence the literal.
        const VERSION_2: &str = "eyJ2IjoyLCJrIjpbImd0cy5jZi5jb3JlLmV4YW1wbGUudHlwZS52MX4iXSwibyI6\
                                 ImFzYyIsInMiOiIrZ3RzX2lkIiwiZCI6ImZ3ZCJ9";
        assert!(decode(VERSION_2, &bound(None, &[])).is_err());
    }

    #[test]
    fn a_cursor_with_another_order_or_direction_is_refused() -> Result<(), CanonicalError> {
        let binding = bound(None, &[]);
        for (o, s, d) in [
            (SortDir::Desc, "-gts_id".to_owned(), FORWARD),
            (SortDir::Asc, page_order().to_signed_tokens(), "bwd"),
        ] {
            let token = CursorV1 {
                k: vec![AFTER.to_owned()],
                o,
                s,
                f: binding_hash(&binding),
                d: d.to_owned(),
            }
            .encode()
            .map_err(|e| CanonicalError::internal(e.to_string()).create())?;
            assert_eq!(violation(decode(&token, &binding)).field, "cursor", "{d}");
        }
        Ok(())
    }
}
