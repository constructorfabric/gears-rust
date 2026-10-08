use construct_sdk::person_types::PROPERTIES;
use serde_json::json;

use super::catalog::PropertyCatalog;

#[test]
fn every_property_of_the_person_types_has_its_category_cardinality_and_value_check() {
    let catalog = PropertyCatalog::load().expect("the catalog");

    for (type_id, property, cardinality) in PROPERTIES {
        let spec = catalog.get(property).expect("a listed property");
        assert_eq!(spec.category.type_id(), *type_id, "{property}");
        assert_eq!(spec.cardinality, *cardinality, "{property}");
    }
    assert_eq!(catalog.names().count(), PROPERTIES.len());
}

#[test]
fn a_value_is_checked_against_its_property_schema() {
    let catalog = PropertyCatalog::load().expect("the catalog");
    let orcid = catalog.get("orcid_id").expect("orcid_id");

    assert!(orcid.check(&json!("0000-0002-1825-0097")).is_ok());
    assert!(orcid.check(&json!(42)).is_err());
    let refusal = orcid
        .check(&json!("orcid of Jane"))
        .expect_err("not an ORCID");
    assert!(refusal.contains("it takes"), "{refusal}");
}
