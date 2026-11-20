//! Tests for the gear's published permission catalogue (`gts::permissions`).
//!
//! The catalogue is the list a role is composed from, so it has to be exactly
//! the `Permission` values the handlers ask the policy decision point about: a
//! permission missing here cannot be granted, and an entry that no handler can
//! ask for grants nothing. The handlers can only pass a `Permission`, so these
//! tests pin the catalogue to that type.

use std::collections::BTreeSet;

use toolkit_gts::{InventoryInstance, gts_id};

use crate::api::authz::Permission;

const PERMISSION_TYPE_ID: &str = gts_id!("cf.toolkit.authz.permission.v1~");
/// The namespace the gear's permission instances and resource types share.
const SETTINGS_NAMESPACE: &str = "cf.core.settings.";

fn settings_permissions() -> Vec<&'static InventoryInstance> {
    toolkit_gts::inventory::iter::<InventoryInstance>
        .into_iter()
        .filter(|e| {
            e.instance_id.starts_with(PERMISSION_TYPE_ID)
                && e.instance_id[PERMISSION_TYPE_ID.len()..].starts_with(SETTINGS_NAMESPACE)
        })
        .collect()
}

fn pair_of(entry: &InventoryInstance) -> (String, String) {
    let payload = (entry.payload_fn)();
    (
        payload["resource_type"]
            .as_str()
            .expect("a permission carries a resource_type")
            .to_owned(),
        payload["action"]
            .as_str()
            .expect("a permission carries an action")
            .to_owned(),
    )
}

fn pair_of_permission(permission: Permission) -> (String, String) {
    (
        permission.resource().name().to_owned(),
        permission.action().to_owned(),
    )
}

/// The instance id a permission is published under: the resource's slug and
/// the action, in the layout the module documents.
fn expected_instance_id(permission: Permission) -> String {
    let resource_type = permission.resource();
    let resource = resource_type.name();
    let slug = resource
        .split_once(SETTINGS_NAMESPACE)
        .and_then(|(_, rest)| rest.strip_suffix(".v1~"))
        .unwrap_or_else(|| panic!("`{resource}` is not a settings resource type"));
    format!(
        "{PERMISSION_TYPE_ID}{SETTINGS_NAMESPACE}{slug}_{}.v1",
        permission.action()
    )
}

#[test]
fn every_permission_has_exactly_one_catalogue_entry() {
    let entries = settings_permissions();
    for permission in Permission::ALL {
        let matching = entries
            .iter()
            .filter(|e| pair_of(e) == pair_of_permission(*permission))
            .count();
        assert_eq!(matching, 1, "{permission:?} is published {matching} times");
    }
}

#[test]
fn every_catalogue_entry_is_a_permission() {
    let known: BTreeSet<(String, String)> = Permission::ALL
        .iter()
        .map(|p| pair_of_permission(*p))
        .collect();
    for entry in settings_permissions() {
        assert!(
            known.contains(&pair_of(entry)),
            "{} publishes a pair no handler can ask for",
            entry.instance_id
        );
    }
    assert_eq!(settings_permissions().len(), Permission::ALL.len());
}

/// Each entry carries the pair of the permission its own id names. The set of
/// ids and the set of pairs are pinned separately above, so a swapped `id:` (an
/// entry labelled `value_read` that grants `write`) passes both; only a
/// comparison inside one entry catches it.
#[test]
fn an_instance_id_agrees_with_its_payload() {
    let entries = settings_permissions();
    for permission in Permission::ALL {
        let id = expected_instance_id(*permission);
        let entry = entries
            .iter()
            .find(|e| e.instance_id == id)
            .unwrap_or_else(|| panic!("{permission:?} is not published as `{id}`"));
        assert_eq!(
            pair_of(entry),
            pair_of_permission(*permission),
            "`{id}` carries the pair of another permission"
        );
        let payload = (entry.payload_fn)();
        assert_eq!(
            payload.get("id").and_then(serde_json::Value::as_str),
            Some(entry.instance_id),
            "`{id}`: the payload carries its own id"
        );
    }
}

#[test]
fn each_entry_is_an_instance_of_the_permission_type_once() {
    let entries = settings_permissions();
    let ids: BTreeSet<&str> = entries.iter().map(|e| e.instance_id).collect();
    assert_eq!(ids.len(), entries.len(), "an instance id repeats");
    for entry in entries {
        assert_eq!(entry.type_id, PERMISSION_TYPE_ID, "{}", entry.instance_id);
    }
}

#[test]
fn every_permission_has_a_display_name() {
    for entry in settings_permissions() {
        let name = (entry.payload_fn)()["display_name"]
            .as_str()
            .expect("a permission carries a display_name")
            .to_owned();
        assert!(!name.trim().is_empty(), "{}", entry.instance_id);
    }
}

/// The gear publishes this crate's collector (`gts_publication(...)`): every
/// permission has to be in it, and nothing else.
#[test]
fn the_crate_collector_holds_exactly_the_catalogue() {
    let declared: BTreeSet<String> = crate::gts_declarations()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let catalogue: BTreeSet<String> = settings_permissions()
        .iter()
        .map(|e| e.instance_id.to_owned())
        .collect();
    assert_eq!(declared, catalogue);
    assert_eq!(declared.len(), Permission::ALL.len());
}
