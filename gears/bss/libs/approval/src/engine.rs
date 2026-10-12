//! Approval transitions inside the transaction opened by the gear.
//!
//! Return every error to that transaction so all preceding writes roll back.
//! [`ApproveOutcome::Refreshed`] and [`RejectOutcome::Refreshed`] are successes: commit before
//! mapping them to the wire `UNIT_STALE` response.
//!
//! Every entry point takes an [`InTransaction`] runner: its writes (the unit, its locks, the vote,
//! the state) are one unit of work only inside the caller's transaction, so an autocommit runner
//! does not compile.
use crate::hash::snapshot_hash;
use crate::model::{
    ApprovalError, Decision, ItemRef, NOTE_MAX_CHARS, Policy, Unit, UnitState, Verdict,
};
use crate::rules::{ApproveStep, already_voted, evaluate_approve};
use crate::store::Store;
use crate::subject::ApprovalSubject;
use time::{Date, OffsetDateTime};
use toolkit_db::secure::{DBRunner, DbTx, SecureTx};
use uuid::Uuid;

mod sealed {
    pub trait Sealed {}
    impl Sealed for toolkit_db::secure::DbTx<'_> {}
    impl Sealed for toolkit_db::secure::SecureTx<'_> {}
}
/// A runner inside an open transaction: [`DbTx`] or [`SecureTx`], never a connection. Sealed:
/// the engine's multi-write sequences roll back as one only on such a runner.
pub trait InTransaction: DBRunner + Sync + sealed::Sealed {}
impl InTransaction for DbTx<'_> {}
impl InTransaction for SecureTx<'_> {}

/// Inputs captured by the gear for one submission.
pub struct SubmitRequest<'a> {
    pub tenant_id: Uuid,
    pub ref_id: Uuid,
    pub item_ids: &'a [Uuid],
    pub actor: Uuid,
    pub policy: &'a Policy,
    pub common_effective_date: Option<Date>,
    /// The submitter's note, stored on the unit as [`Unit::submit_note`]; not a fingerprint input.
    pub note: Option<&'a str>,
    pub now: OffsetDateTime,
}

/// The recorded unit and whether submission immediately applied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submitted {
    pub unit: Unit,
    pub applied: bool,
}

/// A successful approval transaction; every variant must be committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApproveOutcome {
    Pending { have: u32, need: u32 },
    Applied,
    Refreshed { generation: i32 },
}

/// A successful rejection transaction; both variants must be committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectOutcome {
    /// The unit is rejected and its locks are released.
    Rejected,
    /// The content changed since the reviewer's generation: the unit is refreshed to
    /// `generation`, still pending with its locks, and no vote is recorded.
    Refreshed { generation: i32 },
}

/// Stateless coordinator; it opens no connection or transaction.
pub struct Engine;

impl Engine {
    /// validate → insert → lock → (quorum 0: apply, unlock as approved, state). One transaction: the caller's.
    ///
    /// # Errors
    /// Returns empty-input, validation, lock, application or persistence errors.
    // NOT cancel-safe: a drop between its writes relies on the caller's transaction rolling back.
    pub async fn submit<R: InTransaction, S: Store<R>, B: ApprovalSubject<R>>(
        store: &S,
        subject: &B,
        runner: &R,
        req: SubmitRequest<'_>,
    ) -> Result<Submitted, ApprovalError> {
        if req.item_ids.is_empty() {
            return Err(ApprovalError::Empty);
        }
        let items = subject.collect(runner, req.item_ids).await?;
        subject.validate_submit(runner, &items).await?;
        let quorum = req.policy.quorum_for(subject.kind());
        let mut unit = Unit {
            id: Uuid::new_v4(),
            tenant_id: req.tenant_id,
            kind: subject.kind().to_owned(),
            ref_type: subject.ref_type().to_owned(),
            ref_id: req.ref_id,
            state: UnitState::Pending,
            common_effective_date: req.common_effective_date,
            quorum_required: quorum,
            generation: 1,
            submitted_by: req.actor,
            submitted_at: req.now,
            submit_note: req.note.map(str::to_owned),
            decided_at: None,
            decided_note: None,
            snapshot: subject.snapshot(&items, req.common_effective_date),
            snapshot_hash: snapshot_hash(&items, req.common_effective_date),
            version: 1,
        };
        store.insert_unit(runner, &unit, &items).await?;
        subject.lock(runner, unit.id, &items).await?;
        if quorum == 0 {
            subject.apply(runner, &unit, &items).await?;
            subject.unlock(runner, &unit, &items, true).await?;
            store
                .set_state(runner, unit.id, UnitState::Approved, Some(req.now), None)
                .await?;
            unit.state = UnitState::Approved;
            unit.decided_at = Some(req.now);
            return Ok(Submitted {
                unit,
                applied: true,
            });
        }
        Ok(Submitted {
            unit,
            applied: false,
        })
    }

    /// note length → load → pending? → bump version (or `Contended`) → generation seen? → separation of duties → duplicate → fingerprint → vote → quorum → apply.
    ///
    /// `seen_generation` must match the reviewed generation. A mismatch returns
    /// an error before voting; the earlier version bump rolls back with it.
    ///
    /// # Errors
    /// Returns note-length, not-found, state, contention, generation, separation of duties,
    /// duplicate, application or store errors.
    #[expect(
        clippy::too_many_arguments,
        reason = "The approval command carries the reviewer and generation explicitly"
    )]
    // NOT cancel-safe: a drop between its writes relies on the caller's transaction rolling back.
    pub async fn approve<R: InTransaction, S: Store<R>, B: ApprovalSubject<R>>(
        store: &S,
        subject: &B,
        runner: &R,
        unit_id: Uuid,
        actor: Uuid,
        seen_generation: i32,
        note: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<ApproveOutcome, ApprovalError> {
        check_note_length(note)?;
        let unit = Self::load_pending(store, runner, unit_id).await?;
        check_generation(&unit, seen_generation)?;
        let items = store.items(runner, unit_id).await?;
        let decisions = store.decisions(runner, unit_id).await?;
        let step = evaluate_approve(&unit, &decisions, actor, &items)?;
        if let Some(generation) =
            Self::refresh_if_stale(store, subject, runner, &unit, &items).await?
        {
            return Ok(ApproveOutcome::Refreshed { generation });
        }
        store
            .insert_decision(
                runner,
                &Decision {
                    unit_id,
                    actor,
                    generation: unit.generation,
                    verdict: Verdict::Approve,
                    note: note.map(str::to_owned),
                    at: now,
                    stale: false,
                },
            )
            .await?;
        match step {
            ApproveStep::NeedMore { have, need } => Ok(ApproveOutcome::Pending { have, need }),
            ApproveStep::Apply => {
                subject.apply(runner, &unit, &items).await?;
                subject.unlock(runner, &unit, &items, true).await?;
                store
                    .set_state(runner, unit_id, UnitState::Approved, Some(now), None)
                    .await?;
                Ok(ApproveOutcome::Applied)
            }
        }
    }

    /// Closes a pending unit with one rejection and a nonblank note, unless its content changed
    /// since the reviewer's generation: then it refreshes the unit as [`Engine::approve`] does
    /// and records no vote.
    ///
    /// blank note → note length → load → pending? → bump version (or `Contended`) → generation
    /// seen? → fingerprint → duplicate → vote → unlock → rejected. The fingerprint comes before
    /// the duplicate check, so a reviewer who already voted in a stale generation is answered
    /// with the refresh.
    ///
    /// # Errors
    /// Returns missing-note, note-length, not-found, state, contention, generation, duplicate or
    /// store errors.
    #[expect(
        clippy::too_many_arguments,
        reason = "The rejection command carries the reviewer, generation and required note"
    )]
    // NOT cancel-safe: a drop between its writes relies on the caller's transaction rolling back.
    pub async fn reject<R: InTransaction, S: Store<R>, B: ApprovalSubject<R>>(
        store: &S,
        subject: &B,
        runner: &R,
        unit_id: Uuid,
        actor: Uuid,
        seen_generation: i32,
        note: &str,
        now: OffsetDateTime,
    ) -> Result<RejectOutcome, ApprovalError> {
        if note.trim().is_empty() {
            return Err(ApprovalError::NoteRequired);
        }
        check_note_length(Some(note))?;
        let unit = Self::load_pending(store, runner, unit_id).await?;
        check_generation(&unit, seen_generation)?;
        let items = store.items(runner, unit_id).await?;
        if let Some(generation) =
            Self::refresh_if_stale(store, subject, runner, &unit, &items).await?
        {
            return Ok(RejectOutcome::Refreshed { generation });
        }
        let decisions = store.decisions(runner, unit_id).await?;
        if already_voted(&unit, &decisions, actor) {
            return Err(ApprovalError::DuplicateVote);
        }
        store
            .insert_decision(
                runner,
                &Decision {
                    unit_id,
                    actor,
                    generation: unit.generation,
                    verdict: Verdict::Reject,
                    note: Some(note.to_owned()),
                    at: now,
                    stale: false,
                },
            )
            .await?;
        subject.unlock(runner, &unit, &items, false).await?;
        store
            .set_state(runner, unit_id, UnitState::Rejected, Some(now), Some(note))
            .await?;
        Ok(RejectOutcome::Rejected)
    }

    /// Lets the submitter close a pending unit and release its locks.
    ///
    /// # Errors
    /// Returns not-found, state, contention, wrong-submitter or persistence errors.
    // NOT cancel-safe: a drop between its writes relies on the caller's transaction rolling back.
    pub async fn withdraw<R: InTransaction, S: Store<R>, B: ApprovalSubject<R>>(
        store: &S,
        subject: &B,
        runner: &R,
        unit_id: Uuid,
        actor: Uuid,
        now: OffsetDateTime,
    ) -> Result<(), ApprovalError> {
        let unit = Self::load_pending(store, runner, unit_id).await?;
        if unit.submitted_by != actor {
            return Err(ApprovalError::NotSubmitter);
        }
        let items = store.items(runner, unit_id).await?;
        subject.unlock(runner, &unit, &items, false).await?;
        store
            .set_state(runner, unit_id, UnitState::Withdrawn, Some(now), None)
            .await
    }

    /// Refreshes the unit when the items' current content no longer has its fingerprint: the
    /// items, snapshot and fingerprint are rewritten at the next generation, which it returns.
    async fn refresh_if_stale<R: InTransaction, S: Store<R>, B: ApprovalSubject<R>>(
        store: &S,
        subject: &B,
        runner: &R,
        unit: &Unit,
        items: &[ItemRef],
    ) -> Result<Option<i32>, ApprovalError> {
        let fresh_items = subject
            .collect(runner, &items.iter().map(|i| i.item_id).collect::<Vec<_>>())
            .await?;
        let fresh_hash = snapshot_hash(&fresh_items, unit.common_effective_date);
        if fresh_hash == unit.snapshot_hash {
            return Ok(None);
        }
        let generation = unit.generation.saturating_add(1);
        let snapshot = subject.snapshot(&fresh_items, unit.common_effective_date);
        store
            .refresh(
                runner,
                unit.id,
                &fresh_items,
                &snapshot,
                &fresh_hash,
                generation,
            )
            .await?;
        Ok(Some(generation))
    }

    /// The unit as of now, still pending, with its version bumped so a concurrent writer loses.
    async fn load_pending<R: InTransaction, S: Store<R>>(
        store: &S,
        runner: &R,
        unit_id: Uuid,
    ) -> Result<Unit, ApprovalError> {
        let unit = store
            .unit(runner, unit_id)
            .await?
            .ok_or(ApprovalError::UnitNotFound { unit_id })?;
        if unit.state != UnitState::Pending {
            return Err(ApprovalError::AlreadyDecided);
        }
        if !store.bump_version(runner, unit_id, unit.version).await? {
            return Err(ApprovalError::Contended);
        }
        Ok(Unit {
            version: unit.version + 1,
            ..unit
        })
    }
}

/// The vote names the unit's current generation.
const fn check_generation(unit: &Unit, seen: i32) -> Result<(), ApprovalError> {
    if unit.generation == seen {
        Ok(())
    } else {
        Err(ApprovalError::GenerationMismatch {
            seen,
            current: unit.generation,
        })
    }
}

/// A vote's note is at most [`NOTE_MAX_CHARS`] characters (Unicode scalar values).
fn check_note_length(note: Option<&str>) -> Result<(), ApprovalError> {
    if note.is_some_and(|n| n.chars().count() > NOTE_MAX_CHARS) {
        Err(ApprovalError::NoteTooLong)
    } else {
        Ok(())
    }
}
