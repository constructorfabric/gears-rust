#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use super::permissions::{self, Capability};
use super::*;

#[test]
fn identifiers_are_valid_and_consistent() {
    for type_id in [
        BUNDLE_RESOURCE,
        BUNDLE_VERSION_RESOURCE,
        ASSIGNMENT_RESOURCE,
    ] {
        assert!(type_id.ends_with('~'), "{type_id} is not a type id");
        gts::GtsId::try_new(type_id).unwrap();
    }
    assert_eq!(POLICY_ENTRYPOINT, "deny");
}

#[test]
fn resource_type_schemas_are_in_inventory_under_their_constants() {
    let schemas = toolkit_gts::all_inventory_type_schemas().unwrap();
    for type_id in [
        BUNDLE_RESOURCE,
        BUNDLE_VERSION_RESOURCE,
        ASSIGNMENT_RESOURCE,
    ] {
        let schema = schemas
            .iter()
            .find(|s| s["$id"] == format!("gts://{type_id}"))
            .unwrap_or_else(|| panic!("{type_id} not declared"));
        assert!(
            schema["description"]
                .as_str()
                .is_some_and(|d| d.starts_with("Policy engine ")),
            "{type_id}: {schema}"
        );
        let properties: Vec<_> = schema["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{type_id}: {schema}"))
            .keys()
            .collect();
        assert_eq!(properties, ["id"], "{type_id} is id-only");
    }
}

#[test]
fn permission_catalog_is_in_inventory_and_matches_capabilities() {
    let instances = toolkit_gts::all_inventory_instances().unwrap();
    let mut seen = BTreeSet::new();
    for capability in Capability::ALL {
        let id = capability.permission_id();
        assert!(seen.insert(id), "duplicate permission id {id}");
        assert!(id.starts_with("gts.cf.toolkit.authz.permission.v1~cf.core.policy_engine."));
        gts::GtsId::try_new(id).unwrap();
        let entry = instances
            .iter()
            .find(|i| i["id"] == id)
            .unwrap_or_else(|| panic!("{id} not declared"));
        assert_eq!(entry["resource_type"], permissions::RESOURCE_TYPE);
        assert_eq!(entry["action"], capability.action());
        assert!(
            entry["display_name"]
                .as_str()
                .is_some_and(|d| !d.is_empty())
        );
    }
    assert_eq!(seen.len(), 3, "exactly three permissions");
    let ids: Vec<_> = Capability::ALL.iter().map(|c| c.permission_id()).collect();
    assert_eq!(
        ids,
        [
            "gts.cf.toolkit.authz.permission.v1~cf.core.policy_engine.read.v1",
            "gts.cf.toolkit.authz.permission.v1~cf.core.policy_engine.author.v1",
            "gts.cf.toolkit.authz.permission.v1~cf.core.policy_engine.publish.v1",
        ]
    );
    let actions: BTreeSet<_> = Capability::ALL.iter().map(|c| c.action()).collect();
    assert_eq!(actions.len(), 3, "capabilities collide");
}
