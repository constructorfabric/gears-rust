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
    assert_eq!(
        ADMISSION_ENGINE_INSTANCE_ID,
        "gts.cf.toolkit.plugins.plugin.v1~cf.core.admission_control.engine.v1~cf.core.policy_engine.engine.v1"
    );
    gts::GtsId::try_new(ADMISSION_ENGINE_INSTANCE_ID).unwrap();
    let spec_type = <admission_control_sdk::AdmissionEnginePluginSpecV1 as gts::GtsSchema>::TYPE_ID;
    assert!(ADMISSION_ENGINE_INSTANCE_ID.starts_with(spec_type));
    assert!(!ADMISSION_ENGINE_INSTANCE_ID.ends_with('~'));
    assert_eq!(POLICY_ENTRYPOINT, "deny");
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
