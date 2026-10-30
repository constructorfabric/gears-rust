//! Shared REST path constants.

/// In-memory contract path, retired with its repository at T38.
pub const V1: &str = "/types-registry/v1";

/// Interim tenant read path; T38 promotes it to [`V1`] while retiring the in-memory API.
/// The SDK's, so the routes and the clients cannot drift apart.
pub const V2: &str = types_registry_sdk::TENANT_BASE_PATH;

/// Interim platform API: every operation requires a validated internal token (D17).
/// T38 promotes it to `/types-registry/v1/platform` after retiring legacy v1.
pub const PLATFORM: &str = types_registry_sdk::PLATFORM_BASE_PATH;
