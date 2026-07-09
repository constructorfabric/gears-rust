// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-gear-scaffold:p1
//! Resource Group SDK
//!
//! This crate provides the public API for the `resource-group` gear:
//! - `ResourceGroupClient` / `ResourceGroupReadHierarchy` traits
//!   (boundary returns [`toolkit_canonical_errors::CanonicalError`] per
//!   [ADR 0005][adr])
//! - Model types for GTS types, groups, memberships
//! - [`ResourceGroupError`] — opt-in `From<CanonicalError>` projection
//!   (see [`error`]) plus its co-located wire vocabulary ([`field`],
//!   [`precondition`], [`reason`], [`gts`])
//! - `OData` filter field definitions (behind `odata` feature)
//!
//! [adr]: https://github.com/constructorfabric/gears-rust/blob/main/docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod api;
pub mod error;
pub mod field;
pub mod gts;
pub mod models;
pub mod precondition;
pub mod reason;

// OData filter field definitions (feature-gated)
#[cfg(feature = "odata")]
pub mod odata;

// Re-export main types at crate root for convenience
pub use api::{ResourceGroupClient, ResourceGroupReadHierarchy};
pub use error::ResourceGroupError;
pub use gts::{GROUP_RESOURCE_TYPE, TENANT_RG_TYPE_PATH};
pub use models::{
    CreateGroupRequest, CreateTypeRequest, GroupHierarchy, GroupHierarchyWithDepth, GtsTypePath,
    ResourceGroup, ResourceGroupMembership, ResourceGroupType, ResourceGroupWithDepth,
    UpdateGroupRequest, UpdateTypeRequest,
};
