//! GTS resource type identifiers for the `users_info` example gear.
//!
//! Single source of truth for the resource-type strings used in:
//!
//! * `resource_type` field on the canonical-error envelope produced
//!   when a `users_info` domain failure converts to
//!   [`toolkit_canonical_errors::CanonicalError`] at the gear boundary
//!   (see `users_info::api::rest::error`).
//! * Future PEP authorization checks and cross-gear event consumers
//!   that pattern-match on resource type.
//!
//! Mirrors the `gts` gear layout used by `account-management-sdk` and
//! `resource-group-sdk`.
//!
//! # Note on `#[resource_error]` macro arguments
//!
//! The `toolkit_canonical_errors::resource_error` proc-macro takes a
//! literal string at expansion time and cannot resolve constants — the
//! impl-crate sites that call the macro therefore duplicate these
//! literals. A test in the impl crate asserts the strings stay in sync.

use toolkit_gts::gts_id;
/// `User` resource. Used as the `resource_type` field on canonical
/// errors emitted from user-scoped operations (e.g. `user {id} not
/// found` → 404).
pub const USER_RESOURCE_TYPE: &str = gts_id!("cf.example1.users.user.v1~");

/// `City` resource. Used as the `resource_type` field on canonical
/// errors emitted from city-scoped operations.
pub const CITY_RESOURCE_TYPE: &str = gts_id!("cf.example1.users.city.v1~");

/// `Address` resource. Used as the `resource_type` field on canonical
/// errors emitted from address-scoped operations.
pub const ADDRESS_RESOURCE_TYPE: &str = gts_id!("cf.example1.users.address.v1~");
