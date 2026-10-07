//! Physical table and column names the scope compiler emits SQL against.
//!
//! These belong here, with the code that writes the queries, rather than in
//! `toolkit-security`: they are not part of the authorization *model*, they are
//! the storage layout two gears happen to use. The security crate defines what
//! a scope means; this module knows what it has to become in SQL.
//!
//! Both sets mirror schemas this crate cannot see -- they are created by the
//! resource-group and account-management gears, which depend on this one -- so
//! nothing here can verify the names still match. A rename on the migration
//! side is a runtime failure, not a build one, and the assertion belongs with
//! the migration that owns the table.

/// Well-known resource-group table and column names for subquery construction.
///
/// Used to translate `InGroup` / `InGroupSubtree` scope filters into SQL
/// subqueries without depending on entity types.
///
/// **Note:** These tables are canonical to the RG gear's database.
/// `resource_group_membership` is not projected to domain services.
/// `InGroup`/`InGroupSubtree` predicates are only executable within the RG gear.
pub mod rg_tables {
    /// Membership table (RG-internal, not projected to domain services).
    pub const MEMBERSHIP_TABLE: &str = "resource_group_membership";
    /// Column in membership table: the resource's external ID.
    pub const MEMBERSHIP_RESOURCE_ID: &str = "resource_id";
    /// Column in membership table: the group the resource belongs to.
    pub const MEMBERSHIP_GROUP_ID: &str = "group_id";
    /// Column in membership table: the RG-local GTS type surrogate.
    pub const MEMBERSHIP_GTS_TYPE_ID: &str = "gts_type_id";

    /// RG-local GTS type registry table.
    pub const GTS_TYPE_TABLE: &str = "gts_type";
    /// Primary key in the RG-local GTS type registry.
    pub const GTS_TYPE_ID: &str = "id";
    /// External GTS schema identifier in the RG-local type registry.
    pub const GTS_TYPE_SCHEMA_ID: &str = "schema_id";

    /// Closure table for group hierarchy.
    pub const CLOSURE_TABLE: &str = "resource_group_closure";
    /// Column in closure table: the ancestor group.
    pub const CLOSURE_ANCESTOR_ID: &str = "ancestor_id";
    /// Column in closure table: the descendant group.
    pub const CLOSURE_DESCENDANT_ID: &str = "descendant_id";
}

/// Well-known tenant-closure table and column names for subquery construction.
///
/// Used to translate `InTenantSubtree` scope filters into SQL subqueries
/// without depending on entity types.
///
/// **Note:** This table is canonical to the Account Management gear's
/// database. `InTenantSubtree` predicates are only executable in gears
/// that share the AM database (or replicate `tenant_closure` from it).
pub mod tenant_tables {
    /// Closure table for tenant hierarchy.
    pub const CLOSURE_TABLE: &str = "tenant_closure";
    /// Column in closure table: the ancestor tenant.
    pub const CLOSURE_ANCESTOR_ID: &str = "ancestor_id";
    /// Column in closure table: the descendant tenant.
    pub const CLOSURE_DESCENDANT_ID: &str = "descendant_id";
    /// Column in closure table: barrier flag.
    ///
    /// AM materializes `barrier = 1` on every closure row whose strict path
    /// `(ancestor, descendant]` crosses a self-managed tenant. Subtree
    /// queries that should stop at delegation boundaries clamp the
    /// subquery with `AND barrier = 0`.
    pub const CLOSURE_BARRIER: &str = "barrier";
    /// Column in closure table: status of the descendant tenant (SMALLINT,
    /// canonically `{1 = active, 2 = suspended, 3 = deleted}` — see
    /// `tenant_resolver_sdk::TenantStatus::as_smallint`).
    pub const CLOSURE_DESCENDANT_STATUS: &str = "descendant_status";
}
