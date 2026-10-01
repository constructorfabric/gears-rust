//! `SeaORM` entities - one file per table, named after the table's local
//! name (`policy_engine__<file>`).
//!
//! Every entity derives `Scopable`. Policy content is scoped by the owning
//! tenant (`owner_tenant_id`, with `id` as the resource). `smallint` columns
//! carry the integer codes defined next to their entity; the domain maps them
//! to its own enums.

pub mod assignment;
pub mod bundle;
pub mod bundle_version;
pub mod document;
