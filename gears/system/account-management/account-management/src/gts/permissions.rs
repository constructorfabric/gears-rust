//! Account-management authorization permissions catalog.
//!
//! Declares every AM-grantable permission as an [`AuthzPermissionV1`] GTS
//! instance via [`gts_instance!`]. Each invocation submits an
//! [`InventoryInstance`]; `types-registry::init()` aggregates them at boot.
//! `resource_type` values come from the `account_management_sdk` resource-type
//! consts; `action` values come from each service's `pep::actions` — the same
//! constants the `PolicyEnforcer` gate passes — so the catalog cannot drift
//! from what the REST surface actually enforces.
//!
//! Instance id layout: `gts.cf.toolkit.authz.permission.v1~cf.core.am.<seg>.v1`.
//!
//! [`AuthzPermissionV1`]: toolkit_gts::AuthzPermissionV1
//! [`InventoryInstance`]: toolkit_gts::InventoryInstance
//! [`gts_instance!`]: toolkit_gts::gts_instance

use account_management_sdk::{
    CONVERSION_REQUEST_RESOURCE_TYPE, SERVICE_ACCOUNT_RESOURCE_TYPE, TENANT_METADATA_RESOURCE_TYPE,
    TENANT_RESOURCE_TYPE, USER_RESOURCE_TYPE,
};
use toolkit_gts::{AuthzPermissionV1, gts_instance};

use crate::domain::conversion::service::pep::actions as conversion_actions;
use crate::domain::metadata::service::pep::actions as metadata_actions;
use crate::domain::service_account::service::pep::actions as service_account_actions;
use crate::domain::tenant::service::pep::actions as tenant_actions;
use crate::domain::user::service::pep::actions as user_actions;

// ---- tenant (gts.cf.core.am.tenant.v1~) -----------------------------------

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_create.v1"),
        resource_type: TENANT_RESOURCE_TYPE.to_owned(),
        action: tenant_actions::CREATE.to_owned(),
        display_name: "Create tenant".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_read.v1"),
        resource_type: TENANT_RESOURCE_TYPE.to_owned(),
        action: tenant_actions::READ.to_owned(),
        display_name: "Read tenant".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_update.v1"),
        resource_type: TENANT_RESOURCE_TYPE.to_owned(),
        action: tenant_actions::UPDATE.to_owned(),
        display_name: "Update tenant".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_delete.v1"),
        resource_type: TENANT_RESOURCE_TYPE.to_owned(),
        action: tenant_actions::DELETE.to_owned(),
        display_name: "Delete tenant".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_list_children.v1"),
        resource_type: TENANT_RESOURCE_TYPE.to_owned(),
        action: tenant_actions::LIST_CHILDREN.to_owned(),
        display_name: "List child tenants".to_owned(),
    }
}

// ---- tenant_metadata (gts.cf.core.am.tenant_metadata.v1~) ------------------

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_metadata_read.v1"),
        resource_type: TENANT_METADATA_RESOURCE_TYPE.to_owned(),
        action: metadata_actions::READ.to_owned(),
        display_name: "Read tenant metadata".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_metadata_write.v1"),
        resource_type: TENANT_METADATA_RESOURCE_TYPE.to_owned(),
        action: metadata_actions::WRITE.to_owned(),
        display_name: "Write tenant metadata".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_metadata_list.v1"),
        resource_type: TENANT_METADATA_RESOURCE_TYPE.to_owned(),
        action: metadata_actions::LIST.to_owned(),
        display_name: "List tenant metadata".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_metadata_delete.v1"),
        resource_type: TENANT_METADATA_RESOURCE_TYPE.to_owned(),
        action: metadata_actions::DELETE.to_owned(),
        display_name: "Delete tenant metadata".to_owned(),
    }
}

// ---- conversion_request (gts.cf.core.am.conversion_request.v1~) ------------

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.conversion_request.v1"),
        resource_type: CONVERSION_REQUEST_RESOURCE_TYPE.to_owned(),
        action: conversion_actions::REQUEST.to_owned(),
        display_name: "Request tenant conversion".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.conversion_cancel.v1"),
        resource_type: CONVERSION_REQUEST_RESOURCE_TYPE.to_owned(),
        action: conversion_actions::CANCEL.to_owned(),
        display_name: "Cancel conversion request".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.conversion_reject.v1"),
        resource_type: CONVERSION_REQUEST_RESOURCE_TYPE.to_owned(),
        action: conversion_actions::REJECT.to_owned(),
        display_name: "Reject conversion request".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.conversion_approve.v1"),
        resource_type: CONVERSION_REQUEST_RESOURCE_TYPE.to_owned(),
        action: conversion_actions::APPROVE.to_owned(),
        display_name: "Approve conversion request".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.conversion_list_own.v1"),
        resource_type: CONVERSION_REQUEST_RESOURCE_TYPE.to_owned(),
        action: conversion_actions::LIST_OWN.to_owned(),
        display_name: "List own conversion requests".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.conversion_list_inbound.v1"),
        resource_type: CONVERSION_REQUEST_RESOURCE_TYPE.to_owned(),
        action: conversion_actions::LIST_INBOUND.to_owned(),
        display_name: "List inbound conversion requests".to_owned(),
    }
}

// ---- user (gts.cf.core.am.user.v1~) ---------------------------------------

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.user_create.v1"),
        resource_type: USER_RESOURCE_TYPE.to_owned(),
        action: user_actions::CREATE.to_owned(),
        display_name: "Create user".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.user_list.v1"),
        resource_type: USER_RESOURCE_TYPE.to_owned(),
        action: user_actions::LIST.to_owned(),
        display_name: "List users".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.user_delete.v1"),
        resource_type: USER_RESOURCE_TYPE.to_owned(),
        action: user_actions::DELETE.to_owned(),
        display_name: "Delete user".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.user_update.v1"),
        resource_type: USER_RESOURCE_TYPE.to_owned(),
        action: user_actions::UPDATE.to_owned(),
        display_name: "Update user".to_owned(),
    }
}

// ---- service_account (gts.cf.core.am.service_account.v1~) --------------
// @cpt-begin:cpt-cf-account-management-dod-service-accounts-independent-permissions:p1:inst-dod-sa-independent-permissions-catalog
//
// Per-verb rather than a read/write/delete triad, and a separate
// resource type from `user` on purpose: minting a machine credential is
// not something a "manage users" grant should imply, and re-keying an
// existing account (`rotate_secret`) is grantable without the power to
// create new ones.

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.service_account_create.v1"),
        resource_type: SERVICE_ACCOUNT_RESOURCE_TYPE.to_owned(),
        action: service_account_actions::CREATE.to_owned(),
        display_name: "Create service account".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.service_account_list.v1"),
        resource_type: SERVICE_ACCOUNT_RESOURCE_TYPE.to_owned(),
        action: service_account_actions::LIST.to_owned(),
        display_name: "List service accounts".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.service_account_rotate_secret.v1"),
        resource_type: SERVICE_ACCOUNT_RESOURCE_TYPE.to_owned(),
        action: service_account_actions::ROTATE_SECRET.to_owned(),
        display_name: "Rotate service-account secret".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.service_account_revoke.v1"),
        resource_type: SERVICE_ACCOUNT_RESOURCE_TYPE.to_owned(),
        action: service_account_actions::REVOKE.to_owned(),
        display_name: "Revoke service account".to_owned(),
    }
}
// @cpt-end:cpt-cf-account-management-dod-service-accounts-independent-permissions:p1:inst-dod-sa-independent-permissions-catalog

#[cfg(test)]
mod tests {
    use toolkit_gts::{InventoryInstance, gts_id};

    use super::{
        CONVERSION_REQUEST_RESOURCE_TYPE, SERVICE_ACCOUNT_RESOURCE_TYPE,
        TENANT_METADATA_RESOURCE_TYPE, TENANT_RESOURCE_TYPE, USER_RESOURCE_TYPE,
        conversion_actions, metadata_actions, service_account_actions, tenant_actions,
        user_actions,
    };

    const PERMISSION_TYPE_ID: &str = gts_id!("cf.toolkit.authz.permission.v1~");
    /// AM's instance-id namespace segment, appended after
    /// [`PERMISSION_TYPE_ID`]. Kept as a bare fragment (not a `gts.`-prefixed
    /// literal) so it is composed with the valid type id at the filter site
    /// rather than spelled as a malformed standalone GTS string.
    const AM_INSTANCE_NS: &str = "cf.core.am.";

    /// One per `(resource_type, action)` the AM REST/PEP surface enforces.
    const EXPECTED_PERMISSION_IDS: &[&str] = &[
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_create.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_read.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_update.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_delete.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_list_children.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_metadata_read.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_metadata_write.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_metadata_list.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.tenant_metadata_delete.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.conversion_request.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.conversion_cancel.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.conversion_reject.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.conversion_approve.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.conversion_list_own.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.conversion_list_inbound.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.user_create.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.user_list.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.user_delete.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.user_update.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.service_account_create.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.service_account_list.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.service_account_rotate_secret.v1"),
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.am.service_account_revoke.v1"),
    ];

    fn am_permission_instances() -> Vec<&'static InventoryInstance> {
        inventory::iter::<InventoryInstance>
            .into_iter()
            .filter(|e| {
                e.instance_id
                    .strip_prefix(PERMISSION_TYPE_ID)
                    .is_some_and(|seg| seg.starts_with(AM_INSTANCE_NS))
            })
            .collect()
    }

    #[test]
    fn all_am_permissions_registered_in_inventory() {
        let entries = am_permission_instances();
        assert_eq!(
            entries.len(),
            EXPECTED_PERMISSION_IDS.len(),
            "expected {} AM permission instances; found {}: {:?}",
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

    /// Every `(resource_type, action)` pair an AM PEP gate can enforce,
    /// assembled from the per-service action vocabularies rather than
    /// restated by hand — the vocabularies are the same constants the
    /// `PolicyEnforcer` call sites pass.
    fn enforced_permission_pairs() -> std::collections::BTreeSet<(&'static str, &'static str)> {
        let vocabularies: [(&'static str, &'static [&'static str]); 5] = [
            (TENANT_RESOURCE_TYPE, tenant_actions::ALL),
            (TENANT_METADATA_RESOURCE_TYPE, metadata_actions::ALL),
            (CONVERSION_REQUEST_RESOURCE_TYPE, conversion_actions::ALL),
            (USER_RESOURCE_TYPE, user_actions::ALL),
            (SERVICE_ACCOUNT_RESOURCE_TYPE, service_account_actions::ALL),
        ];
        vocabularies
            .into_iter()
            .flat_map(|(resource_type, actions)| {
                actions.iter().map(move |action| (resource_type, *action))
            })
            .collect()
    }

    /// Reads the `(resource_type, action)` pair back out of each registered
    /// instance's serialized payload.
    fn declared_permission_pairs() -> std::collections::BTreeSet<(String, String)> {
        am_permission_instances()
            .iter()
            .map(|entry| {
                let payload = (entry.payload_fn)();
                let field = |name: &str| {
                    payload
                        .get(name)
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_else(|| {
                            panic!("instance {} lacks a string `{name}`", entry.instance_id)
                        })
                        .to_owned()
                };
                (field("resource_type"), field("action"))
            })
            .collect()
    }

    /// Catalog-vs-enforcement drift guard.
    ///
    /// [`EXPECTED_PERMISSION_IDS`] pins instance *ids*, which only catches a
    /// renamed or dropped instance; it cannot notice a service that starts
    /// enforcing a brand-new action nobody declared as grantable. Comparing
    /// the semantic `(resource_type, action)` pairs closes that gap in both
    /// directions: an enforced-but-undeclared action leaves the permission
    /// ungrantable to least-privilege roles, and a declared-but-unenforced
    /// one advertises a grant that gates nothing.
    #[test]
    fn am_permission_catalog_matches_enforced_action_vocabulary() {
        let declared = declared_permission_pairs();
        let enforced: std::collections::BTreeSet<(String, String)> = enforced_permission_pairs()
            .into_iter()
            .map(|(resource_type, action)| (resource_type.to_owned(), action.to_owned()))
            .collect();

        let undeclared: Vec<_> = enforced.difference(&declared).collect();
        let unenforced: Vec<_> = declared.difference(&enforced).collect();
        assert!(
            undeclared.is_empty() && unenforced.is_empty(),
            "AM permission catalog drifted from the enforced action vocabulary; \
             enforced but not declared as an AuthzPermissionV1: {undeclared:?}; \
             declared but not in any pep::actions::ALL: {unenforced:?}"
        );
    }

    #[test]
    fn am_permission_inventory_covers_every_expected_id() {
        let actual: std::collections::BTreeSet<&str> = am_permission_instances()
            .iter()
            .map(|e| e.instance_id)
            .collect();
        for expected in EXPECTED_PERMISSION_IDS {
            assert!(
                actual.contains(expected),
                "missing permission id: {expected}"
            );
        }
        assert_eq!(actual.len(), EXPECTED_PERMISSION_IDS.len());
    }
}
