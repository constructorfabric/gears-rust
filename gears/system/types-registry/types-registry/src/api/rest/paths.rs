//! Shared REST path constants.

/// In-memory contract path, retired with its repository at T26.
pub const V1: &str = "/types-registry/v1";

/// Interim database API path; T27 promotes it to [`V1`] after retiring the in-memory API.
pub const V2: &str = "/types-registry/v2";
