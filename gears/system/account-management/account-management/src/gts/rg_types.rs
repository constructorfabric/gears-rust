//! AM-owned resource-group types, published with AM's manifest.
//!
//! Resource-group keeps no type definitions: these declarations are the only
//! definition of AM's user-group container and user member-handle. Their
//! identifiers are the ones AM has always used, so existing RG rows keep
//! resolving. They replace AM's `init`-time registration through
//! resource-group's bootstrap catalogue.

use resource_group_sdk::gts::{ResourceGroupMembershipV1, ResourceGroupTypeV1};
use toolkit_gts::local::gts_type_schema;

use account_management_sdk::gts::{USER_GROUP_RG_TYPE_CODE, USER_RG_TYPE_CODE};

const USER_MEMBERSHIP_SCHEMA: &str = <UserGroupMembershipV1 as gts::GtsSchema>::TYPE_ID;

fn user_group_parent(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "anyOf": [
            {"type": "null"},
            {"type": "string", "x-gts-type-ref": USER_GROUP_RG_TYPE_CODE}
        ]
    })
}

/// The AM user-group container allows root placement and parent types in the AM user-group family.
#[gts_type_schema(
    dir_path = "schemas",
    base = ResourceGroupTypeV1,
    type_id = gts_id!("cf.core.rg.type.v1~cf.core.am.user_group.v1~"),
    description = "Account Management user group: an RG group of AM users",
    properties = "",
    overrides(ResourceGroupTypeV1 = {
        #[schemars(schema_with = "user_group_parent")]
        parent_type: String,
        #[schemars(extend("x-gts-type-ref" = resource_group_sdk::gts::MEMBERSHIP_TYPE_V1, "const" = USER_MEMBERSHIP_SCHEMA))]
        membership_type: String,
    })
)]
pub struct UserGroupRgTypeV1;

fn user_group_member_type(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "anyOf": [
            {"type": "string", "x-gts-type-ref": USER_RG_TYPE_CODE}
        ]
    })
}

/// Allowed resource types for membership in AM user groups.
#[gts_type_schema(
    dir_path = "schemas",
    base = ResourceGroupMembershipV1,
    type_id = gts_id!("cf.core.rg.membership.v1~cf.core.am.user_group_member.v1~"),
    description = "Allowed resource types for Account Management user groups",
    properties = "",
    overrides(ResourceGroupMembershipV1 = {
        #[schemars(schema_with = "user_group_member_type")]
        resource_type: String,
    })
)]
pub struct UserGroupMembershipV1;

/// GTS type handle for AM user resources. AM uses the identifier on links
/// and does not instantiate it as a resource group.
#[gts_type_schema(
    dir_path = "schemas",
    base = ResourceGroupTypeV1,
    type_id = gts_id!("cf.core.rg.type.v1~cf.core.am.user.v1~"),
    description = "Account Management user as a resource-group member",
    properties = "",
    gts_abstract = true
)]
pub struct UserMemberRgTypeV1;

#[cfg(test)]
#[path = "rg_types_tests.rs"]
mod rg_types_tests;
