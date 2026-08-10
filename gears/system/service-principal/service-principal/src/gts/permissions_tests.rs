//! Tests for the service-principal permission catalog ([`super`]).
//!
//! Verifies the `gts_instance!`-declared permissions land in the link-time
//! inventory and that the set exactly matches the expected ids.

// Deliberate panic on a malformed instance id in a test-only helper — a
// pinning assertion, not production code (matches the convention used by
// other `*_tests.rs` files, e.g. rbac's `canonical_mapping_tests.rs`).
#![allow(clippy::panic)]

use toolkit_gts::{InventoryInstance, gts_id};

const PERMISSION_TYPE_ID: &str = gts_id!("cf.toolkit.authz.permission.v1~");
/// This gear's instance-id namespace segment, appended after `PERMISSION_TYPE_ID`.
const SP_INSTANCE_NS: &str = "cf.core.service_principal.";

const EXPECTED_PERMISSION_IDS: &[&str] = &[
    gts_id!("cf.toolkit.authz.permission.v1~cf.core.service_principal.create.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.core.service_principal.read.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.core.service_principal.rotate_secret.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.core.service_principal.revoke.v1"),
];

fn sp_permission_instances() -> Vec<&'static InventoryInstance> {
    toolkit_gts::inventory::iter::<InventoryInstance>
        .into_iter()
        .filter(|e| {
            e.instance_id
                .strip_prefix(PERMISSION_TYPE_ID)
                .is_some_and(|seg| seg.starts_with(SP_INSTANCE_NS))
        })
        .collect()
}

#[test]
fn all_permissions_are_registered_in_inventory() {
    let entries = sp_permission_instances();
    assert_eq!(
        entries.len(),
        EXPECTED_PERMISSION_IDS.len(),
        "expected {} permission instances; found {}: {:?}",
        EXPECTED_PERMISSION_IDS.len(),
        entries.len(),
        entries.iter().map(|e| e.instance_id).collect::<Vec<_>>()
    );
    for entry in &entries {
        assert_eq!(entry.type_id, PERMISSION_TYPE_ID);
    }
}

#[test]
fn permission_inventory_covers_every_expected_id() {
    let actual: std::collections::BTreeSet<&str> = sp_permission_instances()
        .iter()
        .map(|e| e.instance_id)
        .collect();
    for expected in EXPECTED_PERMISSION_IDS {
        assert!(
            actual.contains(expected),
            "missing expected permission id: {expected}"
        );
    }
    assert_eq!(actual.len(), EXPECTED_PERMISSION_IDS.len());
}

#[test]
fn every_permission_carries_the_sdk_resource_type() {
    for entry in sp_permission_instances() {
        assert_eq!(
            (entry.payload_fn)()["resource_type"].as_str(),
            Some(service_principal_sdk::SERVICE_PRINCIPAL_RESOURCE_TYPE),
        );
    }
}

/// Guards the trap that makes a catalogued permission un-grantable: the PDP
/// canonicalizes the *request-side* action (`get`/`list` → `read`) before querying
/// RBAC, but RBAC matches grant operations by exact string equality with no
/// grant-side canonicalization. A catalog entry naming an alias therefore tells a
/// role-builder to author `operation: "list"`, which validates, persists, and
/// silently authorizes nothing. Only the canonical verb may be published.
#[test]
fn no_permission_publishes_a_canonicalization_alias() {
    /// Request-side aliases the PDP rewrites before consulting RBAC. Kept in
    /// lock-step with the resolver plugin's `canonicalize_operation`.
    const READ_ALIASES: &[&str] = &["get", "list"];
    for entry in sp_permission_instances() {
        let payload = (entry.payload_fn)();
        let action = payload["action"].as_str().unwrap_or_default();
        assert!(
            !READ_ALIASES.contains(&action),
            "permission `{}` publishes `{action}`, which the PDP rewrites to \
             `read` before querying RBAC — a grant naming it can never match. \
             Publish the canonical verb instead.",
            entry.instance_id
        );
    }
}

/// Anti-drift: catches a copy-paste slip where an entry's `action` field
/// doesn't match the verb encoded in its own instance id (e.g. the
/// `rotate_secret` entry accidentally carrying `action: actions::REVOKE`).
/// Derives the expected action from the id suffix
/// (`…service_principal.<action>.v1`) and compares it against the
/// catalogued `AuthzPermissionV1::action` payload field.
#[test]
fn every_permission_action_matches_its_instance_id_verb() {
    for entry in sp_permission_instances() {
        let expected_action = entry
            .instance_id
            .strip_prefix(PERMISSION_TYPE_ID)
            .and_then(|seg| seg.strip_prefix(SP_INSTANCE_NS))
            .and_then(|rest| rest.strip_suffix(".v1"))
            .unwrap_or_else(|| {
                panic!(
                    "instance id `{}` does not match the expected \
                     `{PERMISSION_TYPE_ID}{SP_INSTANCE_NS}<action>.v1` shape",
                    entry.instance_id
                )
            });
        assert_eq!(
            (entry.payload_fn)()["action"].as_str(),
            Some(expected_action),
            "permission `{}` carries an action that does not match the verb \
             encoded in its own instance id",
            entry.instance_id
        );
    }
}
