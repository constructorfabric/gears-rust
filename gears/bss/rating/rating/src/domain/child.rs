//! Child windows (DESIGN §3.1, §3.7 `bss_rating__child_window`; feature 06 §4.1).
//!
//! Fact intake creates the `period_line` child of a recurring fact and the children a usage fact
//! fails closed (`resource_scope_not_supported`, `usage_policy_mismatch`); the scheduler (feature
//! 06b) creates the due usage windows, and the rater (feature 07) evaluates them all. A newer fact
//! version bumps every child of the fact and deletes none.

use async_trait::async_trait;
use bss_rating_core::RatingWindow;
use bss_rating_core::engine::current::input::{AggregationKey, ChildKind};
use time::OffsetDateTime;
use toolkit_db::secure::DbTx;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::digest::Sha256Hex;
use crate::domain::error::DomainError;

/// The stored name of a child kind.
#[must_use]
pub const fn kind_name(kind: ChildKind) -> &'static str {
    match kind {
        ChildKind::UsageWindow => "usage_window",
        ChildKind::PeriodLine => "period_line",
        ChildKind::OneTime => "one_time",
    }
}

/// The child kind a stored name denotes.
#[must_use]
pub fn parse_kind(name: &str) -> Option<ChildKind> {
    match name {
        "usage_window" => Some(ChildKind::UsageWindow),
        "period_line" => Some(ChildKind::PeriodLine),
        "one_time" => Some(ChildKind::OneTime),
        _ => None,
    }
}

/// A child's status (DESIGN 14 §3.1 `ChildStatus`).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChildStatus {
    /// Waiting: not due, or its evidence or inputs are incomplete.
    Pending,
    /// An estimate exists; never delivered.
    Provisional,
    /// Final for its current input generation.
    Final,
    /// Final once, but its inputs moved on: corrected in a new revision.
    FinalStale,
    /// Failed closed: an exception names why.
    Failed,
}

impl ChildStatus {
    /// The stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Provisional => "provisional",
            Self::Final => "final",
            Self::FinalStale => "final_stale",
            Self::Failed => "failed",
        }
    }

    /// The status a stored name denotes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "pending" => Some(Self::Pending),
            "provisional" => Some(Self::Provisional),
            "final" => Some(Self::Final),
            "final_stale" => Some(Self::FinalStale),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    /// The status a newer fact version moves this one to (feature 06 §3.4): `final` becomes
    /// `final_stale`; every other status stays.
    #[must_use]
    pub const fn bumped(self) -> Self {
        match self {
            Self::Final => Self::FinalStale,
            other => other,
        }
    }
}

/// Why fact intake created a child failed (DESIGN 14 §3.1 `FailureReason`).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChildFailure {
    /// The binding's `aggregation_scope` is `resource` (LAUNCH-GATED, T-D-75).
    ResourceScopeNotSupported,
    /// The usage bindings of one fact disagree on the window, scope, meter or dimension value
    /// (T-D-87).
    UsagePolicyMismatch,
}

impl ChildFailure {
    /// The stored reason and the exception's reason code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ResourceScopeNotSupported => "resource_scope_not_supported",
            Self::UsagePolicyMismatch => "usage_policy_mismatch",
        }
    }
}

/// A child to create if absent.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct NewChild {
    /// `UUIDv5` of its `wk1` name.
    pub child_id: Uuid,
    /// The seller tenant.
    pub tenant_id: Uuid,
    /// Its fact.
    pub fact_id: Uuid,
    /// The fact's shard.
    pub shard: i16,
    /// What kind of child.
    pub kind: ChildKind,
    /// The subscription (the work queue's partition key).
    pub subscription_id: Uuid,
    /// The `PriceBook` item.
    pub item_id: Uuid,
    /// The dimension value pinned at sale.
    pub dimension_value: Option<String>,
    /// Its aggregation key, and that key's digest.
    pub aggregation_key: AggregationKey,
    /// SHA-256 of the key's canonical JSON.
    pub agg_key_digest: Sha256Hex,
    /// Its window.
    pub window: RatingWindow,
    /// `pending` or `failed`.
    pub status: ChildStatus,
    /// Why a failed child failed.
    pub failure: Option<ChildFailure>,
    /// The earliest instant its state can change by time alone.
    pub next_check_at: Option<OffsetDateTime>,
}

/// A child's work item on `rating.child_work`: a notification only (DESIGN 14 §3.1 `WorkItem`).
#[expect(
    clippy::struct_field_names,
    reason = "the ids as the work item and DESIGN 14 §3.1 name them"
)]
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChildWork {
    /// The seller tenant.
    pub tenant_id: Uuid,
    /// The child.
    pub child_id: Uuid,
    /// The partition key: `hash(subscription_id)`.
    pub subscription_id: Uuid,
}

/// `bss_rating__child_window`.
#[async_trait]
pub trait ChildRepo: Send + Sync {
    /// Inserts `child` unless its id or its `(tenant_id, fact_id, window_start, agg_key_digest)`
    /// exists: `true` when this call created it.
    async fn insert_if_absent(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        child: &NewChild,
        at: OffsetDateTime,
    ) -> Result<bool, DomainError>;

    /// Bumps every child of the fact for a newer fact version — `input_generation + 1`, `final`
    /// to `final_stale` — and returns their work items.
    async fn bump_fact(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        fact_id: Uuid,
        at: OffsetDateTime,
    ) -> Result<Vec<ChildWork>, DomainError>;
}

#[cfg(test)]
#[path = "child_tests.rs"]
mod tests;
