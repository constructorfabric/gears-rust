//! Commercial fact intake (feature 06 §2.1, §3.1, §3.3, §3.4; DESIGN §3.6 Flow C and the usage
//! parent flow, [14 §3.6]; DECISIONS T-D-73, T-D-87).
//!
//! Subscriptions publishes versioned, full-replacement facts (`subscriptions-rating/1`, its PR
//! #5516), each carrying its served extent, timing, billing terms and a timeline of slices with
//! the frozen accepted bindings and purchase lots in force. Fact intake accepts each version
//! through the inbox in one transaction: it stores the version insert-only and raises the head's
//! `current_version`, stores each accepted binding as the binding of record, creates the
//! `period_line` child of a recurring fact or the schedule, usage-policy projection and expected
//! manifest of a usage fact, and bumps every child of the fact when the version is newer. A lower
//! version is stored and touches nothing else (V81).
//!
//! What this feature does not do: pin the finalization policy and create the due usage windows
//! (feature 06b, which pins every schedule this one creates `policy_pending`), the provisional
//! children of windows that already have counters (feature 05), and evaluation (feature 07).
//!
//! [14 §3.6]: ../../../docs/features/06-fact-scheduling.md#contract-14-3-6-f06

pub mod expected;
pub mod intake;
pub mod key;
pub mod model;
pub mod profile;

use async_trait::async_trait;
use bss_rating_core::engine::current::input::{DerivedInput, RatingWindowKind};
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_db::secure::DbTx;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::digest::Sha256Hex;
use crate::domain::error::DomainError;
use crate::domain::fact::model::{FactKind, FactVersion};

/// Why a usage fact's head is not bound yet (DESIGN 14 §3.1 `FactBindingState`).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PendingBinding {
    /// Products' derived declaration could not be read (R-31): retried on `rating.fact_bind`.
    DerivedDeclarationUnavailable,
}

/// Why a usage fact's head failed to bind.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailedBinding {
    /// The derived declaration cannot be rated, or differs from the stored copy.
    DerivedDeclarationConflict,
}

/// Whether a fact's inputs are complete: its schedule, expected manifest and `rollup_due_at`
/// exist only once it is `bound`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindingState {
    /// Waiting for an input; a retry runs on `rating.fact_bind`.
    Pending(PendingBinding),
    /// Every input is stored.
    Bound,
    /// An input can never be used; a new fact version may bring another.
    Failed(FailedBinding),
}

impl BindingState {
    /// The stored `binding_state`.
    #[must_use]
    pub const fn state(self) -> &'static str {
        match self {
            Self::Pending(_) => "pending",
            Self::Bound => "bound",
            Self::Failed(_) => "failed",
        }
    }

    /// The stored `binding_reason`.
    #[must_use]
    pub const fn reason(self) -> Option<&'static str> {
        match self {
            Self::Pending(PendingBinding::DerivedDeclarationUnavailable) => {
                Some("derived_declaration_unavailable")
            }
            Self::Bound => None,
            Self::Failed(FailedBinding::DerivedDeclarationConflict) => {
                Some("derived_declaration_conflict")
            }
        }
    }

    /// The state stored columns denote.
    #[must_use]
    pub fn parse(state: &str, reason: Option<&str>) -> Option<Self> {
        match (state, reason) {
            ("pending", Some("derived_declaration_unavailable")) => {
                Some(Self::Pending(PendingBinding::DerivedDeclarationUnavailable))
            }
            ("bound", None) => Some(Self::Bound),
            ("failed", Some("derived_declaration_conflict")) => {
                Some(Self::Failed(FailedBinding::DerivedDeclarationConflict))
            }
            _ => None,
        }
    }
}

/// `bss_rating__fact_head`: one row per fact, updated under its row lock.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactHead {
    /// The seller tenant.
    pub tenant_id: Uuid,
    /// The fact.
    pub fact_id: Uuid,
    /// `hash(fact_id) mod S`, stored at insert and never recomputed.
    pub shard: i16,
    /// What the fact bills.
    pub kind: FactKind,
    /// The highest version stored.
    pub current_version: i64,
    /// Whether the current version's inputs are complete.
    pub binding: BindingState,
    /// The version the stored inputs answer.
    pub bound_version: Option<i64>,
    /// When a pending binding is retried.
    pub binding_next_check_at: Option<OffsetDateTime>,
    /// The expected children of the bound version.
    pub expected_manifest_digest: Option<Sha256Hex>,
    /// When roll-up next looks at the fact.
    pub rollup_due_at: Option<OffsetDateTime>,
    /// The billing currency.
    pub currency: String,
    /// The inbox receipt of the current version.
    pub received_at: OffsetDateTime,
    /// The last change.
    pub updated_at: OffsetDateTime,
}

/// A fact version as stored, with its inbox entry.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct StoredVersion<'a> {
    /// The version.
    pub fact: &'a FactVersion,
    /// The inbox entry it arrived as.
    pub entry_id: Uuid,
    /// Rating's digest of the payload.
    pub payload_digest: &'a Sha256Hex,
    /// The payload, verbatim.
    pub payload: &'a Value,
    /// When it arrived.
    pub received_at: OffsetDateTime,
}

/// `bss_rating__fact` and `bss_rating__fact_head`.
#[async_trait]
pub trait FactRepo: Send + Sync {
    /// Stores a fact version unless it is stored: `true` when this call stored it.
    async fn insert_version(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        version: &StoredVersion<'_>,
    ) -> Result<bool, DomainError>;

    /// The stored payload of a fact version.
    async fn version_payload(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        fact_id: Uuid,
        fact_version: i64,
    ) -> Result<Option<Value>, DomainError>;

    /// The fact's head, without a lock.
    async fn find_head(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        fact_id: Uuid,
    ) -> Result<Option<FactHead>, DomainError>;

    /// The fact's head, locked for the transaction (`FOR UPDATE`; SQLite serializes writers).
    async fn lock_head(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        fact_id: Uuid,
    ) -> Result<Option<FactHead>, DomainError>;

    /// Inserts a head; [`DomainError::Concurrent`] when another transaction inserted it first.
    async fn insert_head(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        head: &FactHead,
    ) -> Result<(), DomainError>;

    /// Writes a locked head back.
    async fn update_head(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        head: &FactHead,
    ) -> Result<(), DomainError>;
}

/// A usage schedule fact intake creates `policy_pending`: feature 06b pins its finalization
/// policy and creates its windows when they are due.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSchedule {
    /// The seller tenant.
    pub tenant_id: Uuid,
    /// The fact.
    pub fact_id: Uuid,
    /// The usage item.
    pub item_id: Uuid,
    /// The fact's shard.
    pub shard: i16,
    /// The fact version it serves; raised by a newer version, never lowered.
    pub fact_version: i64,
    /// The binding's rating window.
    pub window_policy: RatingWindowKind,
    /// The binding's usage rating policy.
    pub usage_policy_id: Uuid,
    /// Its version.
    pub usage_policy_version: u64,
    /// Its digest.
    pub usage_policy_digest: String,
    /// When the scheduler next looks at it: now, so it is pinned at the next scan.
    pub next_due_at: OffsetDateTime,
    /// How many windows the fact expects.
    pub expected_count: usize,
}

/// `bss_rating__window_schedule`.
#[async_trait]
pub trait ScheduleRepo: Send + Sync {
    /// Creates the schedule, or raises its `fact_version` when it exists with a lower one; a
    /// pinned policy is never touched.
    async fn upsert(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        schedule: &NewSchedule,
        at: OffsetDateTime,
    ) -> Result<(), DomainError>;
}

/// One row of the usage policy projection (DESIGN §3.7): what ingestion reads to place a record
/// into its window and granule.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionRow {
    /// The seller tenant.
    pub tenant_id: Uuid,
    /// The subscription.
    pub subscription_id: Uuid,
    /// The usage item.
    pub item_id: Uuid,
    /// The served extent's start.
    pub valid_from: OffsetDateTime,
    /// The served extent's end.
    pub valid_to: OffsetDateTime,
    /// The fact version it was written from.
    pub fact_version: i64,
    /// The fact.
    pub fact_id: Uuid,
    /// The component interval alias.
    pub sub_line_key: String,
    /// The usage rating policy.
    pub policy_id: Uuid,
    /// Its version.
    pub policy_version: u64,
    /// Its digest.
    pub policy_digest: String,
    /// Its rating window.
    pub rating_window: RatingWindowKind,
    /// The derived meter, `products.derived/<code>@<n>`.
    pub meter: String,
    /// The declaration's inputs: names, raw GTS ids and folds.
    pub inputs: Vec<DerivedInput>,
    /// The binding of record of the first chargeable slice.
    pub binding_digest: Sha256Hex,
}

/// `bss_rating__usage_policy_projection`: insert-only.
#[async_trait]
pub trait ProjectionRepo: Send + Sync {
    /// Stores `row` unless its key is stored.
    async fn insert(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        row: &ProjectionRow,
        at: OffsetDateTime,
    ) -> Result<(), DomainError>;
}

/// `hash(fact_id) mod shards`: the shard a fact's head, schedule and children are scanned in
/// (DESIGN 14 §3.8). `shards` is at least 1.
#[must_use]
pub fn shard_of(fact_id: Uuid, shards: u16) -> i16 {
    let bytes = fact_id.as_bytes();
    let high = u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]);
    let shard = high % u64::from(shards.max(1));
    i16::try_from(shard).unwrap_or(i16::MAX)
}

#[cfg(test)]
#[path = "fact_tests.rs"]
mod tests;
