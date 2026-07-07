//! REST API layer for the `bss-ledger` gear.
//!
//! Hosts the seller-provisioning handler plus the shared error mapping that
//! converts authz-gate and body-rejection errors into RFC 9457 `Problem`
//! responses (rendered by `toolkit::api::canonical_error_middleware`).

pub mod adjustments;
pub mod approvals;
pub mod audit;
pub(crate) mod auth_context;
pub(crate) mod canonical_json;
pub mod closure;
pub mod control;
pub mod credit;
pub mod disputes;
pub mod dto;
pub mod error;
pub mod exceptions;
pub mod fx;
pub mod fx_revaluation_mode;
pub mod journal_entries;
pub mod payers;
pub mod payments;
pub mod posting_policy;
pub mod provisioning;
pub mod recognition;
pub mod reconciliation;
pub mod refunds;
