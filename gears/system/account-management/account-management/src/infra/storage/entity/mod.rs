//! `SeaORM` entity definitions for AM-owned tables.
//!
//! Each gear mirrors exactly one table declared in the migration set.
//! Entities contain no domain logic — they are `sea_orm` value types used
//! by the repository implementation layer.

pub mod am_leases;
pub mod conversion_requests;
pub mod pending_user_deprovisions;
pub mod tenant_closure;
pub mod tenant_idp_metadata;
pub mod tenant_metadata;
pub mod tenants;
