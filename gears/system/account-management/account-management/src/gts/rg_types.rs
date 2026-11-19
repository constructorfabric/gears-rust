//! AM-owned resource-group types, published with AM's manifest.
//!
//! Resource-group keeps no type definitions: these declarations are the only
//! definition of AM's user-group container and user member-handle. Their
//! identifiers are the ones AM has always used, so existing RG rows keep
//! resolving. They replace AM's `init`-time registration through
//! resource-group's bootstrap catalogue.

use resource_group_sdk::gts::{ResourceGroupTypeTraitsV1, ResourceGroupTypeV1};
use toolkit_gts::local::gts_type_schema;

use account_management_sdk::gts::{USER_GROUP_RG_TYPE_CODE, USER_RG_TYPE_CODE};

/// The AM user-group container: tenant-scoped RG groups that nest under one
/// another and admit AM users (the member handle) as members.
#[gts_type_schema(
    dir_path = "schemas",
    base = ResourceGroupTypeV1,
    type_id = gts_id!("cf.core.rg.type.v1~cf.core.am.user_group.v1~"),
    description = "Account Management user group: an RG group of AM users",
    properties = "",
    traits = ResourceGroupTypeTraitsV1::builder()
        .with_can_be_root(true)
        .with_allowed_parent_types([USER_GROUP_RG_TYPE_CODE])
        .with_allowed_membership_types([USER_RG_TYPE_CODE])
        .build()
)]
// Open metadata, as before: the registered type carried no metadata schema.
#[schemars(extend("additionalProperties" = true))]
pub struct UserGroupRgTypeV1;

/// The AM user member-handle: the `resource_type` of a user's membership in a
/// user group. AM never creates groups of this type; its placement is the one
/// AM registered before (root allowed, no parents, no members).
#[gts_type_schema(
    dir_path = "schemas",
    base = ResourceGroupTypeV1,
    type_id = gts_id!("cf.core.rg.type.v1~cf.core.am.user.v1~"),
    description = "Account Management user as a resource-group member",
    properties = "",
    traits = ResourceGroupTypeTraitsV1::builder()
        .with_can_be_root(true)
        .with_allowed_parent_types(Vec::<String>::new())
        .with_allowed_membership_types(Vec::<String>::new())
        .build()
)]
pub struct UserMemberRgTypeV1;

#[cfg(test)]
#[path = "rg_types_tests.rs"]
mod rg_types_tests;
