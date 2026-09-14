//! Tenant hierarchy domain gear.
//!
//! Owns the tenant entity's core model, repository contract, closure-table
//! invariants, retention pipeline types, the `TenantService` saga
//! orchestrator, hard-delete cascade hooks, and the `ResourceOwnership`
//! checker abstraction. Public input/output shapes
//! ([`account_management_sdk::CreateTenantRequest`],
//! [`account_management_sdk::UpdateTenantRequest`],
//! [`account_management_sdk::TenantInfoQuery`] /
//! [`account_management_sdk::TenantInfoFilterField`],
//! [`account_management_sdk::Tenant`]) live on the SDK; the listing
//! envelope is [`toolkit_odata::Page<account_management_sdk::Tenant>`].

pub mod closure;
pub mod context;
pub mod hierarchy_read_port;
pub mod hooks;
pub mod integrity;
pub mod model;
pub mod repo;
pub mod resolve;
pub mod resource_checker;
pub mod retention;
pub mod service;

#[cfg(test)]
pub(crate) mod test_support;

pub use closure::{ClosureRow, build_activation_rows};
pub use context::TenantContext;
pub use hierarchy_read_port::{BarrierMode, StatusFilter, TenantHierarchyReadPort};
pub use model::{ChildCountFilter, NewTenant, TenantModel, TenantStatus};
pub use repo::TenantRepo;
pub use retention::{
    HardDeleteOutcome, HardDeleteResult, ReaperResult, TenantProvisioningRow, TenantRetentionRow,
    is_due, order_batch_leaf_first,
};
