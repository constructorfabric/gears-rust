//! `SeaORM` entities for the ADR-0009 tables (all except `oagw_plugin`).
//!
//! Every entity is tenant-scoped (`tenant_col = "tenant_id"`), so every query
//! runs under an `AccessScope::for_tenants(..)`. Root tables (`oagw_upstream`,
//! `oagw_route`) expose `id` as the resource column. Child tables are keyed by
//! their parent ID (composite with the tag, method or position where a parent
//! has several rows), carry their parent's `tenant_id`, and reference the parent
//! through a composite foreign key `(tenant_id, parent_id)`, so a child row can
//! never belong to another tenant's parent.

pub(crate) mod route;
pub(crate) mod route_grpc_match;
pub(crate) mod route_http_match;
pub(crate) mod route_method;
pub(crate) mod route_plugin;
pub(crate) mod route_tag;
pub(crate) mod upstream;
pub(crate) mod upstream_plugin;
pub(crate) mod upstream_tag;
