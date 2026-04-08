// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-module-scaffold:p1
//! Resource Group Module
//!
//! This module provides GTS type and resource group management with REST API,
//! database storage, and inter-module communication via `ClientHub`.
//!
//! ## Architecture
//!
//! ### Contract Layer (`resource-group-sdk`)
//! - `ResourceGroupClient` trait
//! - Model types: `ResourceGroupType`, `CreateTypeRequest`, `UpdateTypeRequest`
//! - Error type: `ResourceGroupError`
//! - `OData` filter schemas (behind `odata` feature): `TypeFilterField`
//!
//! ### API Layer (`api`)
//! - `routes/` - Route definitions using `OperationBuilder`
//! - `handlers/` - Request handlers
//! - `dto.rs` - REST-specific DTOs and serialization
//! - `error.rs` - HTTP error mapping (domain errors -> RFC9457 Problem)
//!
//! ### Domain Layer (`domain`)
//! - `type_service.rs` - Business operations for GTS type management
//! - `error.rs` - Domain error types
//!
//! ### Infrastructure Layer (`infra`)
//! - `storage/entity/` - `SeaORM` entity definitions
//! - `storage/type_repo.rs` - Type repository (persistence)
//! - `storage/odata_mapper.rs` - `OData` filter -> `SeaORM` column mappings
//! - `storage/migrations/` - Database schema migrations
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

// === PUBLIC API (from SDK) ===
pub use resource_group_sdk::{
    CreateGroupRequest, CreateTypeRequest, PatchGroupRequest, ResourceGroup as ResourceGroupModel,
    ResourceGroupClient, ResourceGroupError, ResourceGroupType, ResourceGroupWithDepth,
    UpdateGroupRequest, UpdateTypeRequest,
};

// === MODULE DEFINITION ===
pub mod module;
pub use module::ResourceGroup;

// === INTERNAL MODULES ===
#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod infra;
