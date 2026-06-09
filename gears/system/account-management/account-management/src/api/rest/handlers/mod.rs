//! REST handler functions for the Account Management gear.

pub mod common;
pub mod conversions;
pub mod me;
pub mod metadata;
pub mod tenants;
pub mod users;

pub(crate) use conversions::{
    get_child_conversion, get_own_conversion, list_child_conversions, list_own_conversions,
    patch_child_conversion, patch_own_conversion, request_child_conversion, request_own_conversion,
};
pub(crate) use me::get_me;
pub(crate) use metadata::{
    delete_metadata, get_metadata, list_metadata, resolve_metadata, upsert_metadata,
};
pub(crate) use tenants::{
    create_tenant, delete_tenant, get_tenant, list_tenant_children, suspend_tenant,
    unsuspend_tenant, update_tenant,
};
pub(crate) use users::{create_user, delete_user, list_users};
