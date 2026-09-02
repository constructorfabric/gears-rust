//! Shared REST path constants.

/// Base path of the pre-database contract. `main`'s routes, unchanged, served
/// from the in-memory service until T24 deletes them with their repository.
pub const V1: &str = "/types-registry/v1";

/// Base path of the database-backed async surface. Interim by design: T24a
/// promotes these operations onto [`V1`] once the in-memory path is gone, so P0
/// ends on one version (P12). Route paths are built from these two constants and
/// nowhere else — the promotion is then a constant change, not a sweep.
pub const V2: &str = "/types-registry/v2";
