//! Tenant Resolver Plugin — in-process query facade over AM-owned
//! `tenants` and `tenant_closure`.
//!
//! Co-located with AM (rather than shipped as a standalone crate) so
//! the plugin can rely on AM-writer invariants directly: transactional
//! `(tenants, tenant_closure)` maintenance, self-row existence, barrier
//! materialization over `(ancestor, descendant]`, and provisioning
//! lifecycle semantics. See `docs/tr-plugin/DESIGN.md` §1.1 for the
//! co-location rationale.
//!
//! The plugin owns no schema, no migration, no cache, and no REST
//! surface. It implements the `TenantResolverPluginClient` SDK trait
//! and registers a scoped binding in `ClientHub` from
//! [`crate::AccountManagementGear::init`]. Every SDK call resolves
//! to one or two indexed reads against AM-owned storage; tenant-type
//! reverse-hydration is delegated to the registry client: one read per
//! answer until the SDK's client cache (types-registry T36) lands.

mod error_map;
mod plugin_impl;
mod projection;
mod queries;

pub use plugin_impl::PluginImpl;

#[cfg(test)]
mod tests;
