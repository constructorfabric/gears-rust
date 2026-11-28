//! The rules a changed profile schema keeps, and the changes that make it.

use construct_sdk::linked_schema::default_schema;
use construct_sdk::schema_doc::{LinkKindDoc, NodeKindDoc, OwnerDoc, PropertyDoc, SchemaDoc};
use serde_json::json;

use super::profile_schema::{SchemaChange, apply, removed_node_kinds, validate};

fn default_doc() -> SchemaDoc {
    SchemaDoc::of(&default_schema())
}

fn text(name: &str, required: bool) -> PropertyDoc {
    PropertyDoc {
        name: name.to_owned(),
        required,
        schema: json!({ "type": "string", "minLength": 1, "maxLength": 100 }),
    }
}

fn pets() -> NodeKindDoc {
    crate::test_support::pets_kind()
}

fn refusal(changes: &[SchemaChange]) -> String {
    apply(&default_doc(), changes)
        .expect_err("the change is refused")
        .0
}

#[test]
fn the_default_schema_is_valid_and_survives_the_document_form() {
    let doc = default_doc();
    validate(&doc).expect("the default schema keeps the rules");
    assert_eq!(doc.build(), default_schema());
}

#[test]
fn a_kind_is_added_with_two_properties() {
    let next = apply(
        &default_doc(),
        &[SchemaChange::AddNodeKind { kind: pets() }],
    )
    .unwrap();
    let schema = next.build();
    let kind = schema.node("pets").expect("the kind is there");
    assert_eq!(kind.properties.len(), 2);
    assert_eq!(kind.key, "name");
    assert_eq!(next.nodes.len(), default_doc().nodes.len() + 1);
}

#[test]
fn a_property_is_added_and_removed() {
    let with = apply(
        &default_doc(),
        &[SchemaChange::AddProperty {
            kind: "project".to_owned(),
            property: text("codename", false),
        }],
    )
    .unwrap();
    assert!(
        with.build()
            .node("project")
            .unwrap()
            .properties
            .iter()
            .any(|property| property.name == "codename")
    );
    let without = apply(
        &with,
        &[SchemaChange::RemoveProperty {
            kind: "project".to_owned(),
            name: "codename".to_owned(),
        }],
    )
    .unwrap();
    assert_eq!(without, default_doc());
}

#[test]
fn a_link_kind_is_added_between_kinds_that_exist() {
    let link = LinkKindDoc {
        name: "keeps".to_owned(),
        description: "The person keeps the pet.".to_owned(),
        from: vec!["person".to_owned()],
        to: vec!["pets".to_owned()],
        fields: vec![],
    };
    let next = apply(
        &default_doc(),
        &[
            SchemaChange::AddNodeKind { kind: pets() },
            SchemaChange::AddLinkKind { kind: link },
        ],
    )
    .unwrap();
    assert!(next.build().link("keeps").is_some());
}

#[test]
fn a_name_already_in_use_is_refused() {
    let mut project = pets();
    project.name = "project".to_owned();
    assert!(refusal(&[SchemaChange::AddNodeKind { kind: project }]).contains("already in use"));

    let reason = refusal(&[SchemaChange::AddProperty {
        kind: "project".to_owned(),
        property: text("name", false),
    }]);
    assert!(reason.contains("two properties named `name`"), "{reason}");
}

#[test]
fn a_link_to_an_unknown_kind_is_refused() {
    let link = LinkKindDoc {
        name: "keeps".to_owned(),
        description: String::new(),
        from: vec!["person".to_owned()],
        to: vec!["pets".to_owned()],
        fields: vec![],
    };
    let reason = refusal(&[SchemaChange::AddLinkKind { kind: link }]);
    assert!(
        reason.contains("`pets`, which is not a node kind"),
        "{reason}"
    );
}

#[test]
fn a_property_that_is_no_json_schema_is_refused() {
    let mut kind = pets();
    kind.properties.push(PropertyDoc {
        name: "age".to_owned(),
        required: false,
        schema: json!({ "type": "no-such-type" }),
    });
    let reason = refusal(&[SchemaChange::AddNodeKind { kind }]);
    assert!(reason.contains("not a valid JSON schema"), "{reason}");

    let mut kind = pets();
    kind.properties[1].schema = json!("string");
    assert!(
        refusal(&[SchemaChange::AddNodeKind { kind }]).contains("must be a JSON schema object")
    );
}

#[test]
fn a_kind_must_have_its_key_among_its_properties() {
    let mut kind = pets();
    kind.key = "nickname".to_owned();
    assert!(
        refusal(&[SchemaChange::AddNodeKind { kind }]).contains("is not one of its properties")
    );
}

#[test]
fn names_that_the_payload_or_a_type_id_cannot_take_are_refused() {
    for name in ["origin", "learned", "Pets", "my pets", "1pets", ""] {
        let mut kind = pets();
        kind.name = name.to_owned();
        assert!(
            apply(&default_doc(), &[SchemaChange::AddNodeKind { kind }]).is_err(),
            "{name:?}"
        );
    }
}

#[test]
fn removing_a_kind_takes_it_out_of_the_owners_and_link_ends_that_name_it() {
    let doc = default_doc();
    let next = apply(
        &doc,
        &[SchemaChange::RemoveNodeKind {
            name: "project".to_owned(),
        }],
    )
    .unwrap();
    assert_eq!(removed_node_kinds(&doc, &next), ["project"]);
    let schema = next.build();
    assert!(schema.node("project").is_none());
    for kind in &schema.nodes {
        if let construct_sdk::linked_schema::Owner::Kinds(owners) = &kind.owner {
            assert!(!owners.contains(&"project"), "{}", kind.name);
        }
    }
    for link in &schema.links {
        assert!(!link.from.contains(&"project") && !link.to.contains(&"project"));
    }
}

#[test]
fn a_link_kind_left_with_no_end_goes_with_the_kind() {
    let link = LinkKindDoc {
        name: "keeps".to_owned(),
        description: String::new(),
        from: vec!["person".to_owned()],
        to: vec!["pets".to_owned()],
        fields: vec![],
    };
    let with_pets = apply(
        &default_doc(),
        &[
            SchemaChange::AddNodeKind { kind: pets() },
            SchemaChange::AddLinkKind { kind: link },
        ],
    )
    .unwrap();
    let next = apply(
        &with_pets,
        &[SchemaChange::RemoveNodeKind {
            name: "pets".to_owned(),
        }],
    )
    .unwrap();
    assert!(next.links.iter().all(|link| link.name != "keeps"));
    assert_eq!(next, default_doc());
    let mut owned = pets();
    owned.name = "toy".to_owned();
    owned.owner = OwnerDoc::Kinds(vec!["pets".to_owned()]);
    let reason = apply(
        &with_pets,
        &[
            SchemaChange::AddNodeKind { kind: owned },
            SchemaChange::RemoveNodeKind {
                name: "pets".to_owned(),
            },
        ],
    )
    .expect_err("the toy would be owned by nothing")
    .0;
    assert!(reason.contains("names no owner kind"), "{reason}");
    let reason = refusal(&[SchemaChange::RemoveNodeKind {
        name: "nothing".to_owned(),
    }]);
    assert!(reason.contains("no node kind `nothing`"), "{reason}");
}

#[test]
fn a_failing_change_leaves_the_schema_as_it_was() {
    let before = default_doc();
    let result = apply(
        &before,
        &[
            SchemaChange::AddNodeKind { kind: pets() },
            SchemaChange::RemoveLinkKind {
                name: "no_such_link".to_owned(),
            },
        ],
    );
    assert!(result.is_err());
}

#[test]
fn the_sizes_of_a_schema_are_bounded() {
    let mut kind = pets();
    kind.description = "x".repeat(501);
    assert!(refusal(&[SchemaChange::AddNodeKind { kind }]).contains("at most 500"));
    let mut kind = pets();
    kind.properties = (0..33).map(|n| text(&format!("p{n}"), false)).collect();
    kind.key = "p0".to_owned();
    assert!(refusal(&[SchemaChange::AddNodeKind { kind }]).contains("at most 32"));
}

#[test]
fn link_fields_and_link_kinds_are_added_and_removed() {
    let with_field = apply(
        &default_doc(),
        &[SchemaChange::AddLinkField {
            kind: "works_on".to_owned(),
            field: text("weight", false),
        }],
    )
    .unwrap();
    let built = with_field.build();
    let fields = &built.link("works_on").unwrap().fields;
    assert!(fields.iter().any(|field| field.name == "weight"));
    let without = apply(
        &with_field,
        &[SchemaChange::RemoveLinkField {
            kind: "works_on".to_owned(),
            name: "weight".to_owned(),
        }],
    )
    .unwrap();
    assert_eq!(without, default_doc());
    assert!(
        refusal(&[SchemaChange::RemoveLinkField {
            kind: "works_on".to_owned(),
            name: "nothing".to_owned(),
        }])
        .contains("no field `nothing`")
    );

    let removed = apply(
        &default_doc(),
        &[SchemaChange::RemoveLinkKind {
            name: "works_on".to_owned(),
        }],
    )
    .unwrap();
    assert!(removed.links.iter().all(|link| link.name != "works_on"));
    assert_eq!(removed.nodes, default_doc().nodes);
}

#[test]
fn a_local_ref_that_would_not_resolve_in_the_node_is_refused() {
    let mut kind = pets();
    kind.properties.push(PropertyDoc {
        name: "alias".to_owned(),
        required: false,
        schema: json!({ "$ref": "#/definitions/x", "definitions": { "x": { "type": "string" } } }),
    });
    let reason = refusal(&[SchemaChange::AddNodeKind { kind }]);
    assert!(reason.contains("reference into the document"), "{reason}");
}

#[test]
fn a_reference_into_the_document_is_refused_in_a_property_schema() {
    for schema in [
        json!({ "$ref": "#" }),
        json!({ "type": "object", "properties": { "next": { "$ref": "#" } } }),
        json!({ "anyOf": [{ "type": "string" }, { "$ref": "#/definitions/x" }] }),
    ] {
        let mut kind = pets();
        kind.properties.push(PropertyDoc {
            name: "branch".to_owned(),
            required: false,
            schema,
        });
        let reason = refusal(&[SchemaChange::AddNodeKind { kind }]);
        assert!(reason.contains("reference into the document"), "{reason}");
    }
}

#[test]
fn a_schema_past_the_byte_cap_is_refused_and_the_default_is_far_below_it() {
    let default_bytes = serde_json::to_string(&default_doc()).unwrap().len();
    assert!(
        default_bytes * 2 < super::profile_schema::MAX_DOCUMENT_BYTES,
        "the default schema takes {default_bytes} bytes"
    );
    let mut kind = pets();
    for n in 0..30 {
        kind.properties.push(PropertyDoc {
            name: format!("p{n}"),
            required: false,
            schema: json!({ "type": "string", "description": "d".repeat(900), "maxLength": 100 }),
        });
    }
    kind.properties.truncate(32);
    let mut doc = default_doc();
    for n in 0..3 {
        let mut next = kind.clone();
        next.name = format!("big{n}");
        doc.nodes.push(next);
    }
    let reason = validate(&doc).expect_err("too big").0;
    assert!(reason.contains("bytes as JSON"), "{reason}");
}

#[test]
fn a_name_the_process_keeps_already_costs_nothing_and_a_fresh_one_costs_one() {
    use super::profile_schema::names_brought_in;
    let before = default_doc();
    let unique = uuid::Uuid::new_v4().simple().to_string();
    let mut fresh = pets();
    fresh.name = format!("k{unique}");
    fresh.key = format!("n{unique}");
    fresh.properties = vec![text(&fresh.key, true)];
    let next = apply(&before, &[SchemaChange::AddNodeKind { kind: fresh }]).unwrap();
    assert_eq!(
        names_brought_in(&before, &next),
        2,
        "the kind and its one property"
    );

    // Once the process keeps the names, adding them again, or removing and adding the kind back, is free.
    let _kept = next.build();
    assert_eq!(names_brought_in(&before, &next), 0);
    let removed = apply(
        &next,
        &[SchemaChange::RemoveNodeKind {
            name: format!("k{unique}"),
        }],
    )
    .unwrap();
    assert_eq!(
        names_brought_in(&removed, &next),
        0,
        "a removed kind added back"
    );
}

#[test]
fn a_property_that_compiles_alone_but_not_as_a_member_of_the_node_is_refused() {
    // Alone its root `$schema` is not read and it compiles as draft 7; nested, it is a resource of its own with
    // its own dialect, where `items` as an array is no schema.
    let mut kind = pets();
    kind.properties.push(PropertyDoc {
        name: "tags".to_owned(),
        required: false,
        schema: json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": "https://example.test/tags",
            "items": [{ "type": "string" }]
        }),
    });
    let reason = refusal(&[SchemaChange::AddNodeKind { kind }]);
    assert!(reason.contains("together"), "{reason}");
}
