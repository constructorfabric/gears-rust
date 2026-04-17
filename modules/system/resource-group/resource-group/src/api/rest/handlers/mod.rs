// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-module-scaffold:p1
use crate::api::rest::dto::{
    CreateGroupDto, CreateTypeDto, GroupDto, GroupWithDepthDto, MembershipDto, PatchGroupDto,
    TypeDto, UpdateGroupDto, UpdateTypeDto,
};

use modkit_security::SecurityContext;
use tracing::{debug, info};

mod groups;
mod memberships;
mod types;

pub(crate) use groups::create_group;
pub(crate) use groups::delete_group;
pub(crate) use groups::get_group;
pub(crate) use groups::get_group_ancestors;
pub(crate) use groups::get_group_descendants;
pub(crate) use groups::list_groups;
pub(crate) use groups::patch_group;
pub(crate) use groups::update_group;
pub(crate) use memberships::add_membership;
pub(crate) use memberships::list_memberships;
pub(crate) use memberships::remove_membership;
pub(crate) use types::create_type;
pub(crate) use types::delete_type;
pub(crate) use types::get_type;
pub(crate) use types::list_types;
pub(crate) use types::update_type;
