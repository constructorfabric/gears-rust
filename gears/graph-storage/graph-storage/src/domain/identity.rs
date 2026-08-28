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
#[must_use]
pub fn reference_node_key(system: &str, kind: &str, native_id: &str) -> String {
    format!("{system}:{kind}:{native_id}")
}

/// Recursively sort object keys so two semantically identical JSON values
/// hash identically regardless of member order.
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
        other => other.clone(),
    }
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
