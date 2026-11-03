use construct_sdk::person_types::PROPERTIES;
use serde_json::json;

use super::test_data::{catalog, schema_in_person_type};

#[test]
fn every_property_of_the_person_types_has_its_category_and_cardinality() {
    let catalog = catalog();

    for property in PROPERTIES {
        let spec = catalog.get(property.name).expect("a listed property");
        assert_eq!(
            spec.category.type_id(),
            property.type_id,
            "{}",
            property.name
        );
        assert_eq!(spec.cardinality, property.cardinality, "{}", property.name);
    }
    assert_eq!(catalog.names().count(), PROPERTIES.len());
}

#[test]
fn each_property_checks_a_value_against_its_own_schema() {
    let catalog = catalog();

    for property in catalog.names() {
        let spec = catalog.get(property).expect("a listed property");
        let refusal = spec
            .check(&json!({ "no_such_field": true }))
            .expect_err("an object with an unknown field fits no property");
        assert!(
            refusal.ends_with(&format!("it takes {}", schema_in_person_type(property))),
            "{property}: {refusal}"
        );
    }
}

#[test]
fn an_orcid_must_have_the_orcid_form() {
    let catalog = catalog();
    let orcid = catalog.get("orcid_id").expect("orcid_id");

    assert_eq!(orcid.check(&json!("0000-0002-1825-0097")), Ok(()));
    assert!(orcid.check(&json!(42)).is_err());
    assert!(orcid.check(&json!("orcid of Jane")).is_err());
}

#[test]
fn a_refusal_names_the_broken_rule_and_where_but_not_the_value() {
    let catalog = catalog();
    let long = "7".repeat(10_000);

    let orcid = catalog
        .get("orcid_id")
        .expect("orcid_id")
        .check(&json!(long))
        .expect_err("not an orcid");
    let skill = catalog
        .get("skills")
        .expect("skills")
        .check(&json!({ "name": format!("{long}{long}") }))
        .expect_err("a name over its limit");

    assert!(
        orcid.starts_with("the value breaks `pattern`; it takes "),
        "{orcid}"
    );
    assert!(
        skill.starts_with("`/name` breaks `maxLength`; it takes "),
        "{skill}"
    );
    for refusal in [orcid, skill] {
        assert!(!refusal.contains("7777"), "{refusal}");
    }
}
