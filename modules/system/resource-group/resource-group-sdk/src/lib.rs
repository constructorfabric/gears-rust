// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-module-scaffold:p1
//! Resource Group SDK
//!
//! This crate provides the public API for the `resource-group` module:
//! - `ResourceGroupClient` trait
//! - Model types for GTS types, groups, memberships
//! - Error type (`ResourceGroupError`)
//! - `OData` filter field definitions (behind `odata` feature)

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod api;
pub mod error;
pub mod gts;
pub mod models;

// OData filter field definitions (feature-gated)
#[cfg(feature = "odata")]
pub mod odata;

// Re-export main types at crate root for convenience
pub use api::{ResourceGroupClient, ResourceGroupReadHierarchy};
pub use error::ResourceGroupError;
pub use gts::TENANT_RG_TYPE_PATH;
pub use models::{
    CreateGroupRequest, CreateTypeRequest, GroupHierarchy, GroupHierarchyWithDepth, GtsTypePath,
    ResourceGroup, ResourceGroupMembership, ResourceGroupType, ResourceGroupWithDepth,
    UpdateGroupRequest, UpdateTypeRequest,
};
