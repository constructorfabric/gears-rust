//! The permission catalog holds one instance per resource type and action
//! Construct enforces, and nothing else. The expected values are literals, so
//! a renamed resource type or action shows here as a change to what a
//! deployment grants.

use toolkit_gts::{InventoryInstance, gts_id};

const PERMISSION_TYPE_ID: &str = gts_id!("cf.toolkit.authz.permission.v1~");

/// Each Construct permission as (id suffix, resource type, action), sorted
/// by id.
fn construct_permissions() -> Vec<(String, String, String)> {
    let mut permissions: Vec<(String, String, String)> =
        toolkit_gts::inventory::iter::<InventoryInstance>
            .into_iter()
            .filter_map(|entry| {
                let suffix = entry.instance_id.strip_prefix(PERMISSION_TYPE_ID)?;
                suffix.starts_with("cf.construct.").then(|| {
                    let payload = (entry.payload_fn)();
                    let field = |name: &str| {
                        payload[name]
                            .as_str()
                            .unwrap_or_else(|| panic!("{suffix} has no {name}"))
                            .to_owned()
                    };
                    (suffix.to_owned(), field("resource_type"), field("action"))
                })
            })
            .collect();
    permissions.sort();
    permissions
}

#[test]
fn every_enforced_permission_is_declared_once_on_its_resource_and_action() {
    let expected = [
        (
            "cf.construct._.facts_manage.v1",
            "gts.cf.construct.subject.facts.v1~",
            "manage",
        ),
        (
            "cf.construct._.profile_read.v1",
            "gts.cf.construct.subject.profile.v1~",
            "read",
        ),
        (
            "cf.construct._.record_send.v1",
            "gts.cf.connectors.core.record.v1~",
            "send",
        ),
        (
            "cf.construct._.subject_settings_get.v1",
            "gts.cf.construct.subject.settings.v1~",
            "get",
        ),
        (
            "cf.construct._.subject_settings_update.v1",
            "gts.cf.construct.subject.settings.v1~",
            "update",
        ),
    ]
    .map(|(id, resource, action)| (id.to_owned(), resource.to_owned(), action.to_owned()));

    assert_eq!(construct_permissions(), expected);
}
