use std::collections::HashSet;

use jsonschema::Validator;
use serde_json::{Value, json};
use uuid::Uuid;

use super::*;
use crate::graph_schemas_tests::GraphStorageSchemas;

fn prefix() -> SchemaPrefix {
    SchemaPrefix::of(Uuid::from_u128(0x8b0b_4c4e_0000_4000_8000_0000_0000_0001))
}

fn validator(schema: &Value) -> Validator {
    jsonschema::options()
        .with_retriever(GraphStorageSchemas)
        .build(schema)
        .expect("the type compiles on graph storage's base schemas")
}

fn node_validator(kind: &str) -> Validator {
    let types = default_schema().graph_types(&prefix());
    let id = Schema::node_type_id(&prefix(), kind);
    validator(
        &types
            .iter()
            .find(|t| t.type_id == id)
            .expect("a type")
            .schema,
    )
}

fn node(payload: &Value) -> Value {
    json!({ "node_key": "construct:t:n", "type": "x", "payload": payload })
}

#[test]
fn the_default_schema_has_the_kinds_of_the_linked_profile() {
    let schema = default_schema();

    let names: Vec<&str> = schema.nodes.iter().map(|kind| kind.name).collect();
    assert_eq!(
        names,
        [
            "me",
            "person",
            "project",
            "product",
            "activity",
            "team",
            "customer",
            "partner",
            "competitor",
            "commitment",
            "decision",
            "question",
            "metric",
            "ticket",
            "source"
        ]
    );
    let references: Vec<&str> = schema
        .nodes
        .iter()
        .filter(|kind| kind.reference)
        .map(|kind| kind.name)
        .collect();
    assert_eq!(references, ["ticket", "source"]);
    let singles: Vec<&str> = schema
        .nodes
        .iter()
        .filter(|kind| kind.single)
        .map(|kind| kind.name)
        .collect();
    assert_eq!(singles, ["me"]);
}

#[test]
fn every_link_names_kinds_of_the_schema_and_no_name_is_used_twice() {
    let schema = default_schema();

    let nodes: HashSet<&str> = schema.nodes.iter().map(|kind| kind.name).collect();
    assert_eq!(nodes.len(), schema.nodes.len());
    let links: HashSet<&str> = schema.links.iter().map(|kind| kind.name).collect();
    assert_eq!(links.len(), schema.links.len());
    for link in &schema.links {
        assert!(
            !link.from.is_empty() && !link.to.is_empty(),
            "{}",
            link.name
        );
        for end in link.from.iter().chain(&link.to) {
            assert!(nodes.contains(end), "{} names the kind {end}", link.name);
        }
    }
    for kind in &schema.nodes {
        if let Owner::Kinds(owners) = &kind.owner {
            for owner in owners {
                assert!(nodes.contains(owner), "{} is owned by {owner}", kind.name);
            }
        }
        let key = kind
            .properties
            .iter()
            .find(|property| property.name == kind.key)
            .unwrap_or_else(|| panic!("{} has no key property {}", kind.name, kind.key));
        assert!(key.required, "the key of {} is required", kind.name);
    }
}

#[test]
fn the_links_of_the_story_are_there() {
    let schema = default_schema();

    for (name, from, to) in [
        ("works_on", "person", "project"),
        ("member_of", "person", "team"),
        ("lead", "team", "person"),
        ("knows", "person", "person"),
        ("account_of", "person", "customer"),
        ("about", "project", "product"),
        ("done_by", "project", "team"),
        ("related_to", "project", "project"),
        ("serves", "activity", "project"),
        ("with", "activity", "customer"),
        ("owned_by", "product", "team"),
        ("uses", "product", "product"),
        ("supplies", "partner", "product"),
        ("competes_with", "competitor", "product"),
        ("owner", "commitment", "person"),
        ("counterpart", "commitment", "person"),
        ("decided_by", "decision", "person"),
        ("asked_by", "question", "person"),
        ("tracked_in", "commitment", "ticket"),
        ("came_from", "commitment", "source"),
        ("came_from", "decision", "source"),
        ("learned_from", "project", "source"),
        ("learned_from", "person", "source"),
    ] {
        let link = schema
            .link(name)
            .unwrap_or_else(|| panic!("no link {name}"));
        assert!(link.from.contains(&from) && link.to.contains(&to), "{name}");
    }
    let works_on = schema.link("works_on").unwrap();
    let fields: Vec<&str> = works_on.fields.iter().map(|field| field.name).collect();
    assert_eq!(fields, ["role", "task", "since"]);
}

#[test]
fn the_prefix_of_a_person_keeps_the_kinds_of_two_people_apart() {
    let (first, second) = (
        SchemaPrefix::of(Uuid::from_u128(1)),
        SchemaPrefix::of(Uuid::from_u128(2)),
    );

    let a = Schema::node_type_id(&first, "project");
    let b = Schema::node_type_id(&second, "project");

    assert_ne!(a, b);
    assert!(a.starts_with("gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.u"));
    assert!(a.ends_with(".project.v1~"), "{a}");
    assert_eq!(first.as_str().len(), 33);
    assert_ne!(
        Schema::link_type_id(&first, "works_on"),
        Schema::link_type_id(&second, "works_on")
    );
}

#[test]
fn a_type_id_names_its_kind_back() {
    let schema = default_schema();
    let prefix = prefix();

    for kind in &schema.nodes {
        let id = Schema::node_type_id(&prefix, kind.name);
        assert_eq!(
            schema.node_of_type_id(&prefix, &id).map(|k| k.name),
            Some(kind.name)
        );
        assert!(schema.link_of_type_id(&prefix, &id).is_none());
    }
    for kind in &schema.links {
        let id = Schema::link_type_id(&prefix, kind.name);
        assert_eq!(
            schema.link_of_type_id(&prefix, &id).map(|k| k.name),
            Some(kind.name)
        );
    }
    let other = SchemaPrefix::of(Uuid::from_u128(9));
    assert!(
        schema
            .node_of_type_id(&other, &Schema::node_type_id(&prefix, "project"))
            .is_none(),
        "another person's kind is not this person's"
    );
}

#[test]
fn the_graph_types_are_every_node_and_link_kind_and_derive_from_their_family() {
    let schema = default_schema();

    let types = schema.graph_types(&prefix());

    assert_eq!(types.len(), schema.nodes.len() + schema.links.len());
    for graph_type in &types {
        assert_eq!(
            graph_type.schema["$id"],
            format!("gts://{}", graph_type.type_id)
        );
        let family = if graph_type.type_id.starts_with(NODE_BASE) {
            NODE_BASE
        } else {
            EDGE_BASE
        };
        assert_eq!(
            graph_type.schema["allOf"][0]["$ref"],
            format!("gts://{family}")
        );
        // Compiles on graph storage's base schemas.
        validator(&graph_type.schema);
    }
}

#[test]
fn a_node_holds_the_object_of_its_fields_in_one_property_named_like_its_kind() {
    let project = node_validator("project");
    let valid = json!({
        "project": { "name": "Construct Browser onboarding", "status": "active", "space": "work" },
        "origin": { "produced_by": { "subject_id": "x" }, "method": "m", "produced_at": "2026-11-26T10:00:00Z" },
        "record": { "connector": "chat", "provenance": "p", "version": "1" },
        "learned": [{ "tool": "chat", "message": "p" }],
    });

    assert!(project.is_valid(&node(&valid)));
    let refused = |change: &dyn Fn(&mut Value)| {
        let mut payload = valid.clone();
        change(&mut payload);
        !project.is_valid(&node(&payload))
    };
    assert!(
        refused(&|p| p["project"]["colour"] = json!("red")),
        "closed fields"
    );
    assert!(
        refused(&|p| p["project"]["space"] = json!("holiday")),
        "a list of values"
    );
    assert!(
        refused(&|p| p["project"]["name"] = json!("")),
        "no empty name"
    );
    assert!(refused(&|p| p["extra"] = json!(true)), "closed payload");
    assert!(refused(&|p| {
        p.as_object_mut().unwrap().remove("project");
    }));
    assert!(refused(&|p| {
        p.as_object_mut().unwrap().remove("origin");
    }));
    assert!(refused(&|p| p["learned"] = json!(
        (0..51)
            .map(|i| json!({ "tool": format!("t{i}") }))
            .collect::<Vec<_>>()
    )));
}

#[test]
fn a_node_of_an_entry_kind_names_its_owner() {
    let commitment = node_validator("commitment");
    let valid = json!({
        "commitment": { "kind": "promise", "text": "send the script", "due": "2026-11-27" },
        "owner": "construct:t:project",
        "origin": { "produced_by": { "subject_id": "x" }, "method": "m", "produced_at": "2026-11-26T10:00:00Z" },
    });

    assert!(commitment.is_valid(&node(&valid)));
    let mut ownerless = valid.clone();
    ownerless.as_object_mut().unwrap().remove("owner");
    assert!(!commitment.is_valid(&node(&ownerless)));
    let mut wrong = valid;
    wrong["commitment"]["kind"] = json!("wish");
    assert!(!commitment.is_valid(&node(&wrong)));
    // A thing takes no owner member at all: the root owns it.
    let project = node_validator("project");
    let mut owned = json!({
        "project": { "name": "Atlas" },
        "origin": { "produced_by": { "subject_id": "x" }, "method": "m", "produced_at": "2026-11-26T10:00:00Z" },
    });
    assert!(project.is_valid(&node(&owned)));
    owned["owner"] = json!("construct:t:x");
    assert!(!project.is_valid(&node(&owned)));
}

#[test]
fn a_link_holds_its_fields_its_origin_and_the_record() {
    let types = default_schema().graph_types(&prefix());
    let id = Schema::link_type_id(&prefix(), "works_on");
    let works_on = validator(&types.iter().find(|t| t.type_id == id).unwrap().schema);
    let edge = |payload: &Value| json!({ "type": "x", "src_node_key": "a", "dst_node_key": "b", "payload": payload });
    let valid = json!({
        "fields": { "role": "lead", "since": "2026-01-05" },
        "origin": { "produced_by": { "subject_id": "x" }, "method": "m", "produced_at": "2026-11-26T10:00:00Z" },
        "record": { "connector": "chat", "provenance": "p", "version": "1" },
    });

    assert!(works_on.is_valid(&edge(&valid)));
    let mut bad = valid.clone();
    bad["fields"]["colour"] = json!("red");
    assert!(!works_on.is_valid(&edge(&bad)));
    let mut no_origin = valid;
    no_origin.as_object_mut().unwrap().remove("origin");
    assert!(!works_on.is_valid(&edge(&no_origin)));
}

#[test]
fn the_endpoints_of_a_link_type_are_its_kinds_for_that_person() {
    let types = default_schema().graph_types(&prefix());
    let id = Schema::link_type_id(&prefix(), "works_on");

    let works_on = &types.iter().find(|t| t.type_id == id).unwrap().schema;

    assert_eq!(
        works_on["x-gts-traits"]["src_types"],
        json!([
            Schema::node_type_id(&prefix(), "person"),
            Schema::node_type_id(&prefix(), "me")
        ])
    );
    assert_eq!(
        works_on["x-gts-traits"]["dst_types"],
        json!([Schema::node_type_id(&prefix(), "project")])
    );
}
