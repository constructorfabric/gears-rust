//! Unit tests for the public model types in `models.rs`
//! ([`GtsTypeSchema`], [`GtsInstance`], [`RegisterResult`], etc.).
//!
//! Kept in a sibling `_tests.rs` file per the `de1101_tests_in_separate_files`
//! repo lint. Linked into `models.rs` via
//! `#[path = "models_tests.rs"] mod tests;`, so `super::*` pulls every
//! item declared at the top of `models.rs` (`GtsTypeSchema`, `GtsInstance`,
//! `RegisterResult`, etc.) into scope.

#![allow(clippy::needless_pass_by_value)]

use super::*;
use serde_json::json;
use toolkit_canonical_errors::{CanonicalError, InvalidArgument};

/// `try_new` now returns `CanonicalError`; the legacy `is_invalid_gts_*`
/// predicates are gone. The kind distinction collapsed at the canonical
/// boundary (ADR 0005), so both former variants are one `InvalidArgument`
/// with reason `INVALID_GTS_ID`.
fn is_invalid_gts_id(err: &CanonicalError) -> bool {
    matches!(
        err,
        CanonicalError::InvalidArgument {
            ctx: InvalidArgument::FieldViolations { field_violations },
            ..
        } if field_violations
            .iter()
            .any(|v| v.reason == crate::field::INVALID_GTS_ID)
    )
}

fn make_type_schema(
    type_id: &str,
    schema: Value,
    parent: Option<Arc<GtsTypeSchema>>,
) -> GtsTypeSchema {
    GtsTypeSchema::try_new(GtsTypeId::new(type_id), schema, None, parent).unwrap()
}

// Reusable chain-form ids (string-shape valid for derive_parent_type_id).
const BASE_ID: &str = "gts.acme.core.events.base.v1~";
const DERIVED_ID: &str = "gts.acme.core.events.base.v1~acme.core.events.derived.v1.0~";
const LEAF_ID: &str =
    "gts.acme.core.events.base.v1~acme.core.events.derived.v1.0~vendor.x.y.leaf.v1.0~";

#[test]
fn test_type_schema_try_new_extracts_traits() {
    let s = make_type_schema(
        BASE_ID,
        json!({
            "title": "User",
            "x-gts-traits": { "topicRef": "x" },
            "x-gts-traits-schema": { "type": "object" }
        }),
        None,
    );
    assert_eq!(s.title.as_deref(), Some("User"));
    assert!(s.traits.is_some());
    assert!(s.traits_schema.is_some());
    assert!(s.parent.is_none());
}

#[test]
fn test_derive_parent_type_id() {
    // Root (single segment) → no parent.
    assert!(GtsTypeSchema::derive_parent_type_id(BASE_ID).is_none());
    // Derived → strips last segment, returns parent.
    assert_eq!(
        GtsTypeSchema::derive_parent_type_id(DERIVED_ID).map(GtsTypeId::into_string),
        Some(BASE_ID.to_owned())
    );
    // Three-level → parent is the two-level.
    assert_eq!(
        GtsTypeSchema::derive_parent_type_id(LEAF_ID).map(GtsTypeId::into_string),
        Some(DERIVED_ID.to_owned())
    );
    // Instance (no trailing `~`) → no parent (helper is type-schemas-only).
    assert!(GtsTypeSchema::derive_parent_type_id("gts.foo.bar.baz.v1~inst").is_none());
}

#[test]
fn test_type_schema_rejects_instance_id() {
    let err = GtsTypeSchema::try_new(
        GtsTypeId::new("gts.acme.core.events.user.v1~acme.core.instances.u1.v1"),
        json!({}),
        None,
        None,
    )
    .unwrap_err();
    assert!(is_invalid_gts_id(&err));
}

#[test]
fn test_type_schema_rejects_mismatched_parent() {
    // BASE_ID and "gts.contoso.vendor.events.x.v1~" are unrelated; passing
    // the wrong one as parent of DERIVED_ID must error rather than
    // silently corrupt the chain.
    let wrong_parent = Arc::new(make_type_schema(
        "gts.contoso.vendor.events.base.v1~",
        json!({}),
        None,
    ));
    let err = GtsTypeSchema::try_new(
        GtsTypeId::new(DERIVED_ID),
        json!({}),
        None,
        Some(wrong_parent),
    )
    .unwrap_err();
    assert!(is_invalid_gts_id(&err));
}

#[test]
fn test_type_schema_rejects_root_with_parent() {
    // BASE_ID is a root (single segment) — providing any parent is wrong.
    let stray_parent = Arc::new(make_type_schema(BASE_ID, json!({}), None));
    let err = GtsTypeSchema::try_new(GtsTypeId::new(BASE_ID), json!({}), None, Some(stray_parent))
        .unwrap_err();
    assert!(is_invalid_gts_id(&err));
}

#[test]
fn test_type_schema_rejects_derived_without_parent() {
    // DERIVED_ID has a chain prefix — `parent: None` would silently produce a
    // schema whose `ancestors()` walk is incomplete. The constructor must
    // reject this and force the caller to pass the chain.
    let err =
        GtsTypeSchema::try_new(GtsTypeId::new(DERIVED_ID), json!({}), None, None).unwrap_err();
    assert!(is_invalid_gts_id(&err));
}

#[test]
fn test_effective_properties_deep_merge() {
    let base = Arc::new(make_type_schema(
        BASE_ID,
        json!({ "type": "object", "properties": { "id": { "type": "string" } } }),
        None,
    ));
    let child = make_type_schema(
        DERIVED_ID,
        json!({
            "allOf": [
                { "$ref": format!("gts://{BASE_ID}") },
                { "properties": { "name": { "type": "string" } } }
            ]
        }),
        Some(Arc::clone(&base)),
    );
    let merged = child.effective_properties();
    assert!(merged.contains_key("id")); // inherited
    assert!(merged.contains_key("name")); // own
}

#[test]
fn test_effective_properties_child_overrides_parent() {
    let base = Arc::new(make_type_schema(
        BASE_ID,
        json!({ "properties": { "field": { "type": "string", "title": "from-base" } } }),
        None,
    ));
    let child = make_type_schema(
        DERIVED_ID,
        json!({
            "allOf": [{ "$ref": format!("gts://{BASE_ID}") }],
            "properties": { "field": { "type": "string", "title": "from-child" } }
        }),
        Some(base),
    );
    let merged = child.effective_properties();
    let field = merged.get("field").unwrap();
    assert_eq!(
        field.get("title").and_then(|v| v.as_str()),
        Some("from-child")
    );
}

#[test]
fn test_effective_required_dedup() {
    let base = Arc::new(make_type_schema(
        BASE_ID,
        json!({ "required": ["id"] }),
        None,
    ));
    let child = make_type_schema(
        DERIVED_ID,
        json!({
            "allOf": [
                { "$ref": format!("gts://{BASE_ID}") },
                { "required": ["name", "id"] }
            ]
        }),
        Some(base),
    );
    let req = child.effective_required();
    assert_eq!(req.iter().filter(|s| *s == "id").count(), 1);
    assert!(req.contains(&"name".to_owned()));
}

#[test]
fn test_effective_traits_rightmost_wins() {
    let base = Arc::new(make_type_schema(
        BASE_ID,
        json!({ "x-gts-traits": { "retention": "P30D", "scope": "global" } }),
        None,
    ));
    let child = make_type_schema(
        DERIVED_ID,
        json!({ "x-gts-traits": { "retention": "P90D" } }),
        Some(base),
    );
    let merged = child.effective_traits();
    let m = merged.as_object().unwrap();
    assert_eq!(m.get("retention").and_then(|v| v.as_str()), Some("P90D"));
    assert_eq!(m.get("scope").and_then(|v| v.as_str()), Some("global"));
}

#[test]
fn test_effective_traits_schema_chain_order() {
    let base = Arc::new(make_type_schema(
        BASE_ID,
        json!({ "x-gts-traits-schema": { "type": "object", "properties": { "a": {} } } }),
        None,
    ));
    let child = make_type_schema(
        DERIVED_ID,
        json!({ "x-gts-traits-schema": { "type": "object", "properties": { "b": {} } } }),
        Some(base),
    );
    let chain = child.effective_traits_schema();
    assert_eq!(chain.len(), 2);
    // Deepest base first.
    assert!(chain[0]["properties"]["a"].is_object());
    assert!(chain[1]["properties"]["b"].is_object());
}

#[test]
fn test_effective_schema_inlines_refs() {
    let base = Arc::new(make_type_schema(
        BASE_ID,
        json!({ "type": "object", "properties": { "id": { "type": "string" } } }),
        None,
    ));
    let child = make_type_schema(
        DERIVED_ID,
        json!({
            "allOf": [
                { "$ref": format!("gts://{BASE_ID}") },
                { "properties": { "name": { "type": "string" } } }
            ]
        }),
        Some(base),
    );
    let merged = child.effective_schema();
    let all_of = merged["allOf"].as_array().unwrap();
    // First entry is the inlined base (no longer a $ref).
    assert!(all_of[0].get("$ref").is_none());
    assert!(all_of[0]["properties"]["id"].is_object());
    // Second entry is the own overlay, untouched.
    assert!(all_of[1]["properties"]["name"].is_object());
}

#[test]
fn test_effective_properties_three_level_chain() {
    // grandparent → parent → child. Properties from each level surface
    // in `effective_properties`; later levels win on key collisions.
    let grand = Arc::new(make_type_schema(
        BASE_ID,
        json!({ "properties": {
            "a": { "type": "string", "title": "from-grand" },
            "shared": { "type": "string", "title": "from-grand" }
        }}),
        None,
    ));
    let parent = Arc::new(make_type_schema(
        DERIVED_ID,
        json!({
            "allOf": [{ "$ref": format!("gts://{BASE_ID}") }],
            "properties": {
                "b": { "type": "string", "title": "from-parent" },
                "shared": { "type": "string", "title": "from-parent" }
            }
        }),
        Some(grand),
    ));
    let child = make_type_schema(
        LEAF_ID,
        json!({
            "allOf": [{ "$ref": format!("gts://{DERIVED_ID}") }],
            "properties": {
                "c": { "type": "string", "title": "from-child" },
                "shared": { "type": "string", "title": "from-child" }
            }
        }),
        Some(parent),
    );
    let merged = child.effective_properties();
    assert!(merged.contains_key("a")); // from grandparent
    assert!(merged.contains_key("b")); // from parent
    assert!(merged.contains_key("c")); // from child
    // child wins on `shared` over grandparent and parent.
    assert_eq!(
        merged
            .get("shared")
            .unwrap()
            .get("title")
            .and_then(|v| v.as_str()),
        Some("from-child")
    );
}

#[test]
fn test_effective_properties_root_no_parent() {
    // Root schema (no parent) — effective_properties returns own only.
    let schema = make_type_schema(
        BASE_ID,
        json!({ "properties": {
            "x": { "type": "integer" },
            "y": { "type": "string" }
        }}),
        None,
    );
    let merged = schema.effective_properties();
    assert_eq!(merged.len(), 2);
    assert!(merged.contains_key("x"));
    assert!(merged.contains_key("y"));
}

#[test]
fn test_effective_properties_from_allof_inline_overlay() {
    // Properties declared inside allOf overlay branches (non-$ref) are
    // counted as "own" — the parent's body comes from `self.parent`.
    let base = Arc::new(make_type_schema(
        BASE_ID,
        json!({ "properties": { "inherited": { "type": "string" } } }),
        None,
    ));
    let child = make_type_schema(
        DERIVED_ID,
        json!({
            "allOf": [
                { "$ref": format!("gts://{BASE_ID}") },
                { "properties": { "from_overlay": { "type": "string" } } }
            ]
        }),
        Some(base),
    );
    let merged = child.effective_properties();
    assert!(merged.contains_key("inherited"));
    assert!(merged.contains_key("from_overlay"));
}

#[test]
fn test_effective_required_three_level_dedup_and_order() {
    // Each level adds new required fields; duplicates dedup, order is
    // preserved by first occurrence in pre-order walk (self → ancestors).
    let grand = Arc::new(make_type_schema(
        BASE_ID,
        json!({ "required": ["id"] }),
        None,
    ));
    let parent = Arc::new(make_type_schema(
        DERIVED_ID,
        json!({
            "allOf": [{ "$ref": format!("gts://{BASE_ID}") }],
            "required": ["name", "id"]
        }),
        Some(grand),
    ));
    let child = make_type_schema(
        LEAF_ID,
        json!({
            "allOf": [{ "$ref": format!("gts://{DERIVED_ID}") }],
            "required": ["age", "name"]
        }),
        Some(parent),
    );
    let req = child.effective_required();
    // All three unique keys present.
    assert!(req.contains(&"id".to_owned()));
    assert!(req.contains(&"name".to_owned()));
    assert!(req.contains(&"age".to_owned()));
    // Each appears exactly once (dedup across levels).
    assert_eq!(req.iter().filter(|s| *s == "id").count(), 1);
    assert_eq!(req.iter().filter(|s| *s == "name").count(), 1);
    assert_eq!(req.iter().filter(|s| *s == "age").count(), 1);
    // Pre-order is self → ancestors, so child's "age" precedes
    // parent's "name", which precedes grandparent's "id".
    let pos = |s: &str| req.iter().position(|r| r == s).unwrap();
    assert!(pos("age") < pos("name"));
    assert!(pos("name") < pos("id"));
}

#[test]
fn test_effective_required_root_no_parent() {
    let schema = make_type_schema(BASE_ID, json!({ "required": ["a", "b", "c"] }), None);
    let req = schema.effective_required();
    assert_eq!(req, vec!["a", "b", "c"]);
}

#[test]
fn test_effective_traits_three_level_rightmost_wins() {
    // Rightmost (= self in our walk) wins; deeper ancestors fill
    // missing keys.
    let grand = Arc::new(make_type_schema(
        BASE_ID,
        json!({ "x-gts-traits": {
            "from_grand_only": "g",
            "shared": "from-grand"
        }}),
        None,
    ));
    let parent = Arc::new(make_type_schema(
        DERIVED_ID,
        json!({ "x-gts-traits": {
            "from_parent_only": "p",
            "shared": "from-parent"
        }}),
        Some(grand),
    ));
    let child = make_type_schema(
        LEAF_ID,
        json!({ "x-gts-traits": {
            "from_child_only": "c",
            "shared": "from-child"
        }}),
        Some(parent),
    );
    let merged = child.effective_traits();
    let m = merged.as_object().unwrap();
    assert_eq!(m.get("from_grand_only").and_then(|v| v.as_str()), Some("g"));
    assert_eq!(
        m.get("from_parent_only").and_then(|v| v.as_str()),
        Some("p")
    );
    assert_eq!(m.get("from_child_only").and_then(|v| v.as_str()), Some("c"));
    assert_eq!(m.get("shared").and_then(|v| v.as_str()), Some("from-child"));
}

#[test]
fn test_effective_traits_returns_null_when_chain_has_none() {
    // No level declares x-gts-traits → effective_traits returns Null.
    let base = Arc::new(make_type_schema(BASE_ID, json!({}), None));
    let child = make_type_schema(DERIVED_ID, json!({}), Some(base));
    assert!(child.effective_traits().is_null());
}

#[test]
fn test_effective_traits_partial_chain_coverage() {
    // Only a middle-of-chain ancestor declares traits — they should
    // surface even though closer levels (self, deepest) declare nothing.
    let grand = Arc::new(make_type_schema(BASE_ID, json!({}), None));
    let parent = Arc::new(make_type_schema(
        DERIVED_ID,
        json!({ "x-gts-traits": { "scope": "tenant" } }),
        Some(grand),
    ));
    let child = make_type_schema(LEAF_ID, json!({}), Some(parent));
    let merged = child.effective_traits();
    assert_eq!(
        merged
            .as_object()
            .and_then(|m| m.get("scope"))
            .and_then(|v| v.as_str()),
        Some("tenant")
    );
}

#[test]
fn test_effective_traits_applies_defaults_from_traits_schema() {
    // Base declares a traits-schema with defaults; nothing declares the
    // values. effective_traits must surface the defaults.
    let base = Arc::new(make_type_schema(
        BASE_ID,
        json!({
            "x-gts-traits-schema": {
                "type": "object",
                "properties": {
                    "allowed_parent_types": { "type": "array", "default": [] },
                    "idp_provisioning":     { "type": "boolean", "default": false }
                }
            }
        }),
        None,
    ));
    let child = make_type_schema(DERIVED_ID, json!({}), Some(base));
    let merged = child.effective_traits();
    let m = merged.as_object().expect("not null");
    assert!(
        m.get("allowed_parent_types")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(m.get("idp_provisioning"), Some(&json!(false)));
}

#[test]
fn test_effective_traits_declared_value_beats_default() {
    // Default says false, leaf declares true. Declared wins.
    let base = Arc::new(make_type_schema(
        BASE_ID,
        json!({
            "x-gts-traits-schema": {
                "properties": {
                    "idp_provisioning": { "type": "boolean", "default": false }
                }
            }
        }),
        None,
    ));
    let child = make_type_schema(
        DERIVED_ID,
        json!({ "x-gts-traits": { "idp_provisioning": true } }),
        Some(base),
    );
    let merged = child.effective_traits();
    assert_eq!(
        merged.get("idp_provisioning").and_then(Value::as_bool),
        Some(true)
    );
}

#[test]
fn test_effective_traits_default_from_descendant_traits_schema() {
    // Narrowing: the descendant introduces a NEW property in its
    // traits-schema with a default. Even though the schema declaration
    // lives below the base, its default still surfaces (no ancestor
    // declared this property).
    let base = Arc::new(make_type_schema(BASE_ID, json!({}), None));
    let child = make_type_schema(
        DERIVED_ID,
        json!({
            "x-gts-traits-schema": {
                "properties": {
                    "retention": { "type": "string", "default": "P30D" }
                }
            }
        }),
        Some(base),
    );
    let merged = child.effective_traits();
    assert_eq!(
        merged.get("retention").and_then(|v| v.as_str()),
        Some("P30D")
    );
}

#[test]
fn test_effective_traits_only_defaults_no_declared_values() {
    // Verifies that effective_traits is NOT Null when only defaults exist
    // (no x-gts-traits declared anywhere).
    let base = Arc::new(make_type_schema(
        BASE_ID,
        json!({
            "x-gts-traits-schema": {
                "properties": {
                    "scope": { "type": "string", "default": "global" }
                }
            }
        }),
        None,
    ));
    let child = make_type_schema(DERIVED_ID, json!({}), Some(base));
    let merged = child.effective_traits();
    assert_eq!(merged.get("scope").and_then(|v| v.as_str()), Some("global"));
}

#[test]
fn test_effective_traits_returns_null_with_default_less_traits_schema() {
    // A traits-schema that declares no defaults must not push spurious
    // entries into the result. Result remains Null when nothing is declared.
    let base = Arc::new(make_type_schema(
        BASE_ID,
        json!({
            "x-gts-traits-schema": {
                "properties": {
                    "scope": { "type": "string" }
                }
            }
        }),
        None,
    ));
    let child = make_type_schema(DERIVED_ID, json!({}), Some(base));
    assert!(child.effective_traits().is_null());
}

#[test]
fn test_effective_traits_schema_three_level_chain_order() {
    // Order is deepest-base first → self last.
    let grand = Arc::new(make_type_schema(
        BASE_ID,
        json!({ "x-gts-traits-schema": { "marker": "grand" } }),
        None,
    ));
    let parent = Arc::new(make_type_schema(
        DERIVED_ID,
        json!({ "x-gts-traits-schema": { "marker": "parent" } }),
        Some(grand),
    ));
    let child = make_type_schema(
        LEAF_ID,
        json!({ "x-gts-traits-schema": { "marker": "child" } }),
        Some(parent),
    );
    let chain = child.effective_traits_schema();
    assert_eq!(chain.len(), 3);
    assert_eq!(chain[0]["marker"].as_str(), Some("grand"));
    assert_eq!(chain[1]["marker"].as_str(), Some("parent"));
    assert_eq!(chain[2]["marker"].as_str(), Some("child"));
}

#[test]
fn test_effective_traits_schema_skips_levels_without_block() {
    // Only the levels that declare x-gts-traits-schema appear.
    let grand = Arc::new(make_type_schema(
        BASE_ID,
        json!({ "x-gts-traits-schema": { "marker": "grand" } }),
        None,
    ));
    let parent = Arc::new(make_type_schema(DERIVED_ID, json!({}), Some(grand)));
    let child = make_type_schema(
        LEAF_ID,
        json!({ "x-gts-traits-schema": { "marker": "child" } }),
        Some(parent),
    );
    let chain = child.effective_traits_schema();
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0]["marker"].as_str(), Some("grand"));
    assert_eq!(chain[1]["marker"].as_str(), Some("child"));
}

#[test]
fn test_effective_schema_root_returns_body_unchanged() {
    // Root: no parent, no inlining; the body is returned as-is.
    let body = json!({
        "type": "object",
        "properties": { "id": { "type": "string" } }
    });
    let schema = make_type_schema(BASE_ID, body.clone(), None);
    assert_eq!(schema.effective_schema(), body);
}

#[test]
fn test_effective_schema_strips_id_and_schema_from_inlined_parent() {
    // Parent's `$id` and `$schema` are stripped when inlined to keep
    // the merged document a valid composite schema.
    let base = Arc::new(make_type_schema(
        BASE_ID,
        json!({
            "$id": format!("gts://{BASE_ID}"),
            "$schema": "https://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "id": { "type": "string" } }
        }),
        None,
    ));
    let child = make_type_schema(
        DERIVED_ID,
        json!({
            "allOf": [{ "$ref": format!("gts://{BASE_ID}") }]
        }),
        Some(base),
    );
    let merged = child.effective_schema();
    let inlined = &merged["allOf"][0];
    assert!(inlined.get("$id").is_none());
    assert!(inlined.get("$schema").is_none());
    // But the actual content survives.
    assert!(inlined["properties"]["id"].is_object());
}

#[test]
fn test_effective_schema_leaves_non_parent_refs_alone() {
    // A `$ref` in `allOf` that points to something OTHER than the GTS
    // parent (a "mixin") is left as-is — only the parent's body is inlined.
    let base = Arc::new(make_type_schema(BASE_ID, json!({"type": "object"}), None));
    let mixin_id = "gts.contoso.vendor.events.mixin.v1~";
    let child = make_type_schema(
        DERIVED_ID,
        json!({
            "allOf": [
                { "$ref": format!("gts://{BASE_ID}") },
                { "$ref": format!("gts://{mixin_id}") }
            ]
        }),
        Some(base),
    );
    let merged = child.effective_schema();
    let all_of = merged["allOf"].as_array().unwrap();
    // Parent inlined.
    assert!(all_of[0].get("$ref").is_none());
    // Mixin ref preserved.
    assert_eq!(
        all_of[1]["$ref"].as_str(),
        Some(format!("gts://{mixin_id}").as_str())
    );
}

#[test]
fn test_ancestors_chain_walk() {
    let grandparent = Arc::new(make_type_schema(BASE_ID, json!({}), None));
    let parent = Arc::new(make_type_schema(
        DERIVED_ID,
        json!({}),
        Some(Arc::clone(&grandparent)),
    ));
    let child = make_type_schema(LEAF_ID, json!({}), Some(parent));
    let ids: Vec<String> = child.ancestors().map(|s| s.type_id.to_string()).collect();
    assert_eq!(ids, vec![LEAF_ID, DERIVED_ID, BASE_ID]);
}

#[test]
fn test_instance_try_new_validates_chain_match() {
    let type_schema = Arc::new(make_type_schema(
        "gts.acme.core.events.user.v1~",
        json!({}),
        None,
    ));
    let inst = GtsInstance::try_new(
        GtsInstanceId::new("gts.acme.core.events.user.v1~", "acme.core.instances.u1.v1"),
        json!({ "id": "acme.core.instances.u1.v1" }),
        None,
        type_schema,
    )
    .unwrap();
    assert_eq!(inst.type_id().as_ref(), "gts.acme.core.events.user.v1~");
}

#[test]
fn test_instance_try_new_rejects_mismatched_type_schema() {
    let type_schema = Arc::new(make_type_schema(
        "gts.acme.other.pkg.type.v1~",
        json!({}),
        None,
    ));
    let err = GtsInstance::try_new(
        GtsInstanceId::new("gts.acme.core.events.user.v1~", "u1"),
        json!({}),
        None,
        type_schema,
    )
    .unwrap_err();
    assert!(is_invalid_gts_id(&err));
}

#[test]
fn test_instance_rejects_type_id() {
    let type_schema = Arc::new(make_type_schema(
        "gts.acme.core.users.user.v1~",
        json!({}),
        None,
    ));
    let err = GtsInstance::try_new(
        GtsInstanceId::new("gts.acme.core.users.user.v1~", ""),
        json!({}),
        None,
        type_schema,
    )
    .unwrap_err();
    assert!(is_invalid_gts_id(&err));
}

#[test]
fn test_type_schema_query_builder() {
    let empty = TypeSchemaQuery::new();
    assert!(empty.is_empty());

    let q = TypeSchemaQuery::new().with_pattern("gts.acme.*");
    assert!(!q.is_empty());
    assert_eq!(q.pattern.as_deref(), Some("gts.acme.*"));
}
