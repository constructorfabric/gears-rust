use construct_sdk::linked_schema::Schema;
use serde_json::json;

use super::test_data::catalog;
use crate::domain::profile::profile_schema;

#[test]
fn every_kind_and_link_kind_of_the_schema_is_in_the_catalog() {
    let catalog = catalog();

    for kind in &profile_schema().nodes {
        let spec = catalog.kind(kind.name).expect("a listed kind");
        assert_eq!(spec.category.name(), kind.name);
        assert_eq!(spec.schema(), &Schema::fields_schema(kind), "{}", kind.name);
    }
    for kind in &profile_schema().links {
        let spec = catalog.link(kind.name).expect("a listed link kind");
        assert_eq!(
            spec.schema(),
            &Schema::link_fields_schema(kind),
            "{}",
            kind.name
        );
    }
    assert_eq!(catalog.kind_names().count(), profile_schema().nodes.len());
    assert_eq!(catalog.link_names().count(), profile_schema().links.len());
}

#[test]
fn each_kind_checks_a_value_against_its_own_schema() {
    let catalog = catalog();

    for name in catalog.kind_names() {
        let spec = catalog.kind(name).expect("a listed kind");
        let refusal = spec
            .check(&json!({ "no_such_field": true }))
            .expect_err("an object with an unknown field fits no kind");
        assert!(
            refusal.ends_with(&format!("it takes {}", spec.schema())),
            "{name}: {refusal}"
        );
    }
}

#[test]
fn a_commitment_takes_one_of_three_kinds_and_a_date_as_a_date() {
    let catalog = catalog();
    let commitment = catalog.kind("commitment").expect("commitment");

    assert_eq!(
        commitment
            .check(&json!({ "kind": "promise", "text": "send the script", "due": "2026-11-27" })),
        Ok(())
    );
    assert!(
        commitment
            .check(&json!({ "kind": "wish", "text": "x" }))
            .is_err()
    );
    assert!(
        commitment
            .check(&json!({ "kind": "promise", "text": "x", "due": "Friday" }))
            .is_err()
    );
    assert!(
        commitment.check(&json!({ "kind": "promise" })).is_err(),
        "the text is required"
    );
}

#[test]
fn a_refusal_names_the_broken_rule_and_where_but_not_the_value() {
    let catalog = catalog();
    let long = "7".repeat(10_000);

    let refusal = catalog
        .kind("project")
        .expect("project")
        .check(&json!({ "name": format!("{long}{long}") }))
        .expect_err("a name over its limit");

    assert!(
        refusal.starts_with("`/name` breaks `maxLength`; it takes "),
        "{refusal}"
    );
    assert!(!refusal.contains("7777"), "{refusal}");
}

#[test]
fn a_link_checks_its_fields() {
    let catalog = catalog();
    let works_on = catalog.link("works_on").expect("works_on");

    assert_eq!(works_on.check(&json!({})), Ok(()));
    assert_eq!(
        works_on.check(&json!({ "role": "lead", "since": "2026-01-05" })),
        Ok(())
    );
    assert!(works_on.check(&json!({ "colour": "red" })).is_err());
}
