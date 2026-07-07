//! Infrastructure authorization helpers for the ledger's audit surface.
//!
//! Hosts the cross-tenant elevation gateway (Slice 6 Phase 2 Group 2C): the
//! forensic-gated path that turns an explicit `targetScope` + role + reason into
//! a same-transaction `cross-tenant-access` secured-audit record before the
//! target tenant is ever read.

pub mod cross_tenant;
