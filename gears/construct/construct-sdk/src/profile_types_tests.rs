use graph_storage_sdk::gts::{NODE_BASE_TYPE, OWNED_NODE_TYPE, STATIC_EDGE_TYPE};
use jsonschema::{Retrieve, Uri, Validator};
use serde_json::{Value, json};

use super::*;

const GRAPH_STORAGE_SCHEMAS: [(&str, &str); 2] = [
    (
        NODE_BASE_TYPE,
        include_str!(
            "../../../graph-storage/graph-storage/schemas/gts.cf.core.graph.node.v1~.schema.json"
        ),
    ),
    (
        OWNED_NODE_TYPE,
        include_str!(
            "../../../graph-storage/graph-storage/schemas/gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~.schema.json"
        ),
    ),
];

struct GraphStorageSchemas;

impl Retrieve for GraphStorageSchemas {
    fn retrieve(
        &self,
        uri: &Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        GRAPH_STORAGE_SCHEMAS
            .iter()
            .find(|(id, _)| format!("gts://{id}") == uri.as_str())
            .map(|(_, text)| serde_json::from_str(text).expect("a JSON schema"))
            .ok_or_else(|| format!("unresolved schema reference `{uri}`").into())
    }
}

fn schema(graph_type: GraphType) -> Value {
    serde_json::from_str(graph_type.schema).expect("a JSON schema")
}

fn root_validator() -> Validator {
    jsonschema::options()
        .with_retriever(GraphStorageSchemas)
        .build(&schema(PROFILE_ROOT))
        .expect("the root type compiles on graph storage's base schemas")
}

fn root(payload: &Value) -> Value {
    json!({ "node_key": "construct:root", "type": PROFILE_ROOT.type_id, "payload": payload })
}

fn valid_payload() -> Value {
    json!({
        "subject_id": "8b0b4c4e-0000-4000-8000-000000000001",
        "version": 1,
        "removals": [],
    })
}

#[test]
fn each_type_names_itself_and_derives_from_its_family() {
    for (graph_type, family) in [
        (PROFILE_ROOT, OWNED_NODE_TYPE),
        (HAS_FACT, STATIC_EDGE_TYPE),
    ] {
        let schema = schema(graph_type);
        assert_eq!(schema["$id"], format!("gts://{}", graph_type.type_id));
        assert_eq!(schema["allOf"][0]["$ref"], format!("gts://{family}"));
        assert!(
            graph_type.type_id.starts_with(family),
            "{} derives from {family}",
            graph_type.type_id
        );
    }
}

#[test]
fn a_root_with_its_subject_version_and_removals_fits() {
    let validator = root_validator();

    assert!(validator.is_valid(&root(&valid_payload())));
    let mut pending = valid_payload();
    pending["version"] = json!(7);
    pending["removals"] = json!(["construct:t:a", "construct:t:b"]);
    assert!(validator.is_valid(&root(&pending)));
}

#[test]
fn a_root_refuses_what_it_does_not_declare() {
    let validator = root_validator();
    let refused = |change: &dyn Fn(&mut Value)| {
        let mut payload = valid_payload();
        change(&mut payload);
        !validator.is_valid(&root(&payload))
    };

    assert!(refused(&|p| p["extra"] = json!(true)), "closed payload");
    assert!(refused(&|p| p["version"] = json!(0)), "versions start at 1");
    assert!(refused(&|p| p["subject_id"] = json!("not-a-uuid")));
    assert!(
        refused(&|p| p["removals"] = json!(["k", "k"])),
        "unique keys"
    );
    assert!(refused(&|p| p["removals"] = json!([""])), "no empty key");
    for member in ["subject_id", "version", "removals"] {
        assert!(
            refused(&|p| {
                p.as_object_mut().unwrap().remove(member);
            }),
            "{member} is required"
        );
    }
    assert!(
        refused(&|p| p["removals"] = json!((0..1001).map(|i| format!("k{i}")).collect::<Vec<_>>())),
        "at most 1000 removals"
    );
    assert!(
        refused(&|p| p["removals"] = json!(["k".repeat(2049)])),
        "a key of at most 2048 characters"
    );
}

#[test]
fn a_root_takes_as_many_removals_and_as_long_a_key_as_its_bounds_allow() {
    let validator = root_validator();
    let mut payload = valid_payload();
    payload["removals"] = json!(
        (0..1000)
            .map(|i| if i == 0 {
                "k".repeat(2048)
            } else {
                format!("k{i}")
            })
            .collect::<Vec<_>>()
    );

    assert!(validator.is_valid(&root(&payload)));
}

#[test]
fn the_root_and_the_edge_register_before_the_person_types() {
    let types: Vec<&str> = graph_types().iter().map(|t| t.type_id).collect();

    assert_eq!(types[..2], [PROFILE_ROOT.type_id, HAS_FACT.type_id]);
    assert_eq!(types.len(), 2 + PERSON_TYPES.len());
}
