//! AM-owned resource-group types, published with AM's manifest.
//!
//! The user-group type binds a membership schema that admits the AM user
//! resource type and its descendants.

use resource_group_sdk::gts::{ResourceGroupMembershipV1, ResourceGroupTypeV1};
use toolkit_gts::local::gts_type_schema;

use account_management_sdk::gts::{USER_GROUP_RG_TYPE_CODE, USER_RESOURCE_TYPE};

const USER_MEMBERSHIP_SCHEMA: &str = <UserGroupMembershipV1 as gts::GtsSchema>::TYPE_ID;

#[derive(schemars::JsonSchema, serde::Serialize, toolkit_gts::GtsTraitsSchema)]
struct UserGroupTraits {
    #[schemars(extend("const" = USER_MEMBERSHIP_SCHEMA))]
    membership_type: &'static str,
}

/// The AM user-group container can be a root or nest under another group of this exact type.
#[gts_type_schema(
    dir_path = "schemas",
    base = ResourceGroupTypeV1,
    type_id = gts_id!("cf.core.rg.type.v1~cf.core.am.user_group.v1~"),
    description = "Account Management user group: an RG group of AM users",
    properties = "",
    traits_schema = inline(UserGroupTraits),
    traits = UserGroupTraits { membership_type: USER_MEMBERSHIP_SCHEMA },
    overrides(ResourceGroupTypeV1 = {
        // A root carries an explicit null. The property itself remains required.
        #[schemars(extend("type" = ["string", "null"], "enum" = [null, USER_GROUP_RG_TYPE_CODE],
            "x-gts-type-ref" = resource_group_sdk::gts::TYPE_RESOURCE_TYPE))]
        parent_type: serde_json::Value,
    })
)]
pub struct UserGroupTypeV1;

/// Allowed resource types for membership in AM user groups.
#[gts_type_schema(
    dir_path = "schemas",
    base = ResourceGroupMembershipV1,
    type_id = gts_id!("cf.core.rg.membership.v1~cf.core.am.user_group_member.v1~"),
    description = "Allowed resource types for Account Management user groups",
    properties = "",
    overrides(ResourceGroupMembershipV1 = {
        #[schemars(extend("x-gts-type-ref" = USER_RESOURCE_TYPE))]
        resource_type: String,
    })
)]
pub struct UserGroupMembershipV1;

#[cfg(test)]
#[path = "rg_types_tests.rs"]
mod rg_types_tests;
