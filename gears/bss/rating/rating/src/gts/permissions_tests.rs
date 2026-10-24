//! The permission catalog is complete and agrees with the resource labels (anti-drift).

use toolkit_gts::{InventoryInstance, gts_id};

use crate::authz::labels;

const PERMISSION_TYPE_ID: &str = gts_id!("cf.toolkit.authz.permission.v1~");
const INSTANCE_SUFFIX_PREFIX: &str = "cf.bss.rating.";

/// One permission per `(resource type, action)` of DESIGN §4.8.
const EXPECTED_PERMISSION_IDS: &[&str] = &[
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.usage_read.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.usage_retry.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.usage_release.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.result_read.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.rerate_read.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.rerate_execute.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.evaluation_evaluate.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.finalization_policy_read.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.finalization_policy_write.v1"),
    gts_id!(
        "cf.toolkit.authz.permission.v1~cf.bss.rating.finalization_policy_accept_delay_only.v1"
    ),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.platform_finalization_policy_read.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.platform_finalization_policy_write.v1"),
    gts_id!(
        "cf.toolkit.authz.permission.v1~cf.bss.rating.platform_finalization_policy_accept_delay_only.v1"
    ),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.source_loss_resolve.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.source_loss_approve.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.retention_hold_read.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.retention_hold_place.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.retention_hold_release.v1"),
];

fn rating_permissions() -> Vec<&'static InventoryInstance> {
    toolkit_gts::inventory::iter::<InventoryInstance>
        .into_iter()
        .filter(|e| {
            e.instance_id.starts_with(PERMISSION_TYPE_ID)
                && e.instance_id[PERMISSION_TYPE_ID.len()..].starts_with(INSTANCE_SUFFIX_PREFIX)
        })
        .collect()
}

#[test]
fn every_permission_is_registered_once() {
    let mut found: Vec<&str> = rating_permissions().iter().map(|e| e.instance_id).collect();
    found.sort_unstable();
    let mut expected = EXPECTED_PERMISSION_IDS.to_vec();
    expected.sort_unstable();
    assert_eq!(found, expected);
}

#[test]
fn the_catalog_covers_every_resource_label() {
    let mut covered: Vec<String> = rating_permissions()
        .iter()
        .map(|e| {
            (e.payload_fn)()["resource_type"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    covered.sort_unstable();
    covered.dedup();
    let mut expected: Vec<String> = labels::ALL.iter().map(|l| (*l).to_owned()).collect();
    expected.sort_unstable();
    assert_eq!(covered, expected);
}

#[test]
fn every_operation_is_grantable_by_a_registered_permission() {
    let granted: Vec<(String, String)> = rating_permissions()
        .iter()
        .map(|e| {
            let payload = (e.payload_fn)();
            (
                payload["resource_type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                payload["action"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    for operation in crate::authz::Operation::ALL {
        let (resource, action) = operation.check();
        let pair = (resource.name().to_owned(), action.to_owned());
        assert!(
            granted.contains(&pair),
            "{} ({}, {}) has no permission a role can grant",
            operation.operation_id(),
            pair.0,
            pair.1
        );
    }
}
