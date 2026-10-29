//! The page transaction, the restart transaction and the retry of a rejected entry (feature 04
//! §2.1–§3.4; DESIGN §3.6 Flow A and B, [12 §3.6]).
//!
//! A page commits with all of its captures, their effects and the cursor advance, or with none:
//! the checkpoint row is locked and its cursor compared first, so a reader that lost its
//! leadership rolls back with no effect (V68). Each entry is interpreted in memory and stored by
//! one insert together with that outcome; a re-read entry with the same digest has no effect at
//! all (V01, V74), and one with a different digest raises `usage_content_conflict` and leaves the
//! stored row standing (V64).
//!
//! [12 §3.6]: ../../../../docs/features/04-usage-intake.md#contract-12-3-6

// @cpt-dod:cpt-cf-bss-rating-dod-intake-normalize:p1

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use toolkit_db::Db;
use toolkit_db::secure::DbTx;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::exception::{
    ExceptionKey, ExceptionRecord, ExceptionRegister, ExceptionRetrier, SubjectKind, SubjectRef,
};
use crate::domain::ports::Clock;
use crate::domain::usage::declarations::PageDeclarations;
use crate::domain::usage::entry::{
    self, BoundedEntry, CURRENT_DIGEST_VERSION, EntryIdentity, MAX_RAW_ENTRY_BYTES,
};
use crate::domain::usage::feed::FeedCursor;
use crate::domain::usage::loss::TaintedDays;
use crate::domain::usage::loss::{
    LossKind, NewSourceLoss, SourceLoss, retention_taint, unidentifiable_taint,
};
use crate::domain::usage::normalize::{
    EntryKind, InvalidationRef, NormalizedUsage, RejectReason, Rejection, normalize,
};
use crate::domain::usage::source::{
    Checkpoint, SourceDefinition, SourceId, SourcePlan, SourceState, TenantScope, Transition,
    plan_sources,
};
use crate::domain::usage::{
    CaptureOutcome, CaptureReject, CaptureRejectKind, CheckpointRepo, ContributionSink,
    ContributionState, NewUsageRecord, Resolution, SourceLossRepo, StoredDigest, UsageException,
    UsageRecordRepo, interpreted_state,
};

/// The exception reason of a re-read entry whose digest differs from the stored one.
pub const CONTENT_CONFLICT: &str = UsageException::ContentConflict.code();
/// The exception reason of an invalidation whose target is not stored (opened on the target id).
pub const TARGET_UNKNOWN: &str = UsageException::TargetUnknown.code();

/// What the intake is built from.
#[domain_model]
pub struct IntakeParts {
    /// `bss_rating__source_checkpoint`.
    pub checkpoints: Arc<dyn CheckpointRepo>,
    /// `bss_rating__usage_record` and the capture rejects.
    pub records: Arc<dyn UsageRecordRepo>,
    /// `bss_rating__source_loss`.
    pub losses: Arc<dyn SourceLossRepo>,
    /// The exception register.
    pub exceptions: ExceptionRegister,
    /// Attribution and counting (feature 05).
    pub contributions: Arc<dyn ContributionSink>,
    /// The time source.
    pub clock: Arc<dyn Clock>,
    /// The collector's retention floor `F`.
    pub retention_floor: Duration,
}

/// One entry of a prepared page, sorted before the transaction.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum PageEntry {
    /// Its tenant and id can be read, and its meter and tenant are the source's.
    Identified {
        /// What it is stored under.
        identity: EntryIdentity,
        /// The entry as it arrived.
        raw: Value,
    },
    /// Its `id` or `tenant_id` cannot be read.
    Unidentifiable(Value),
    /// It names a meter the source does not subscribe to, or a tenant outside its configured
    /// scope.
    OutsideSubscription(Value),
}

impl PageEntry {
    /// The identity and entry of an identified entry.
    #[must_use]
    pub const fn identified(&self) -> Option<(EntryIdentity, &Value)> {
        match self {
            Self::Identified { identity, raw } => Some((*identity, raw)),
            Self::Unidentifiable(_) | Self::OutsideSubscription(_) => None,
        }
    }
}

/// A page read and prepared outside the transaction: its entries sorted, its declarations resolved
/// and the scope of every tenant it writes for, and of the source's owner root, bound.
#[domain_model]
#[derive(Debug, Clone)]
pub struct PreparedPage {
    /// The entries, in feed order.
    pub entries: Vec<PageEntry>,
    /// The cursor the page advances the checkpoint to.
    pub next: FeedCursor,
    /// The declarations of the page's meters.
    pub declarations: PageDeclarations,
    /// The service scope of each tenant the page writes for.
    pub scopes: BTreeMap<Uuid, AccessScope>,
}

/// What one committed page did.
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageReport {
    /// Entries on the page.
    pub entries: usize,
    /// Entries stored for the first time.
    pub captured: usize,
    /// Re-read entries with the stored digest: no effect.
    pub duplicates: usize,
    /// Re-read entries with another digest: alarm, the stored row stands.
    pub content_conflicts: usize,
    /// Entries stored `rejected`, by reason.
    pub rejected: BTreeMap<RejectReason, usize>,
    /// Records stored `withdrawn` because their invalidation came first.
    pub withdrawn_on_capture: usize,
    /// Invalidations applied.
    pub invalidations_applied: usize,
    /// Invalidations whose target is not stored.
    pub targets_unknown: usize,
    /// Entries whose id or tenant could not be read, newly set aside.
    pub unidentifiable: usize,
    /// Entries of a meter or tenant the source does not read, newly set aside.
    pub outside_subscription: usize,
    /// The kinds of the losses the page opened.
    pub losses_opened: Vec<LossKind>,
}

/// How a page transaction ended.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageOutcome {
    /// Committed with the cursor advanced.
    Committed(PageReport),
    /// The checkpoint's cursor was no longer the one the page was read after (or the source was
    /// retired): nothing was written.
    CursorMoved,
}

/// How a restart transaction ended.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartOutcome {
    /// The loss is open and the cursor reset to `Oldest`.
    Restarted(SourceLoss),
    /// The cursor moved since the refused read: a stale reader resets nothing (V76).
    CursorMoved,
}

/// How a retry of a rejected entry ended.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryOutcome {
    /// Re-interpreted from the stored raw entry and handed on.
    Interpreted,
    /// Rejected again.
    RejectedAgain(Rejection),
    /// The row is not rejected: nothing was done.
    NotRejected(ContributionState),
}

/// The digests already stored for a page's identified entries, and those its own captures add.
type KnownDigests = BTreeMap<EntryIdentity, StoredDigest>;

/// Usage capture and normalization.
#[domain_model]
#[derive(Clone)]
pub struct UsageIntake {
    parts: Arc<IntakeParts>,
}

impl UsageIntake {
    /// An intake over `parts`.
    #[must_use]
    pub fn new(parts: IntakeParts) -> Self {
        Self {
            parts: Arc::new(parts),
        }
    }

    /// The collector's retention floor this intake taints days with.
    #[must_use]
    pub fn retention_floor(&self) -> Duration {
        self.parts.retention_floor
    }

    /// The page transaction of `source` for a page read after `expected` (feature 04 §2.1 steps
    /// 5–7).
    ///
    /// # Errors
    ///
    /// A storage failure, a concurrent natural-key writer ([`DomainError::Concurrent`]), or the
    /// contribution step's refusal: the transaction rolls back and the page is read again.
    #[tracing::instrument(name = "rating.feed.page", skip_all, fields(source = %source.source_id(), entries = page.entries.len()))]
    pub async fn apply_page(
        &self,
        db: &Db,
        source: &SourceDefinition,
        expected: Option<FeedCursor>,
        page: PreparedPage,
    ) -> Result<PageOutcome, DomainError> {
        let intake = self.clone();
        let source = source.clone();
        db.transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                intake
                    .apply_page_in(tx, &source, expected.as_ref(), &page)
                    .await
            })
        })
        .await
    }

    async fn apply_page_in(
        &self,
        tx: &DbTx<'_>,
        source: &SourceDefinition,
        expected: Option<&FeedCursor>,
        page: &PreparedPage,
    ) -> Result<PageOutcome, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-cas
        let Some(checkpoint) = self.locked_checkpoint(tx, source, expected).await? else {
            return Ok(PageOutcome::CursorMoved);
        };
        // @cpt-end:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-cas
        // @cpt-begin:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-each
        let mut report = PageReport {
            entries: page.entries.len(),
            ..PageReport::default()
        };
        let mut known = self.stored_digests(tx, page).await?;
        for entry in &page.entries {
            match entry {
                PageEntry::Identified { identity, raw } => {
                    self.capture(
                        tx,
                        &checkpoint,
                        page,
                        *identity,
                        raw,
                        &mut known,
                        &mut report,
                    )
                    .await?;
                }
                PageEntry::Unidentifiable(raw) => {
                    self.capture_unidentifiable(tx, &checkpoint, page, raw, &mut report)
                        .await?;
                }
                PageEntry::OutsideSubscription(raw) => {
                    self.capture_outside(tx, &checkpoint, raw, &mut report)
                        .await?;
                }
            }
        }
        // @cpt-end:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-each
        // @cpt-begin:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-commit
        self.parts
            .checkpoints
            .set_cursor(
                tx,
                checkpoint.source_id,
                Some(&page.next),
                self.parts.clock.now(),
            )
            .await?;
        Ok(PageOutcome::Committed(report))
        // @cpt-end:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-commit
    }

    /// The source's checkpoint, locked, if its cursor is still `expected` and it is not retired.
    async fn locked_checkpoint(
        &self,
        tx: &DbTx<'_>,
        source: &SourceDefinition,
        expected: Option<&FeedCursor>,
    ) -> Result<Option<Checkpoint>, DomainError> {
        let checkpoint = self
            .parts
            .checkpoints
            .lock(tx, source.source_id())
            .await?
            .ok_or(DomainError::NotFound {
                what: "usage source checkpoint",
            })?;
        let current =
            checkpoint.cursor.as_ref() == expected && checkpoint.state != SourceState::Retired;
        Ok(current.then_some(checkpoint))
    }

    /// The stored digests of the page's identified entries: one read per tenant, so a re-read
    /// page (after a rollback, or a restart from `Oldest`) is compared without interpreting its
    /// entries again.
    async fn stored_digests(
        &self,
        tx: &DbTx<'_>,
        page: &PreparedPage,
    ) -> Result<KnownDigests, DomainError> {
        let mut ids: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
        for (identity, _) in page.entries.iter().filter_map(PageEntry::identified) {
            ids.entry(identity.tenant_id)
                .or_default()
                .insert(identity.usage_record_id);
        }
        let mut known = KnownDigests::new();
        for (tenant, ids) in ids {
            let scope = tenant_scope(page, tenant)?;
            let ids: Vec<Uuid> = ids.into_iter().collect();
            for stored in self
                .parts
                .records
                .stored_digests(tx, scope, tenant, &ids)
                .await?
            {
                known.insert(stored.identity, stored);
            }
        }
        Ok(known)
    }

    /// One identified entry: compared with its stored digest when it is stored, else interpreted
    /// and captured with that outcome.
    #[allow(
        clippy::too_many_arguments,
        reason = "one page's state, threaded through its entries"
    )]
    async fn capture(
        &self,
        tx: &DbTx<'_>,
        checkpoint: &Checkpoint,
        page: &PreparedPage,
        identity: EntryIdentity,
        raw: &Value,
        known: &mut KnownDigests,
        report: &mut PageReport,
    ) -> Result<(), DomainError> {
        let scope = tenant_scope(page, identity.tenant_id)?;
        if let Some(stored) = known.get(&identity) {
            return self.recapture(tx, scope, stored, raw, report).await;
        }
        let bounded = entry::bounded(raw);
        let outcome = self
            .interpret(tx, scope, identity, raw, &bounded, &page.declarations)
            .await?;
        // @cpt-begin:cpt-cf-bss-rating-algo-intake-capture:p1:inst-cap-insert
        let record = NewUsageRecord {
            identity,
            source_id: checkpoint.source_id,
            raw_entry: bounded.value,
            raw_sha256: entry::digest(raw, CURRENT_DIGEST_VERSION),
            raw_digest_version: CURRENT_DIGEST_VERSION,
            captured_at: self.parts.clock.now(),
            outcome,
        };
        let inserted = self.parts.records.insert(tx, scope, &record).await?;
        // @cpt-end:cpt-cf-bss-rating-algo-intake-capture:p1:inst-cap-insert
        let stored = StoredDigest {
            identity,
            raw_sha256: record.raw_sha256.clone(),
            raw_digest_version: Some(record.raw_digest_version),
        };
        if !inserted {
            // Another source stored the identity after the page's lookup.
            let stored = self.stored_digest(tx, scope, identity).await?;
            known.insert(identity, stored.clone());
            return self.recapture(tx, scope, &stored, raw, report).await;
        }
        // A later entry of the page with the same identity is a re-read of this one.
        known.insert(identity, stored);
        // @cpt-begin:cpt-cf-bss-rating-algo-intake-capture:p1:inst-cap-return
        // @cpt-begin:cpt-cf-bss-rating-state-intake-capture:p1:inst-st-capture
        report.captured += 1;
        // @cpt-end:cpt-cf-bss-rating-state-intake-capture:p1:inst-st-capture
        self.apply_outcome(tx, scope, identity, &record.outcome, report)
            .await
        // @cpt-end:cpt-cf-bss-rating-algo-intake-capture:p1:inst-cap-return
    }

    async fn stored_digest(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        identity: EntryIdentity,
    ) -> Result<StoredDigest, DomainError> {
        self.parts
            .records
            .stored_digests(tx, scope, identity.tenant_id, &[identity.usage_record_id])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                DomainError::Concurrent(format!(
                    "usage record {} vanished after its insert conflicted",
                    identity.usage_record_id
                ))
            })
    }

    /// The outcome an entry about to be stored has (feature 04 §3.2).
    async fn interpret(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        identity: EntryIdentity,
        raw: &Value,
        bounded: &BoundedEntry,
        declarations: &PageDeclarations,
    ) -> Result<CaptureOutcome, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-algo-intake-normalize:p1:inst-norm-prewithdrawn
        // A record whose invalidation came first is withdrawn on capture (Flow A); an invalidation
        // is never itself the target of one.
        let invalidation_shaped = raw.get("invalidates").is_some_and(|value| !value.is_null());
        let withdrawn_by = if invalidation_shaped {
            None
        } else {
            self.parts
                .records
                .invalidation_naming(tx, scope, identity, identity.usage_record_id)
                .await?
        };
        // @cpt-end:cpt-cf-bss-rating-algo-intake-normalize:p1:inst-norm-prewithdrawn
        let interpretation = match bounded.oversize {
            // The row cannot keep it: a contract breach no retry can fix.
            Some(bytes) => Err(Rejection {
                reason: RejectReason::UsageMalformed,
                detail: format!(
                    "the entry is {bytes} bytes of JSON, more than the {MAX_RAW_ENTRY_BYTES} a row stores"
                ),
            }),
            None => {
                self.normalized(tx, scope, raw, identity, declarations)
                    .await?
            }
        };
        // @cpt-begin:cpt-cf-bss-rating-state-intake-capture:p1:inst-st-late-target
        // @cpt-begin:cpt-cf-bss-rating-state-intake-capture:p1:inst-st-reject
        // @cpt-begin:cpt-cf-bss-rating-state-intake-capture:p1:inst-st-applied
        Ok(match (withdrawn_by, interpretation) {
            (Some(by), interpretation) => CaptureOutcome::Withdrawn { by, interpretation },
            (None, Ok(normalized)) => CaptureOutcome::Interpreted(normalized),
            (None, Err(rejection)) => CaptureOutcome::Rejected(rejection),
        })
        // @cpt-end:cpt-cf-bss-rating-state-intake-capture:p1:inst-st-applied
        // @cpt-end:cpt-cf-bss-rating-state-intake-capture:p1:inst-st-reject
        // @cpt-end:cpt-cf-bss-rating-state-intake-capture:p1:inst-st-late-target
    }

    /// `raw` normalized and checked against the stored natural keys, or why it cannot be.
    async fn normalized(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        raw: &Value,
        identity: EntryIdentity,
        declarations: &PageDeclarations,
    ) -> Result<Result<NormalizedUsage, Rejection>, DomainError> {
        let normalized = match normalize(raw, identity, declarations) {
            Ok(normalized) => normalized,
            Err(rejection) => return Ok(Err(rejection)),
        };
        Ok(match self.key_conflict(tx, scope, &normalized).await? {
            Some(conflict) => Err(conflict),
            None => Ok(normalized),
        })
    }

    /// The natural-key conflict of `normalized`, if another stored entry holds its key or, for an
    /// invalidation, another invalidation names its target (12 §4.3; a safety net).
    async fn key_conflict(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        normalized: &NormalizedUsage,
    ) -> Result<Option<Rejection>, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-algo-intake-normalize:p1:inst-norm-natural-key
        let records = &self.parts.records;
        let own = normalized.identity.usage_record_id;
        if let Some(holder) = records
            .natural_key_holder(tx, scope, &normalized.natural_key(), own)
            .await?
        {
            return Ok(Some(Rejection {
                reason: RejectReason::NaturalKeyConflict,
                detail: format!("usage record {holder} holds the natural key"),
            }));
        }
        if let Some(invalidation) = normalized.invalidation() {
            let target = target_of(normalized.identity, invalidation);
            if let Some(other) = records.invalidation_naming(tx, scope, target, own).await? {
                return Ok(Some(Rejection {
                    reason: RejectReason::NaturalKeyConflict,
                    detail: format!("invalidation {other} already names the target"),
                }));
            }
        }
        Ok(None)
        // @cpt-end:cpt-cf-bss-rating-algo-intake-normalize:p1:inst-norm-natural-key
    }

    /// The effects of a newly stored entry's outcome, in the page transaction.
    async fn apply_outcome(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        identity: EntryIdentity,
        outcome: &CaptureOutcome,
        report: &mut PageReport,
    ) -> Result<(), DomainError> {
        match outcome {
            CaptureOutcome::Withdrawn { .. } => {
                report.withdrawn_on_capture += 1;
                self.resolve(
                    tx,
                    scope,
                    identity,
                    UsageException::TargetUnknown,
                    Resolution::Withdrawn,
                )
                .await
            }
            CaptureOutcome::Rejected(rejection) => {
                *report.rejected.entry(rejection.reason).or_default() += 1;
                self.record_rejection(tx, scope, identity, rejection).await
            }
            CaptureOutcome::Interpreted(normalized) => match &normalized.kind {
                EntryKind::Invalidation(reference) => {
                    self.apply_invalidation(tx, scope, normalized.identity, reference, report)
                        .await
                }
                // @cpt-begin:cpt-cf-bss-rating-algo-intake-normalize:p1:inst-norm-return
                EntryKind::Record => {
                    self.parts
                        .contributions
                        .interpret(tx, scope, normalized)
                        .await
                } // @cpt-end:cpt-cf-bss-rating-algo-intake-normalize:p1:inst-norm-return
            },
        }
    }

    /// A re-read entry whose identity is stored: no effect when its digest matches, an alarm when
    /// it does not (the stored row stands either way).
    async fn recapture(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        stored: &StoredDigest,
        raw: &Value,
        report: &mut PageReport,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-algo-intake-capture:p1:inst-cap-duplicate
        if same_digest(stored, raw) {
            report.duplicates += 1;
            return Ok(());
        }
        // @cpt-end:cpt-cf-bss-rating-algo-intake-capture:p1:inst-cap-duplicate
        // @cpt-begin:cpt-cf-bss-rating-algo-intake-capture:p1:inst-cap-conflict
        let identity = stored.identity;
        report.content_conflicts += 1;
        tracing::warn!(
            target: "bss_rating.usage",
            tenant_id = %identity.tenant_id,
            usage_record_id = %identity.usage_record_id,
            code = CONTENT_CONFLICT,
            "usage entry re-read with a different digest; the stored row stands"
        );
        let key = exception_key(identity, UsageException::ContentConflict);
        self.parts
            .exceptions
            .record(
                tx,
                scope,
                &key,
                "re-read with a different digest; the stored row stands",
            )
            .await?;
        Ok(())
        // @cpt-end:cpt-cf-bss-rating-algo-intake-capture:p1:inst-cap-conflict
    }

    /// An entry whose id or tenant cannot be read: its bytes go to the capture rejects and one
    /// `unidentifiable_entry` loss over the source scope opens (or is reused for the page).
    async fn capture_unidentifiable(
        &self,
        tx: &DbTx<'_>,
        checkpoint: &Checkpoint,
        page: &PreparedPage,
        raw: &Value,
        report: &mut PageReport,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-algo-intake-capture:p1:inst-cap-unidentifiable
        // A replay of an entry already set aside has no effect: its loss is open already.
        if self
            .parts
            .records
            .capture_rejected(tx, checkpoint.source_id, &entry::reject_digest(raw))
            .await?
        {
            return Ok(());
        }
        let now = self.parts.clock.now();
        let definition = &checkpoint.definition;
        let loss = NewSourceLoss {
            loss_id: Uuid::now_v7(),
            tenant_id: definition.tenant_scope().owner(),
            source_id: checkpoint.source_id,
            tenant_scope: definition.tenant_scope().clone(),
            kind: LossKind::UnidentifiableEntry,
            meters: definition.subscription().meters().to_vec(),
            detected_at: now,
            last_good_cursor: checkpoint.cursor.clone(),
            last_good_at: Some(checkpoint.updated_at),
            restart_cursor: Some(page.next.clone()),
            tainted: unidentifiable_taint(
                definition.subscription().meters(),
                now,
                self.parts.retention_floor,
            ),
        };
        let owner_scope = tenant_scope(page, definition.tenant_scope().owner())?;
        let opened = self.parts.losses.open(tx, owner_scope, &loss).await?;
        let kind = CaptureRejectKind::Unidentifiable {
            loss_id: opened.loss_id,
        };
        if self.set_aside(tx, checkpoint, raw, kind).await? {
            report.unidentifiable += 1;
            report.losses_opened.push(LossKind::UnidentifiableEntry);
            tracing::warn!(
                target: "bss_rating.usage",
                source = %checkpoint.source_id,
                loss_id = %opened.loss_id,
                code = "usage_unidentifiable",
                "usage entry without a readable id or tenant; captured as a reject, source loss open"
            );
        }
        Ok(())
        // @cpt-end:cpt-cf-bss-rating-algo-intake-capture:p1:inst-cap-unidentifiable
    }

    /// An entry of a meter or tenant the source does not read: the collector sent it outside the
    /// subscription. It is kept among the capture rejects with an alarm, and written to no
    /// tenant's records; nothing of the source is lost.
    async fn capture_outside(
        &self,
        tx: &DbTx<'_>,
        checkpoint: &Checkpoint,
        raw: &Value,
        report: &mut PageReport,
    ) -> Result<(), DomainError> {
        if self
            .set_aside(tx, checkpoint, raw, CaptureRejectKind::OutsideSubscription)
            .await?
        {
            report.outside_subscription += 1;
            tracing::warn!(
                target: "bss_rating.usage",
                source = %checkpoint.source_id,
                code = "usage_outside_subscription",
                "the feed returned an entry of a meter or tenant the source does not read; captured as a reject"
            );
        }
        Ok(())
    }

    /// Stores `raw` among the source's capture rejects; `false` when a replay already did.
    async fn set_aside(
        &self,
        tx: &DbTx<'_>,
        checkpoint: &Checkpoint,
        raw: &Value,
        kind: CaptureRejectKind,
    ) -> Result<bool, DomainError> {
        let reject = CaptureReject {
            source_id: checkpoint.source_id,
            raw_sha256: entry::reject_digest(raw),
            raw_entry: entry::bounded(raw).value,
            captured_at: self.parts.clock.now(),
            kind,
        };
        self.parts.records.insert_capture_reject(tx, &reject).await
    }

    /// Flow B step 2: withdraw the invalidation's target in the invalidation's own tenant.
    async fn apply_invalidation(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        invalidation: EntryIdentity,
        reference: &InvalidationRef,
        report: &mut PageReport,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-flow-intake-invalidation:p1:inst-iv-capture
        // @cpt-begin:cpt-cf-bss-rating-flow-intake-invalidation:p1:inst-iv-applied
        // The invalidation was stored `applied` by its capture insert, whatever its target's fate.
        report.invalidations_applied += 1;
        // @cpt-end:cpt-cf-bss-rating-flow-intake-invalidation:p1:inst-iv-applied
        // @cpt-end:cpt-cf-bss-rating-flow-intake-invalidation:p1:inst-iv-capture
        let target = target_of(invalidation, reference);
        let withdrawn = self
            .withdraw_target(tx, scope, target, invalidation.usage_record_id)
            .await?;
        // @cpt-begin:cpt-cf-bss-rating-flow-intake-invalidation:p1:inst-iv-unknown
        if !withdrawn {
            report.targets_unknown += 1;
            let key = exception_key(target, UsageException::TargetUnknown);
            self.parts
                .exceptions
                .record(
                    tx,
                    scope,
                    &key,
                    "an invalidation names this record, which is not captured yet",
                )
                .await?;
        }
        // @cpt-end:cpt-cf-bss-rating-flow-intake-invalidation:p1:inst-iv-unknown
        // @cpt-begin:cpt-cf-bss-rating-flow-intake-invalidation:p1:inst-iv-return
        // Rating computes no delta: a correction becomes a new revision downstream.
        Ok(())
        // @cpt-end:cpt-cf-bss-rating-flow-intake-invalidation:p1:inst-iv-return
    }

    /// Withdraws `target` by `invalidation` if it is stored: `false` when it is not. A target
    /// already withdrawn (a replay) or itself an invalidation is left as it is.
    async fn withdraw_target(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        target: EntryIdentity,
        invalidation: Uuid,
    ) -> Result<bool, DomainError> {
        let records = &self.parts.records;
        // @cpt-begin:cpt-cf-bss-rating-flow-intake-invalidation:p1:inst-iv-withdraw
        let Some(stored) = records.find(tx, scope, target).await? else {
            return Ok(false);
        };
        if stored.withdrawn_by.is_some() || stored.state == ContributionState::Applied {
            return Ok(true);
        }
        // @cpt-begin:cpt-cf-bss-rating-state-intake-capture:p1:inst-st-withdraw
        let moved = records
            .withdraw(
                tx,
                scope,
                target,
                stored.state,
                invalidation,
                self.parts.clock.now(),
            )
            .await?;
        // @cpt-end:cpt-cf-bss-rating-state-intake-capture:p1:inst-st-withdraw
        if !moved {
            return Err(DomainError::Concurrent(format!(
                "usage record {} changed state while it was withdrawn",
                target.usage_record_id
            )));
        }
        // @cpt-end:cpt-cf-bss-rating-flow-intake-invalidation:p1:inst-iv-withdraw
        // @cpt-begin:cpt-cf-bss-rating-flow-intake-invalidation:p1:inst-iv-decrement
        // Every state decides what its withdrawal undoes, so a state added later must decide too.
        match stored.state {
            // Nothing was derived from a captured record yet; withdrawn and applied rows are left
            // as they are above, as the conditional withdrawal never moves them.
            ContributionState::Captured
            | ContributionState::Withdrawn
            | ContributionState::Applied => {}
            ContributionState::Rejected => {
                if let Some(reason) = stored.reject_reason {
                    self.resolve(
                        tx,
                        scope,
                        target,
                        UsageException::Rejected(reason),
                        Resolution::Withdrawn,
                    )
                    .await?;
                }
            }
            // Feature 05's states: its contribution step undoes what it did (the counter
            // decrement, the exception it opened for a deferred state).
            ContributionState::Counted
            | ContributionState::AwaitingAttribution
            | ContributionState::AwaitingSpec
            | ContributionState::BoundarySplitRequired
            | ContributionState::Quarantined => {
                self.parts
                    .contributions
                    .withdraw(tx, scope, target, stored.state, invalidation)
                    .await?;
            }
        }
        // @cpt-end:cpt-cf-bss-rating-flow-intake-invalidation:p1:inst-iv-decrement
        Ok(true)
    }

    /// Opens (or counts one more attempt on) the rejection exception of `identity`, with an alarm
    /// for a natural-key conflict.
    async fn record_rejection(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        identity: EntryIdentity,
        rejection: &Rejection,
    ) -> Result<(), DomainError> {
        if rejection.reason == RejectReason::NaturalKeyConflict {
            tracing::warn!(
                target: "bss_rating.usage",
                tenant_id = %identity.tenant_id,
                usage_record_id = %identity.usage_record_id,
                code = rejection.reason.as_str(),
                detail = %rejection.detail,
                "usage entry with a new id holds a stored entry's natural key; the stored entry stands"
            );
        }
        let key = exception_key(identity, UsageException::Rejected(rejection.reason));
        self.parts
            .exceptions
            .record(tx, scope, &key, &rejection.detail)
            .await?;
        Ok(())
    }

    async fn resolve(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        identity: EntryIdentity,
        exception: UsageException,
        how: Resolution,
    ) -> Result<(), DomainError> {
        let key = exception_key(identity, exception);
        self.parts
            .exceptions
            .resolve(tx, scope, &key, how.as_str())
            .await?;
        Ok(())
    }

    /// The restart transaction after `InvalidArgument(CursorBeyondRetention)` for a read after
    /// `expected` (feature 04 §2.1 step 3, §3.4): under the same lock and cursor CAS as a page, one
    /// `cursor_beyond_retention` loss opens and the cursor resets to `Oldest`.
    ///
    /// # Errors
    ///
    /// A storage failure; the transaction rolls back and the read is retried.
    pub async fn restart_after_retention_loss(
        &self,
        db: &Db,
        source: &SourceDefinition,
        expected: Option<FeedCursor>,
        owner_scope: AccessScope,
    ) -> Result<RestartOutcome, DomainError> {
        let intake = self.clone();
        let source = source.clone();
        db.transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                intake
                    .restart_in(tx, &source, expected.as_ref(), &owner_scope)
                    .await
            })
        })
        .await
    }

    async fn restart_in(
        &self,
        tx: &DbTx<'_>,
        source: &SourceDefinition,
        expected: Option<&FeedCursor>,
        owner_scope: &AccessScope,
    ) -> Result<RestartOutcome, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-open-loss
        let Some(checkpoint) = self.locked_checkpoint(tx, source, expected).await? else {
            return Ok(RestartOutcome::CursorMoved);
        };
        let now = self.parts.clock.now();
        let loss = NewSourceLoss {
            loss_id: Uuid::now_v7(),
            tenant_id: source.tenant_scope().owner(),
            source_id: checkpoint.source_id,
            tenant_scope: source.tenant_scope().clone(),
            kind: LossKind::CursorBeyondRetention,
            meters: source.subscription().meters().to_vec(),
            detected_at: now,
            last_good_cursor: checkpoint.cursor.clone(),
            last_good_at: Some(checkpoint.updated_at),
            restart_cursor: None,
            tainted: retention_taint(
                source.subscription().meters(),
                checkpoint.updated_at,
                now,
                self.parts.retention_floor,
            ),
        };
        let opened = self.parts.losses.open(tx, owner_scope, &loss).await?;
        self.parts
            .checkpoints
            .set_cursor(tx, checkpoint.source_id, None, now)
            .await?;
        Ok(RestartOutcome::Restarted(opened))
        // @cpt-end:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-open-loss
    }

    /// The re-interpretation of a rejected entry from its stored raw entry, never the collector
    /// (feature 04 §4.1 `inst-st-retry`; the operator route `POST /exceptions/{id}:retry`, audited
    /// as `usage × retry`, is feature 11's). `declarations` must be read afresh, past the cache.
    ///
    /// # Errors
    ///
    /// [`DomainError::NotFound`] when the row is not stored within `scope`; a storage failure;
    /// [`DomainError::Concurrent`] when the row changed state after it was read, or a concurrent
    /// natural-key writer won.
    pub async fn retry_rejected(
        &self,
        db: &Db,
        scope: AccessScope,
        identity: EntryIdentity,
        declarations: PageDeclarations,
    ) -> Result<RetryOutcome, DomainError> {
        let intake = self.clone();
        db.transaction_ref_mapped(move |tx| {
            Box::pin(async move { intake.retry_in(tx, &scope, identity, &declarations).await })
        })
        .await
    }

    async fn retry_in(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        identity: EntryIdentity,
        declarations: &PageDeclarations,
    ) -> Result<RetryOutcome, DomainError> {
        let records = &self.parts.records;
        let stored = records
            .find(tx, scope, identity)
            .await?
            .ok_or(DomainError::NotFound {
                what: "usage record",
            })?;
        if stored.state != ContributionState::Rejected || stored.withdrawn_by.is_some() {
            return Ok(RetryOutcome::NotRejected(stored.state));
        }
        let changed = || {
            DomainError::Concurrent(format!(
                "usage record {} changed state during its retry",
                identity.usage_record_id
            ))
        };
        // @cpt-begin:cpt-cf-bss-rating-state-intake-capture:p1:inst-st-retry
        let normalized = match self
            .normalized(tx, scope, &stored.raw_entry, identity, declarations)
            .await?
        {
            Ok(normalized) => normalized,
            Err(rejection) => {
                if !records
                    .reject_again(tx, scope, identity, rejection.reason)
                    .await?
                {
                    return Err(changed());
                }
                // The row now stands rejected for this reason alone: an exception for another
                // reason would stay open with nothing left to retry it.
                if let Some(previous) = stored.reject_reason
                    && previous != rejection.reason
                {
                    self.resolve(
                        tx,
                        scope,
                        identity,
                        UsageException::Rejected(previous),
                        Resolution::Retried,
                    )
                    .await?;
                }
                self.record_rejection(tx, scope, identity, &rejection)
                    .await?;
                return Ok(RetryOutcome::RejectedAgain(rejection));
            }
        };
        let to = interpreted_state(&normalized);
        if !records.complete_retry(tx, scope, &normalized, to).await? {
            return Err(changed());
        }
        if let Some(reason) = stored.reject_reason {
            self.resolve(
                tx,
                scope,
                identity,
                UsageException::Rejected(reason),
                Resolution::Retried,
            )
            .await?;
        }
        // @cpt-end:cpt-cf-bss-rating-state-intake-capture:p1:inst-st-retry
        match &normalized.kind {
            EntryKind::Invalidation(reference) => {
                let mut report = PageReport::default();
                self.apply_invalidation(tx, scope, identity, reference, &mut report)
                    .await?;
            }
            EntryKind::Record => {
                self.parts
                    .contributions
                    .interpret(tx, scope, &normalized)
                    .await?;
            }
        }
        Ok(RetryOutcome::Interpreted)
    }
}

impl UsageIntake {
    /// Reconciles the stored usage sources with `configured` ones under configuration
    /// `generation`, in one transaction (feature 04 §3.3 `inst-src-change`, `inst-src-widened`),
    /// and returns the sources to read: every one not retired. `owner_scopes` holds the service
    /// scope of each configured source's owner root.
    ///
    /// The stored rows are locked first, so two instances reconcile one after the other. A
    /// configuration older than the one the sources were last reconciled under changes nothing:
    /// an instance still on it mid-rollout reads the stored sources instead of reverting them.
    ///
    /// # Errors
    ///
    /// A storage failure; nothing is changed and the caller retries.
    pub async fn reconcile_sources(
        &self,
        db: &Db,
        configured: Vec<SourceDefinition>,
        generation: u64,
        owner_scopes: BTreeMap<Uuid, AccessScope>,
    ) -> Result<Vec<Checkpoint>, DomainError> {
        let intake = self.clone();
        db.transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                intake
                    .reconcile_in(tx, &configured, generation, &owner_scopes)
                    .await
            })
        })
        .await
    }

    async fn reconcile_in(
        &self,
        tx: &DbTx<'_>,
        configured: &[SourceDefinition],
        generation: u64,
        owner_scopes: &BTreeMap<Uuid, AccessScope>,
    ) -> Result<Vec<Checkpoint>, DomainError> {
        let checkpoints = &self.parts.checkpoints;
        let now = self.parts.clock.now();
        let stored = checkpoints.lock_usage(tx).await?;
        // @cpt-begin:cpt-cf-bss-rating-algo-intake-source-identity:p1:inst-src-change
        let applied = stored.iter().map(|row| row.config_generation).max();
        if let Some(applied) = applied.filter(|applied| *applied > generation) {
            tracing::warn!(
                target: "bss_rating.usage",
                configured = generation,
                applied,
                "this instance's usage_feed.config_generation is older than the one the sources were reconciled under; reading the stored sources unchanged"
            );
            return Ok(not_retired(stored));
        }
        let plan = plan_sources(configured, &stored);
        self.create_sources(tx, &plan, generation, now).await?;
        self.move_sources(tx, &plan, now).await?;
        checkpoints.stamp_generation(tx, generation).await?;
        // @cpt-end:cpt-cf-bss-rating-algo-intake-source-identity:p1:inst-src-change
        for (source, exposed) in &plan.widened {
            self.open_widened(tx, configured, *source, exposed, owner_scopes, now)
                .await?;
        }
        Ok(not_retired(checkpoints.lock_usage(tx).await?))
    }

    /// Creates the plan's new sources, `active` and reading from `Oldest`.
    async fn create_sources(
        &self,
        tx: &DbTx<'_>,
        plan: &SourcePlan,
        generation: u64,
        now: time::OffsetDateTime,
    ) -> Result<(), DomainError> {
        for definition in &plan.create {
            let checkpoint = Checkpoint {
                source_id: definition.source_id(),
                definition: definition.clone(),
                cursor: None,
                state: SourceState::Active,
                updated_at: now,
                config_generation: generation,
            };
            self.parts.checkpoints.insert(tx, &checkpoint).await?;
            tracing::info!(target: "bss_rating.usage", source = %checkpoint.source_id, "usage source created; reading from Oldest");
        }
        Ok(())
    }

    /// Applies the plan's state changes: configured sources active again, unconfigured ones
    /// draining, or retired when their scope was narrowed on purpose.
    async fn move_sources(
        &self,
        tx: &DbTx<'_>,
        plan: &SourcePlan,
        now: time::OffsetDateTime,
    ) -> Result<(), DomainError> {
        let moves = [
            (&plan.activate, SourceState::Draining, SourceState::Active),
            (&plan.activate, SourceState::Retired, SourceState::Active),
            (&plan.drain, SourceState::Active, SourceState::Draining),
            (&plan.retire, SourceState::Active, SourceState::Retired),
        ];
        for (ids, from, to) in moves {
            for source in ids {
                let transition = Transition { from, to };
                if self
                    .parts
                    .checkpoints
                    .set_state(tx, *source, transition, now)
                    .await?
                {
                    tracing::info!(target: "bss_rating.usage", source = %source, state = to.as_str(), "usage source state changed");
                }
            }
        }
        Ok(())
    }

    /// One `scope_widened_beyond_retention` loss on the created `source`, against the tenants it
    /// exposes: `Oldest` is already past the retention floor for their history. Its tainted days
    /// are those before `day(detected_at − F)` that a stored fact of those tenants covers; facts
    /// arrive with feature 06, so it opens with none and an operator accepts it.
    async fn open_widened(
        &self,
        tx: &DbTx<'_>,
        configured: &[SourceDefinition],
        source: SourceId,
        exposed: &TenantScope,
        owner_scopes: &BTreeMap<Uuid, AccessScope>,
        now: time::OffsetDateTime,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-algo-intake-source-identity:p1:inst-src-widened
        let Some(definition) = configured.iter().find(|d| d.source_id() == source) else {
            return Ok(());
        };
        let owner = definition.tenant_scope().owner();
        let scope = owner_scopes.get(&owner).ok_or_else(|| {
            DomainError::Internal(format!("no service scope is bound for {owner}"), None)
        })?;
        let loss = NewSourceLoss {
            loss_id: Uuid::now_v7(),
            tenant_id: owner,
            source_id: source,
            tenant_scope: exposed.clone(),
            kind: LossKind::ScopeWidenedBeyondRetention,
            meters: definition.subscription().meters().to_vec(),
            detected_at: now,
            last_good_cursor: None,
            last_good_at: None,
            restart_cursor: None,
            tainted: TaintedDays::none(),
        };
        let opened = self.parts.losses.open(tx, scope, &loss).await?;
        tracing::warn!(
            target: "bss_rating.usage",
            source = %source,
            loss_id = %opened.loss_id,
            code = LossKind::ScopeWidenedBeyondRetention.as_str(),
            "a widened scope exposes tenants whose history before the retention floor is gone; source loss open"
        );
        Ok(())
        // @cpt-end:cpt-cf-bss-rating-algo-intake-source-identity:p1:inst-src-widened
    }
}

/// The 15-minute check of `invalidation_target_unknown` (feature 04 §4.5): a target captured
/// since is withdrawn by its invalidation and the exception resolved. Capture already withdraws a
/// late target in its own page, so this is the safety net for a target stored any other way.
#[domain_model]
#[derive(Clone)]
pub struct TargetUnknownRetrier {
    intake: UsageIntake,
}

impl TargetUnknownRetrier {
    /// The retrier over `intake`.
    #[must_use]
    pub const fn new(intake: UsageIntake) -> Self {
        Self { intake }
    }
}

#[async_trait::async_trait]
impl ExceptionRetrier for TargetUnknownRetrier {
    fn subject_kind(&self) -> SubjectKind {
        SubjectKind::UsageRecord
    }

    fn retries(&self, reason_code: &str) -> bool {
        reason_code == TARGET_UNKNOWN
    }

    /// # Errors
    ///
    /// [`DomainError::CorruptRow`] when the exception's subject is not a usage record id: only
    /// capture opens this exception, always on a record id, so another subject is a corrupt row,
    /// and the sweeper counts it as failed rather than retried.
    async fn retry(
        &self,
        db: &Db,
        scope: &AccessScope,
        exception: &ExceptionRecord,
    ) -> Result<(), DomainError> {
        let subject = exception.key.subject_ref();
        let target = Uuid::parse_str(subject).map_err(|_| {
            DomainError::CorruptRow(format!(
                "exception {}: {TARGET_UNKNOWN} names {subject:?}, not a usage record id",
                exception.exception_id
            ))
        })?;
        let identity = EntryIdentity {
            tenant_id: exception.key.tenant_id(),
            usage_record_id: target,
        };
        let intake = self.intake.clone();
        let scope = scope.clone();
        db.transaction_ref_mapped(move |tx| {
            Box::pin(async move { intake.recheck_target(tx, &scope, identity).await })
        })
        .await
    }
}

impl UsageIntake {
    async fn recheck_target(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        target: EntryIdentity,
    ) -> Result<(), DomainError> {
        let records = &self.parts.records;
        let Some(invalidation) = records
            .invalidation_naming(tx, scope, target, target.usage_record_id)
            .await?
        else {
            return Ok(());
        };
        if self
            .withdraw_target(tx, scope, target, invalidation)
            .await?
        {
            self.resolve(
                tx,
                scope,
                target,
                UsageException::TargetUnknown,
                Resolution::Withdrawn,
            )
            .await?;
        }
        Ok(())
    }
}

/// Whether `raw` digests, under the stored row's version, to the stored digest. A version this
/// build does not know cannot be compared and counts as different.
fn same_digest(stored: &StoredDigest, raw: &Value) -> bool {
    stored
        .raw_digest_version
        .is_some_and(|version| entry::digest(raw, version) == stored.raw_sha256)
}

fn exception_key(identity: EntryIdentity, exception: UsageException) -> ExceptionKey {
    ExceptionKey::new(
        identity.tenant_id,
        SubjectKind::UsageRecord,
        SubjectRef::from(identity.usage_record_id),
        exception.reason(),
    )
}

/// The record an invalidation names, in the invalidation's own tenant.
const fn target_of(invalidation: EntryIdentity, reference: &InvalidationRef) -> EntryIdentity {
    EntryIdentity {
        tenant_id: invalidation.tenant_id,
        usage_record_id: reference.target,
    }
}

/// The stored sources a reader runs for.
fn not_retired(checkpoints: Vec<Checkpoint>) -> Vec<Checkpoint> {
    checkpoints
        .into_iter()
        .filter(|checkpoint| checkpoint.state != SourceState::Retired)
        .collect()
}

/// The service scope bound for `tenant` when the page was prepared.
fn tenant_scope(page: &PreparedPage, tenant: Uuid) -> Result<&AccessScope, DomainError> {
    page.scopes.get(&tenant).ok_or_else(|| {
        DomainError::Internal(
            format!("no service scope is bound for tenant {tenant}"),
            None,
        )
    })
}

#[cfg(test)]
#[path = "intake_tests.rs"]
mod tests;
