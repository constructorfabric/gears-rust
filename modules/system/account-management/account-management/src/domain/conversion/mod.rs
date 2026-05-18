//! Conversion-request domain module.
//!
//! Implements FEATURE `managed-self-managed-modes` (see
//! `modules/system/account-management/docs/features/feature-managed-self-managed-modes.md`).
//!
//! This module owns the durable state machine for post-creation tenant
//! mode changes: a `ConversionRequest` carries the dual-consent contract
//! between an initiator (child or parent side) and the counterparty, and
//! resolves into one of the four terminal states `approved`, `cancelled`,
//! `rejected`, `expired`. Each transition is gated by a role-per-transition
//! rule that this module's pure state-machine guard
//! ([`state_machine::validate_transition`]) checks before any DB write.
//!
//! Layering:
//!
//! * [`model`] — pure value types ([`model::ConversionRequest`],
//!   [`model::NewConversionRequest`], [`model::ConversionStatus`],
//!   [`model::ConversionSide`], [`model::TargetMode`],
//!   [`model::ConversionPagination`]).
//! * [`state_machine`] — the role-per-transition guard used by the
//!   service layer (and re-applied as defence-in-depth by the
//!   repo-impl) before touching the DB.
//! * [`repo`] — the [`repo::ConversionRepo`] trait that the service
//!   layer talks to. The `SeaORM`-backed implementation lives in
//!   `crate::infra::storage::repo_impl::conversion`; an in-memory fake
//!   for unit tests lives under [`test_support`].
//! * [`service`] — [`service::ConversionService`] composes the guards
//!   and orchestrates the dual-consent
//!   `pending -> {approved, cancelled, rejected, expired}` lifecycle.
//!
//! The REST surface for `/tenants/{id}/conversions` and
//! `/tenants/{id}/child-conversions` is intentionally not wired in this
//! crate yet — it lands in a follow-up PR once the `InTenantSubtree`
//! predicate (cyberfabric-core#1813) makes the storage-level subtree
//! clamp safe for cross-barrier authorization. The domain types here
//! are REST-ready so that drop-in is a thin handler wiring step.

pub mod model;
pub mod page;
pub mod repo;
pub mod service;
pub mod state_machine;

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
