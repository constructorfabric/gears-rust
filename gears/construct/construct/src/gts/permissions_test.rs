//! The permission catalog holds one instance per resource type and action
//! Construct enforces, and nothing else.

use toolkit_gts::{InventoryInstance, gts_id};

const PERMISSION_TYPE_ID: &str = gts_id!("cf.toolkit.authz.permission.v1~");

fn construct_permissions() -> Vec<String> {
    let mut ids: Vec<String> = toolkit_gts::inventory::iter::<InventoryInstance>
        .into_iter()
        .filter_map(|entry| entry.instance_id.strip_prefix(PERMISSION_TYPE_ID))
        .filter(|suffix| suffix.starts_with("cf.construct."))
        .map(str::to_owned)
        .collect();
    ids.sort();
    ids
}

#[test]
fn every_enforced_permission_is_declared_once() {
    assert_eq!(
        construct_permissions(),
        [
            "cf.construct._.facts_manage.v1",
            "cf.construct._.profile_read.v1",
            "cf.construct._.record_send.v1",
            "cf.construct._.subject_settings_get.v1",
            "cf.construct._.subject_settings_update.v1",
        ]
    );
}
