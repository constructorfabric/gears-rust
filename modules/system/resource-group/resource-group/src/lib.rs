// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-module-scaffold:p1
//! Resource Group Module
//!
//! This module provides GTS type and resource group management with REST API,
//! database storage, and inter-module communication via `ClientHub`.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

// === PUBLIC API (from SDK) ===
pub use resource_group_sdk::{
    CreateGroupRequest, CreateTypeRequest, PatchGroupRequest, ResourceGroup as ResourceGroupModel,
    ResourceGroupClient, ResourceGroupError, ResourceGroupType, ResourceGroupWithDepth,
    UpdateGroupRequest, UpdateTypeRequest,
};

// === INTERNAL MODULES ===
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod infra;
