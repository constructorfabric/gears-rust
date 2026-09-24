//! Stable identity derivations (`cpt-cf-graph-storage-fr-stable-identity`).
//!
//! Shared by every store implementation, so the built-in store, the fake and
//! any external plugin derive byte-identical keys and hashes — identity is
//! contract, not implementation detail.

use aws_lc_rs::digest::{Context, SHA256, digest as sha256};
use graph_storage_sdk::models::{EdgeSpec, IngestRequest};
use serde_json::Value;
use uuid::Uuid;

/// Deterministic edge key: a hash of (edge type, source key, destination key,
/// discriminator). Field boundaries are length-prefixed so no concatenation
/// of distinct inputs can collide.
#[must_use]
pub fn derive_edge_key(type_uuid: Uuid, edge: &EdgeSpec) -> String {
    // `aws-lc-rs` is the workspace's FIPS-capable backend; a pure-Rust hasher
    // is refused by the DE0708 lint, and under `--features fips` it would not
    // run through the validated module at all.
    let mut hasher = Context::new(&SHA256);
    for part in [
        type_uuid.as_bytes().as_slice(),
        edge.src_node_key.as_bytes(),
        edge.dst_node_key.as_bytes(),
        edge.discriminator.as_deref().unwrap_or("").as_bytes(),
    ] {
        hasher.update(&(part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hex::encode(hasher.finish())
}

/// Reference-node key, derived from the full source-qualified canonical
/// identity `(system, kind, native_id)` — a native id alone is not
/// collision-safe (ADR-0002).
///
/// The two leading members act as delimiters, so they may not contain the
/// delimiter themselves: `("a:b", "c", "d")` and `("a", "b:c", "d")` would
/// otherwise both derive `a:b:c:d` and two distinct upstream objects would
/// converge on one node — silently, since each producer's key matches its own
/// triple. [`reference_key_complaint`] refuses that at the door. The *native
/// id* is free to contain colons: everything after the second one is it, so
/// URLs and URNs keep working, which is what a native id often is.
#[must_use]
pub fn reference_node_key(system: &str, kind: &str, native_id: &str) -> String {
    format!("{system}:{kind}:{native_id}")
}

/// Whether a reference node's key is the one its own `source` triple derives,
/// and what to say when it is not.
///
/// A pure rule rather than a branch inside the validator so it can be tested
/// for what it accepts as well as what it refuses: an incomplete `source` is
/// *not* this rule's complaint (the derivation chain refuses that, with the
/// member it is missing), and reporting a mismatch against a triple with an
/// empty member would name a key nobody could have written.
#[must_use]
pub fn reference_key_complaint(node_key: &str, payload: Option<&Value>) -> Option<String> {
    let source = payload?.get("source")?.as_object()?;
    let member = |k: &str| source.get(k).and_then(Value::as_str).unwrap_or_default();
    let (system, kind, native_id) = (member("system"), member("kind"), member("native_id"));
    if system.is_empty() || kind.is_empty() || native_id.is_empty() {
        return None;
    }
    for (member, value) in [("system", system), ("kind", kind)] {
        if value.contains(':') {
            return Some(format!(
                "`source.{member}` may not contain `:`; it is the separator the reference key is \
                 derived with, and a colon there makes two different source triples derive one \
                 key (`native_id` may contain them freely)"
            ));
        }
    }
    let expected = reference_node_key(system, kind, native_id);
    (node_key != expected).then(|| {
        format!("reference node keys derive from the full source triple; expected `{expected}`")
    })
}

/// Recursively sort object keys and normalize numeric spelling so two
/// semantically identical JSON values hash identically regardless of member
/// order or of how a producer's serializer happened to render its numbers.
fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: std::collections::BTreeMap<String, Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), canonicalize(v)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        Value::Number(number) => Value::Number(canonical_number(number)),
        other => other.clone(),
    }
}

/// The largest magnitude an `f64` represents without gaps between consecutive
/// integers. Above it, a float's integral look says nothing about the integer
/// a producer meant, so the number is left exactly as it was parsed.
const EXACT_INTEGER_LIMIT: f64 = 9_007_199_254_740_992.0; // 2^53

/// One spelling per value.
///
/// `serde_json` keeps the variant it parsed — `1`, `1.0` and `1e0` arrive as
/// `PosInt(1)`, `Float(1.0)` and `Float(1.0)` — and `Display` renders the
/// variant, not the value. Since the hash is taken over the rendered text,
/// two retries of the same logical request hash differently whenever the
/// producer's serializer changes how it writes a whole number, which is the
/// one thing an idempotency key exists to survive. An integral float within
/// the exactly-representable range therefore folds onto its integer form;
/// everything else keeps its parsed spelling, which `ryu` already renders
/// canonically for a given `f64`.
///
/// This deliberately makes request identity coarser than payload equality:
/// `{"n": 1}` and `{"n": 1.0}` are one request, while the stored payloads
/// would not compare equal. That is the intended direction — a replay answers
/// with the original outcome and never applies the second body — but it means
/// the first spelling to arrive is the one that persists.
fn canonical_number(number: &serde_json::Number) -> serde_json::Number {
    if number.is_f64()
        && let Some(float) = number.as_f64()
        && float.fract() == 0.0
        && float.abs() < EXACT_INTEGER_LIMIT
    {
        // `fract() == 0.0` already excludes NaN and both infinities.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the magnitude bound above is exactly the range this cast is lossless over"
        )]
        return serde_json::Number::from(float as i64);
    }
    number.clone()
}

fn node_value(node: &graph_storage_sdk::models::NodeSpec) -> Value {
    serde_json::json!({
        "node_key": node.node_key,
        "type": node.type_id,
        "name": node.name,
        "payload": node.payload.as_ref().map(canonicalize),
        "expected_version": node.expected_version,
    })
}

fn edge_value(edge: &EdgeSpec) -> Value {
    serde_json::json!({
        "type": edge.type_id,
        "src": edge.src_node_key,
        "dst": edge.dst_node_key,
        "discriminator": edge.discriminator,
        "payload": edge.payload.as_ref().map(canonicalize),
    })
}

/// Canonical hash of one ingest request — what the idempotency record stores
/// and what a retry is compared against.
pub fn ingest_request_hash(request: &IngestRequest) -> String {
    let canonical = serde_json::json!({
        "nodes": request.nodes.iter().map(node_value).collect::<Vec<_>>(),
        "edges": request.edges.iter().map(edge_value).collect::<Vec<_>>(),
        "replace_scope": request.replace_scope.as_ref().map(|s| {
            serde_json::json!({
                "attribute": s.attribute,
                "value": s.value,
                "generation": s.generation,
            })
        }),
        "create_phantoms": request.options.create_phantoms,
        // `embed` is part of the request's identity: the same nodes ingested
        // with and without embedding leave the store in different states, so
        // a replay of one must not be answered with the other's receipt.
        "embed": request.options.embed,
    });
    hex::encode(sha256(&SHA256, canonical.to_string().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use graph_storage_sdk::models::NodeSpec;

    /// Two different source triples must not derive one key.
    ///
    /// The edge key length-prefixes every part for exactly this reason; the
    /// reference key is a readable `system:kind:native_id` instead, so the
    /// two members that act as separators may not contain the separator.
    /// `native_id` may: everything after the second colon is it, and a native
    /// id is often a URL.
    #[test]
    fn a_source_triple_cannot_borrow_the_key_separator() {
        let node = |system: &str, kind: &str, native: &str| serde_json::json!({ "source": { "system": system, "kind": kind, "native_id": native } });

        // The collision the rule exists to prevent: both of these would
        // derive `a:b:c:d`, and each producer's key matches its own triple.
        let first = node("a:b", "c", "d");
        let second = node("a", "b:c", "d");
        assert_eq!(
            reference_node_key("a:b", "c", "d"),
            reference_node_key("a", "b:c", "d"),
            "the fixture is the collision, or this test asserts nothing"
        );
        for payload in [&first, &second] {
            let complaint = reference_key_complaint("a:b:c:d", Some(payload))
                .expect("a separator inside the leading members is refused");
            assert!(complaint.contains("may not contain"), "{complaint}");
        }

        // A native id may carry them, which is what a URL or a URN needs.
        let url = node("scm", "commit", "https://example.test/r/commit/abc");
        assert!(
            reference_key_complaint("scm:commit:https://example.test/r/commit/abc", Some(&url))
                .is_none(),
            "a native id may contain colons"
        );
    }

    #[test]
    fn edge_keys_do_not_collide_across_field_boundaries() {
        let type_uuid = Uuid::from_u128(7);
        let a = EdgeSpec {
            type_id: "t".into(),
            src_node_key: "ab".into(),
            dst_node_key: "c".into(),
            ..EdgeSpec::default()
        };
        let b = EdgeSpec {
            type_id: "t".into(),
            src_node_key: "a".into(),
            dst_node_key: "bc".into(),
            ..EdgeSpec::default()
        };
        assert_ne!(
            derive_edge_key(type_uuid, &a),
            derive_edge_key(type_uuid, &b)
        );
    }

    #[test]
    fn a_reference_key_is_accepted_only_when_the_triple_derives_it() {
        let payload = serde_json::json!({
            "source": { "system": "scm", "kind": "repo", "native_id": "42" }
        });
        assert_eq!(reference_key_complaint("scm:repo:42", Some(&payload)), None);
        let complaint = reference_key_complaint("42", Some(&payload))
            .expect("a native id alone is not the key");
        assert!(
            complaint.contains("`scm:repo:42`"),
            "the complaint names the key the producer should have written: {complaint}"
        );
        // Two systems spelling the same native id stay distinct (ADR-0002):
        // the key that suits one is a mismatch for the other.
        let other = serde_json::json!({
            "source": { "system": "tracker", "kind": "repo", "native_id": "42" }
        });
        assert!(reference_key_complaint("scm:repo:42", Some(&other)).is_some());
    }

    #[test]
    fn an_incomplete_source_is_left_to_the_derivation_chain() {
        // No payload, no `source`, and a `source` missing a member are all
        // refused elsewhere, naming what is absent. This rule stays quiet
        // rather than inventing an expected key from empty strings.
        assert_eq!(reference_key_complaint("k", None), None);
        assert_eq!(
            reference_key_complaint("k", Some(&serde_json::json!({"other": 1}))),
            None
        );
        assert_eq!(
            reference_key_complaint(
                "k",
                Some(&serde_json::json!({"source": {"system": "scm", "kind": "repo"}}))
            ),
            None
        );
    }

    #[test]
    fn the_request_hash_ignores_payload_member_order() {
        let make = |payload: serde_json::Value| IngestRequest {
            nodes: vec![NodeSpec {
                node_key: "k".into(),
                type_id: "t".into(),
                payload: Some(payload),
                ..NodeSpec::default()
            }],
            ..IngestRequest::default()
        };
        let one = make(serde_json::json!({"a": 1, "b": 2}));
        let two = make(serde_json::json!({"b": 2, "a": 1}));
        assert_eq!(ingest_request_hash(&one), ingest_request_hash(&two));
    }

    /// A retry the mechanism exists to survive: the same logical request from
    /// a client library that renders whole numbers differently.
    #[test]
    fn the_request_hash_ignores_how_a_whole_number_is_written() {
        let make = |payload: serde_json::Value| IngestRequest {
            nodes: vec![NodeSpec {
                node_key: "k".into(),
                type_id: "t".into(),
                payload: Some(payload),
                ..NodeSpec::default()
            }],
            ..IngestRequest::default()
        };
        for (left, right) in [
            (r#"{"count": 1}"#, r#"{"count": 1.0}"#),
            (r#"{"n": 100}"#, r#"{"n": 1e2}"#),
            (r#"{"n": -7}"#, r#"{"n": -7.0}"#),
            (r#"{"deep": [{"n": 2}]}"#, r#"{"deep": [{"n": 2.0}]}"#),
        ] {
            let parse = |text: &str| serde_json::from_str(text).expect("the fixture is JSON");
            assert_eq!(
                ingest_request_hash(&make(parse(left))),
                ingest_request_hash(&make(parse(right))),
                "`{left}` and `{right}` are one request"
            );
        }
    }

    /// The normalization folds spellings together, not values. Two integers
    /// too large for an `f64` to separate keep their own hashes.
    #[test]
    fn the_request_hash_still_separates_numbers_that_differ() {
        let make = |payload: serde_json::Value| IngestRequest {
            nodes: vec![NodeSpec {
                node_key: "k".into(),
                type_id: "t".into(),
                payload: Some(payload),
                ..NodeSpec::default()
            }],
            ..IngestRequest::default()
        };
        for (left, right) in [
            (r#"{"n": 1}"#, r#"{"n": 2}"#),
            (r#"{"n": 1.5}"#, r#"{"n": 1.25}"#),
            // Both land on the same `f64`; neither may be folded onto it.
            (
                r#"{"n": 10000000000000000001}"#,
                r#"{"n": 10000000000000000002}"#,
            ),
            // A number is not its own decimal spelling.
            (r#"{"n": 1}"#, r#"{"n": "1"}"#),
        ] {
            let parse = |text: &str| serde_json::from_str(text).expect("the fixture is JSON");
            assert_ne!(
                ingest_request_hash(&make(parse(left))),
                ingest_request_hash(&make(parse(right))),
                "`{left}` and `{right}` are different requests"
            );
        }
    }

    #[test]
    fn the_request_hash_sees_content_changes() {
        let make = |name: &str| IngestRequest {
            nodes: vec![NodeSpec {
                node_key: "k".into(),
                type_id: "t".into(),
                name: Some(name.into()),
                ..NodeSpec::default()
            }],
            ..IngestRequest::default()
        };
        assert_ne!(
            ingest_request_hash(&make("one")),
            ingest_request_hash(&make("two"))
        );
    }
}
