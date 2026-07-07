//! Unit tests for the ledger GTS permission catalog (`gts::permissions`): every
//! `(resource_type, action)` instance is registered in inventory, the expected-id
//! set matches exactly, and the catalog's distinct `resource_type`s equal
//! `crate::authz::labels::ALL` (anti-drift).

use toolkit_gts::InventoryInstance;

const PERMISSION_TYPE_ID: &str = "gts.cf.toolkit.authz.permission.v1~";
const INSTANCE_PREFIX: &str = "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.";

/// Every ledger permission instance id — one per `(resource_type, action)`
/// pair the ledger surfaces enforce.
const EXPECTED_PERMISSION_IDS: &[&str] = &[
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.entry_post.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.entry_reverse.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.entry_approve.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.entry_read.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.entry_annotate.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.entry_audit_read.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.entry_erase.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.entry_reidentify.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.ledger_provision.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.ledger_read.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.fiscal_period_close.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.payment_write.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.payment_read.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.credit_application_write.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.dispute_write.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.dispute_read.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.dual_control_policy_write.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.dual_control_policy_read.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.recognition_write.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.recognition_read.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.reconciliation_read.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.reconciliation_run.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.reconciliation_resolve.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.config_write.v1",
    "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.config_read.v1",
];

fn ledger_permission_instances() -> Vec<&'static InventoryInstance> {
    toolkit_gts::inventory::iter::<InventoryInstance>
        .into_iter()
        .filter(|e| e.instance_id.starts_with(INSTANCE_PREFIX))
        .collect()
}

#[test]
fn all_ledger_permissions_are_registered_in_inventory() {
    let entries = ledger_permission_instances();
    assert_eq!(
        entries.len(),
        EXPECTED_PERMISSION_IDS.len(),
        "expected {} ledger permission instances; found {}: {:?}",
        EXPECTED_PERMISSION_IDS.len(),
        entries.len(),
        entries.iter().map(|e| e.instance_id).collect::<Vec<_>>()
    );
    for entry in &entries {
        assert_eq!(
            entry.type_id, PERMISSION_TYPE_ID,
            "instance {} derived wrong type_id",
            entry.instance_id
        );
    }
}

#[test]
fn ledger_permission_inventory_covers_every_expected_id() {
    let actual: std::collections::BTreeSet<&str> = ledger_permission_instances()
        .iter()
        .map(|e| e.instance_id)
        .collect();
    for expected in EXPECTED_PERMISSION_IDS {
        assert!(
            actual.contains(expected),
            "missing expected permission id: {expected}; got {actual:?}"
        );
    }
    assert_eq!(
        actual.len(),
        EXPECTED_PERMISSION_IDS.len(),
        "inventory contains ledger permission ids not in the expected set"
    );
}

/// Anti-drift: the distinct `resource_type`s this catalog grants MUST equal
/// `crate::authz::labels::ALL` — the set the gear registers stub type-schemas for
/// so RBAC role-definitions can target them. Add a permission with a new label (or
/// a label to `ALL`) without the other and this fails.
#[test]
fn catalog_resource_types_match_authz_labels_all() {
    let catalog_types: std::collections::BTreeSet<String> = ledger_permission_instances()
        .iter()
        .map(|e| {
            (e.payload_fn)()["resource_type"]
                .as_str()
                .expect("AuthzPermissionV1 payload carries a resource_type string")
                .to_owned()
        })
        .collect();
    let labels_all: std::collections::BTreeSet<String> = crate::authz::labels::ALL
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(
        catalog_types, labels_all,
        "permission-catalog resource_types must equal crate::authz::labels::ALL"
    );
}
