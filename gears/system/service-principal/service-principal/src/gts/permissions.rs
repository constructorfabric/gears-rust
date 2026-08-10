//! Service-principal authorization permissions catalog.
//!
//! Declares each grantable permission as an [`AuthzPermissionV1`] GTS instance via
//! [`gts_instance!`]. `types-registry::init()` aggregates them from the link-time
//! inventory at startup — no registration code in `crate::module`. `resource_type`
//! is the SDK's single source of truth; `action` mirrors `domain::authz::actions`,
//! so the catalog and the enforcement path share one source of truth.
//!
//! Instance-id layout: `gts.cf.toolkit.authz.permission.v1~cf.core.service_principal.<action>.v1`.

// The expected-id string literals below trip DE0901 (`gts_string_pattern`);
// they are legitimate catalog literals. Suppress file-wide (mirrors rms).
#![allow(unknown_lints)]
#![allow(de0901_gts_string_pattern)]

use service_principal_sdk::SERVICE_PRINCIPAL_RESOURCE_TYPE;
use toolkit_gts::{AuthzPermissionV1, gts_instance};

use crate::domain::authz::actions;

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.service_principal.create.v1"),
        resource_type: SERVICE_PRINCIPAL_RESOURCE_TYPE.to_owned(),
        action: actions::CREATE.to_owned(),
        display_name: "Create service principal".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.service_principal.read.v1"),
        resource_type: SERVICE_PRINCIPAL_RESOURCE_TYPE.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read service principals".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.service_principal.rotate_secret.v1"),
        resource_type: SERVICE_PRINCIPAL_RESOURCE_TYPE.to_owned(),
        action: actions::ROTATE_SECRET.to_owned(),
        display_name: "Rotate service-principal secret".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.service_principal.revoke.v1"),
        resource_type: SERVICE_PRINCIPAL_RESOURCE_TYPE.to_owned(),
        action: actions::REVOKE.to_owned(),
        display_name: "Revoke service principal".to_owned(),
    }
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
