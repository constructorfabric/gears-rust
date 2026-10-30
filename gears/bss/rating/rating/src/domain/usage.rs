//! Usage capture and normalization (feature 04; DESIGN §3.6 Flow A and B, [contract 12]).
//!
//! One `UsageFeedReader` per canonical source pulls the usage-collector feed while it holds the
//! Cluster leadership `usage-feed-{source_uuid}`. Each page is one transaction: the checkpoint row
//! is locked and its cursor compared (the CAS), every entry is stored complete with its digest
//! together with its interpretation, invalidations withdraw their targets, and the cursor advances
//! with the captures. A feed gap becomes a durable source loss.
//!
//! What this feature does not do: attribution and counting (feature 05, through
//! [`ContributionSink`]), and the children a source loss blocks (features 05–08).
//!
//! [contract 12]: ../../../docs/DESIGN.md#contract-12

pub mod declarations;
pub mod entry;
pub mod feed;
pub mod intake;
pub mod loss;
pub mod metrics;
pub mod normalize;
pub mod reader;
pub mod source;

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_db::secure::DbTx;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::digest::Sha256Hex;
use crate::domain::error::DomainError;
use crate::domain::exception::ExceptionReason;
use crate::domain::usage::entry::{DigestVersion, EntryIdentity};
use crate::domain::usage::feed::FeedCursor;
use crate::domain::usage::loss::{LossKind, NewSourceLoss, SourceLoss};
use crate::domain::usage::normalize::{
    EntryKind, EntryType, NaturalKey, NormalizedUsage, RejectReason, Rejection,
};
use crate::domain::usage::source::{Checkpoint, SourceId, Transition};

/// The contribution state of a captured entry (DESIGN §3.6 Flow A state machine). This feature
/// writes `captured`, `rejected`, `withdrawn` and `applied`; the deferred states and `counted`
/// are feature 05's.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContributionState {
    /// Stored and interpreted; handed to attribution.
    Captured,
    /// Interpretation failed; the raw row is the retry input.
    Rejected,
    /// No covering attribution segment yet (feature 05).
    AwaitingAttribution,
    /// No usage policy projection yet (feature 05).
    AwaitingSpec,
    /// The interval crosses a boundary (feature 05).
    BoundarySplitRequired,
    /// Two segments cover it (feature 05).
    Quarantined,
    /// It contributes to a counter (feature 05).
    Counted,
    /// Withdrawn by an invalidation; it never contributes again.
    Withdrawn,
    /// An invalidation entry, applied in its capture transaction; terminal.
    Applied,
}

impl ContributionState {
    /// Every state, in a stable order.
    pub const ALL: [Self; 9] = [
        Self::Captured,
        Self::Rejected,
        Self::AwaitingAttribution,
        Self::AwaitingSpec,
        Self::BoundarySplitRequired,
        Self::Quarantined,
        Self::Counted,
        Self::Withdrawn,
        Self::Applied,
    ];

    /// The stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Captured => "captured",
            Self::Rejected => "rejected",
            Self::AwaitingAttribution => "awaiting_attribution",
            Self::AwaitingSpec => "awaiting_spec",
            Self::BoundarySplitRequired => "boundary_split_required",
            Self::Quarantined => "quarantined",
            Self::Counted => "counted",
            Self::Withdrawn => "withdrawn",
            Self::Applied => "applied",
        }
    }

    /// The state a stored name denotes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == name)
    }
}

/// The state an interpreted entry is stored in: an invalidation is `applied` in its own capture
/// transaction, a record is `captured` and handed to attribution.
#[must_use]
pub const fn interpreted_state(normalized: &NormalizedUsage) -> ContributionState {
    match normalized.kind {
        EntryKind::Record => ContributionState::Captured,
        EntryKind::Invalidation(_) => ContributionState::Applied,
    }
}

/// What an entry's interpretation decided; its capture insert stores it (feature 04 §4.1). Each
/// stored state carries exactly what its row needs.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// Interpreted: [`interpreted_state`] says `captured` or `applied`.
    Interpreted(NormalizedUsage),
    /// Interpretation failed: stored `rejected`, with an exception.
    Rejected(Rejection),
    /// A record whose invalidation was stored first: stored `withdrawn` whether or not it could
    /// be interpreted; it never contributes.
    Withdrawn {
        /// The invalidation that withdrew it.
        by: Uuid,
        /// Its interpretation, kept on the row.
        interpretation: Result<NormalizedUsage, Rejection>,
    },
}

impl CaptureOutcome {
    /// The contribution state the row is stored in.
    #[must_use]
    pub const fn state(&self) -> ContributionState {
        match self {
            Self::Interpreted(normalized) => interpreted_state(normalized),
            Self::Rejected(_) => ContributionState::Rejected,
            Self::Withdrawn { .. } => ContributionState::Withdrawn,
        }
    }

    /// The normalized columns, when the entry could be interpreted.
    #[must_use]
    pub const fn normalized(&self) -> Option<&NormalizedUsage> {
        match self {
            Self::Interpreted(normalized)
            | Self::Withdrawn {
                interpretation: Ok(normalized),
                ..
            } => Some(normalized),
            Self::Rejected(_)
            | Self::Withdrawn {
                interpretation: Err(_),
                ..
            } => None,
        }
    }

    /// Why it could not be interpreted.
    #[must_use]
    pub const fn reject_reason(&self) -> Option<RejectReason> {
        match self {
            Self::Rejected(rejection)
            | Self::Withdrawn {
                interpretation: Err(rejection),
                ..
            } => Some(rejection.reason),
            Self::Interpreted(_)
            | Self::Withdrawn {
                interpretation: Ok(_),
                ..
            } => None,
        }
    }

    /// The invalidation that withdrew it.
    #[must_use]
    pub const fn withdrawn_by(&self) -> Option<Uuid> {
        match self {
            Self::Withdrawn { by, .. } => Some(*by),
            Self::Interpreted(_) | Self::Rejected(_) => None,
        }
    }
}

/// A row the capture insert writes: the entry, its digest and the interpretation's outcome.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUsageRecord {
    /// Its identity.
    pub identity: EntryIdentity,
    /// The source that read it.
    pub source_id: SourceId,
    /// The entry as received, or its stub when larger than
    /// [`entry::MAX_RAW_ENTRY_BYTES`](crate::domain::usage::entry::MAX_RAW_ENTRY_BYTES).
    pub raw_entry: Value,
    /// The digest under `raw_digest_version`, over the whole entry.
    pub raw_sha256: Sha256Hex,
    /// The digest version.
    pub raw_digest_version: DigestVersion,
    /// When it was captured.
    pub captured_at: OffsetDateTime,
    /// What its interpretation decided.
    pub outcome: CaptureOutcome,
}

/// The exceptions usage capture raises on a usage record (feature 04 §4.5), each a reason code
/// fixed at compile time.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageException {
    /// A re-read entry whose digest differs from the stored one.
    ContentConflict,
    /// An invalidation whose target is not stored (opened on the target id).
    TargetUnknown,
    /// An entry stored `rejected`.
    Rejected(RejectReason),
}

impl UsageException {
    /// The exception's reason code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ContentConflict => "usage_content_conflict",
            Self::TargetUnknown => "invalidation_target_unknown",
            Self::Rejected(reason) => reason.as_str(),
        }
    }

    /// The reason the exception is keyed on.
    #[must_use]
    pub fn reason(self) -> ExceptionReason {
        ExceptionReason::from_code(self.code())
    }
}

/// How capture closes an exception it raised.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resolution {
    /// The record was withdrawn by its invalidation.
    Withdrawn,
    /// A retry interpreted the record.
    Retried,
}

impl Resolution {
    /// The stored resolution.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Withdrawn => "withdrawn",
            Self::Retried => "retried",
        }
    }
}

/// A stored row, as far as this feature reads it.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredUsageRecord {
    /// Its identity.
    pub identity: EntryIdentity,
    /// The entry as stored.
    pub raw_entry: Value,
    /// Its digest.
    pub raw_sha256: Sha256Hex,
    /// The digest version it was captured under; `None` for one this build does not know.
    pub raw_digest_version: Option<DigestVersion>,
    /// Its contribution state.
    pub state: ContributionState,
    /// Why it is rejected.
    pub reject_reason: Option<RejectReason>,
    /// The invalidation that withdrew it.
    pub withdrawn_by: Option<Uuid>,
    /// Its entry type, once interpreted.
    pub entry_type: Option<EntryType>,
}

/// A stored row's digest: what a re-read is compared with.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDigest {
    /// Its identity.
    pub identity: EntryIdentity,
    /// Its digest.
    pub raw_sha256: Sha256Hex,
    /// The digest version; `None` for one this build does not know.
    pub raw_digest_version: Option<DigestVersion>,
}

/// Why an entry went to the capture rejects instead of `bss_rating__usage_record`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureRejectKind {
    /// Its `id` or `tenant_id` cannot be read; it opened this `unidentifiable_entry` loss.
    Unidentifiable {
        /// The loss.
        loss_id: Uuid,
    },
    /// It names a meter or a tenant the source does not read: the collector sent what the source
    /// never asked for. Nothing of the source is lost, so no loss opens.
    OutsideSubscription,
}

impl CaptureRejectKind {
    /// The stored `reason` of an unidentifiable entry.
    pub const UNIDENTIFIABLE: &str = "unidentifiable";
    /// The stored `reason` of an entry outside the subscription.
    pub const OUTSIDE_SUBSCRIPTION: &str = "outside_subscription";

    /// The stored `reason`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unidentifiable { .. } => Self::UNIDENTIFIABLE,
            Self::OutsideSubscription => Self::OUTSIDE_SUBSCRIPTION,
        }
    }
}

/// An entry kept out of `bss_rating__usage_record` (`bss_rating__usage_capture_reject`).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureReject {
    /// The source that read it.
    pub source_id: SourceId,
    /// The digest of the whole entry.
    pub raw_sha256: Sha256Hex,
    /// The entry as received, or its stub when larger than
    /// [`entry::MAX_RAW_ENTRY_BYTES`](crate::domain::usage::entry::MAX_RAW_ENTRY_BYTES).
    pub raw_entry: Value,
    /// When it was captured.
    pub captured_at: OffsetDateTime,
    /// Why.
    pub kind: CaptureRejectKind,
}

/// `bss_rating__source_checkpoint`: platform-scoped, written only by the gear's service identity
/// in a page, restart or reconciliation transaction.
#[async_trait]
pub trait CheckpointRepo: Send + Sync {
    /// Every usage source's checkpoint, each locked for the rest of the transaction (`FOR
    /// UPDATE`), so two reconciliations apply one after the other.
    async fn lock_usage(&self, tx: &DbTx<'_>) -> Result<Vec<Checkpoint>, DomainError>;

    /// The checkpoint of `source`, locked for the rest of the transaction (`FOR UPDATE`).
    async fn lock(
        &self,
        tx: &DbTx<'_>,
        source: SourceId,
    ) -> Result<Option<Checkpoint>, DomainError>;

    /// Stores a new checkpoint; `false` when the source already has one.
    async fn insert(&self, tx: &DbTx<'_>, checkpoint: &Checkpoint) -> Result<bool, DomainError>;

    /// Writes the cursor of a locked checkpoint.
    async fn set_cursor(
        &self,
        tx: &DbTx<'_>,
        source: SourceId,
        cursor: Option<&FeedCursor>,
        now: OffsetDateTime,
    ) -> Result<(), DomainError>;

    /// Moves `source` along `transition` if it is in its `from` state; `true` when it moved.
    async fn set_state(
        &self,
        tx: &DbTx<'_>,
        source: SourceId,
        transition: Transition,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError>;

    /// Records that the usage sources were reconciled under configuration `generation`: every
    /// usage row with a lower one takes it.
    async fn stamp_generation(&self, tx: &DbTx<'_>, generation: u64) -> Result<(), DomainError>;
}

/// `bss_rating__usage_record` and `bss_rating__usage_capture_reject`. Every record read and write
/// runs under the scope of the row's own tenant.
#[async_trait]
pub trait UsageRecordRepo: Send + Sync {
    /// The stored row of `identity`.
    async fn find(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        identity: EntryIdentity,
    ) -> Result<Option<StoredUsageRecord>, DomainError>;

    /// The digests of the rows of `tenant_id` among `ids` that are stored: one read for a page's
    /// entries of one tenant.
    async fn stored_digests(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<StoredDigest>, DomainError>;

    /// The capture insert, `ON CONFLICT (tenant_id, usage_record_id) DO NOTHING`: `false` when a
    /// row of the identity is already stored.
    ///
    /// # Errors
    ///
    /// [`DomainError::Concurrent`] when a concurrent writer stored the natural key first (the page
    /// aborts once; its re-read takes the conflict branch).
    async fn insert(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        record: &NewUsageRecord,
    ) -> Result<bool, DomainError>;

    /// The id of the stored row holding `key`, other than `except`.
    async fn natural_key_holder(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        key: &NaturalKey,
        except: Uuid,
    ) -> Result<Option<Uuid>, DomainError>;

    /// The id of the stored invalidation naming `target` (in the target's tenant), other than
    /// `except`.
    async fn invalidation_naming(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        target: EntryIdentity,
        except: Uuid,
    ) -> Result<Option<Uuid>, DomainError>;

    /// The conditional withdrawal: `target` moves from `from` to `withdrawn` with `withdrawn_by =
    /// by` if it is still in `from` and not withdrawn; `true` when this call moved it.
    async fn withdraw(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        target: EntryIdentity,
        from: ContributionState,
        by: Uuid,
        at: OffsetDateTime,
    ) -> Result<bool, DomainError>;

    /// A retried row re-interpreted: writes `normalized` and moves it from `rejected` to `to` if
    /// it is still rejected and not withdrawn; `true` when this call moved it.
    async fn complete_retry(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        normalized: &NormalizedUsage,
        to: ContributionState,
    ) -> Result<bool, DomainError>;

    /// A retried row rejected again, for `reason`, if it is still rejected and not withdrawn;
    /// `true` when this call wrote it.
    async fn reject_again(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        identity: EntryIdentity,
        reason: RejectReason,
    ) -> Result<bool, DomainError>;

    /// Whether the entry of digest `raw_sha256` is among `source`'s capture rejects.
    async fn capture_rejected(
        &self,
        tx: &DbTx<'_>,
        source: SourceId,
        raw_sha256: &Sha256Hex,
    ) -> Result<bool, DomainError>;

    /// Stores a capture reject; `false` when the same entry was already stored for the source.
    async fn insert_capture_reject(
        &self,
        tx: &DbTx<'_>,
        reject: &CaptureReject,
    ) -> Result<bool, DomainError>;
}

/// `bss_rating__source_loss`, under the scope of the owner root of the source's tenant scope.
#[async_trait]
pub trait SourceLossRepo: Send + Sync {
    /// Opens `loss`, or returns the open loss of its `(tenant_id, source_id, kind,
    /// last_good_cursor, tenant_scope)`: a re-detected loss reuses its open row.
    async fn open(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        loss: &NewSourceLoss,
    ) -> Result<SourceLoss, DomainError>;

    /// The number of not-resolved losses of `source`, by kind.
    async fn count_unresolved(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        source: SourceId,
    ) -> Result<BTreeMap<LossKind, u64>, DomainError>;
}

/// The step after interpretation: attribution and counting (feature 05). Runs inside the page
/// transaction.
#[async_trait]
pub trait ContributionSink: Send + Sync {
    /// A record newly stored `captured` (or retried back to `captured`) is ready for attribution.
    async fn interpret(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        record: &NormalizedUsage,
    ) -> Result<(), DomainError>;

    /// A record in one of this step's states (`counted` or a deferred state) was withdrawn by
    /// `invalidation`, leaving `from`: the step undoes what it did there (a counted contribution
    /// leaves its recorded coordinates, `−quantity` with the child enqueued; a deferred state's
    /// exception is resolved).
    async fn withdraw(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        target: EntryIdentity,
        from: ContributionState,
        invalidation: Uuid,
    ) -> Result<(), DomainError>;
}

/// The contribution step before feature 05: a captured record stays `captured`, and no record is
/// ever in one of feature 05's states, so withdrawing one is a fault that fails the page rather
/// than a counter or an exception left behind.
#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoAttribution;

#[async_trait]
impl ContributionSink for NoAttribution {
    async fn interpret(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        _record: &NormalizedUsage,
    ) -> Result<(), DomainError> {
        Ok(())
    }

    async fn withdraw(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        target: EntryIdentity,
        from: ContributionState,
        _invalidation: Uuid,
    ) -> Result<(), DomainError> {
        Err(DomainError::Internal(
            format!(
                "usage record {} is {}, a state no contribution step is built to withdraw it from",
                target.usage_record_id,
                from.as_str()
            ),
            None,
        ))
    }
}
