#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Real toolkit-db transaction/runner plumbing with nontransactional in-memory state.
//!
//! These fakes deliberately do not roll back. SQL rollback and competing writers
//! are covered by the SQL-backed gear store in phase 1c.
use bss_approval::{
    ApprovalError, ApprovalSubject, ApproveOutcome, Decision, Engine, ItemRef, Policy,
    RejectOutcome, Store, SubmitRequest, Unit, UnitState, approve_eligibility,
};
use parking_lot::Mutex;
use std::{collections::BTreeMap, sync::Arc};
use time::{Date, OffsetDateTime, macros::datetime};
use toolkit_db::secure::{DbTx, TxConfig};
use toolkit_db::{ConnectOpts, Db, DbError, connect_db};
use uuid::Uuid;

// ---- a real Db; the closure's runner type is DbTx<'a> (conv §1) ----
async fn db() -> Db {
    connect_db(
        "sqlite::memory:",
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}
#[derive(Debug)]
enum TxErr {
    Approval(ApprovalError),
    Db(DbError),
}
impl From<DbError> for TxErr {
    fn from(e: DbError) -> Self {
        Self::Db(e)
    }
}
fn no_retry(_: &TxErr) -> Option<&sea_orm::DbErr> {
    None
}

/// Runs `f` inside one transaction; `f` gets `&DbTx` and returns the engine's result.
async fn in_tx<T: Send + 'static>(
    db: &Db,
    mut f: impl for<'a> FnMut(
        &'a DbTx<'a>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, ApprovalError>> + Send + 'a>,
    > + Send,
) -> Result<T, ApprovalError> {
    db.transaction_with_retry::<T, TxErr, _, _>(TxConfig::default(), no_retry, move |tx| {
        let future = f(tx);
        Box::pin(async move { future.await.map_err(TxErr::Approval) })
    })
    .await
    .map_err(|e| match e {
        TxErr::Approval(a) => a,
        TxErr::Db(d) => ApprovalError::Store(d.to_string()),
    })
}

async fn unit_of(db: &Db, store: &Mem, id: Uuid) -> Unit {
    let store = store.clone();
    in_tx(db, move |tx| {
        let store = store.clone();
        Box::pin(async move { store.unit(tx, id).await.map(|unit| unit.unwrap()) })
    })
    .await
    .unwrap()
}

// ---- in-memory store: a map behind a mutex, typed over DbTx so the Engine generics resolve to the real runner ----
type Units = BTreeMap<Uuid, (Unit, Vec<ItemRef>)>;

#[derive(Default, Clone)]
struct Mem {
    units: Arc<Mutex<Units>>,
    decisions: Arc<Mutex<Vec<Decision>>>,
    steal_next_bump: Arc<Mutex<bool>>,
}

#[async_trait::async_trait]
impl<'a> Store<DbTx<'a>> for Mem {
    async fn insert_unit(
        &self,
        _: &DbTx<'a>,
        u: &Unit,
        items: &[ItemRef],
    ) -> Result<(), ApprovalError> {
        self.units.lock().insert(u.id, (u.clone(), items.to_vec()));
        Ok(())
    }
    async fn unit(&self, _: &DbTx<'a>, id: Uuid) -> Result<Option<Unit>, ApprovalError> {
        Ok(self.units.lock().get(&id).map(|(u, _)| u.clone()))
    }
    async fn bump_version(
        &self,
        _: &DbTx<'a>,
        id: Uuid,
        expected: i64,
    ) -> Result<bool, ApprovalError> {
        let mut steal = self.steal_next_bump.lock();
        if *steal {
            *steal = false;
            if let Some((u, _)) = self.units.lock().get_mut(&id) {
                u.version += 1;
            }
            return Ok(false);
        } // the other writer's bump lands, ours misses
        let mut g = self.units.lock();
        let Some((u, _)) = g.get_mut(&id) else {
            return Ok(false);
        };
        if u.version != expected {
            return Ok(false);
        }
        u.version += 1;
        Ok(true)
    }
    async fn items(&self, _: &DbTx<'a>, id: Uuid) -> Result<Vec<ItemRef>, ApprovalError> {
        Ok(self
            .units
            .lock()
            .get(&id)
            .map(|(_, i)| i.clone())
            .unwrap_or_default())
    }
    async fn decisions(&self, _: &DbTx<'a>, id: Uuid) -> Result<Vec<Decision>, ApprovalError> {
        Ok(self
            .decisions
            .lock()
            .iter()
            .filter(|d| d.unit_id == id)
            .cloned()
            .collect())
    }
    async fn insert_decision(&self, _: &DbTx<'a>, d: &Decision) -> Result<(), ApprovalError> {
        let mut g = self.decisions.lock();
        if g.iter()
            .any(|x| x.unit_id == d.unit_id && x.actor == d.actor && x.generation == d.generation)
        {
            return Err(ApprovalError::Store(
                "PK (unit_id, actor, generation)".into(),
            ));
        }
        g.push(d.clone());
        Ok(())
    }
    async fn refresh(
        &self,
        _: &DbTx<'a>,
        id: Uuid,
        items: &[ItemRef],
        snapshot: &serde_json::Value,
        hash: &str,
        generation: i32,
    ) -> Result<(), ApprovalError> {
        let mut g = self.units.lock();
        let (u, i) = g.get_mut(&id).unwrap();
        u.generation = generation;
        u.snapshot = snapshot.clone();
        hash.clone_into(&mut u.snapshot_hash);
        *i = items.to_vec();
        for d in self.decisions.lock().iter_mut() {
            if d.unit_id == id && d.generation < generation {
                d.stale = true;
            }
        }
        Ok(())
    }
    async fn set_state(
        &self,
        _: &DbTx<'a>,
        id: Uuid,
        s: UnitState,
        at: Option<OffsetDateTime>,
        note: Option<&str>,
    ) -> Result<(), ApprovalError> {
        let mut g = self.units.lock();
        let (u, _) = g.get_mut(&id).unwrap();
        u.state = s;
        u.decided_at = at;
        u.decided_note = note.map(str::to_owned);
        Ok(())
    }
}

type LiveRows = BTreeMap<Uuid, (Uuid, i64)>;
/// Each unlocked item with the `approved` flag of its unlock.
type Unlocks = Vec<(Uuid, bool)>;

/// Rows whose business content is one amount; the lock lives in a separate map so it is never part of `after`.
/// `unlocked` records every unlock the engine made, with its `approved` flag, in call order.
#[derive(Default, Clone)]
struct Rows {
    live: Arc<Mutex<LiveRows>>,
    locked: Arc<Mutex<BTreeMap<Uuid, Uuid>>>,
    unlocked: Arc<Mutex<Unlocks>>,
    applied: Arc<Mutex<Vec<Uuid>>>,
    refuse_apply: Arc<Mutex<bool>>,
}

#[async_trait::async_trait]
impl<'a> ApprovalSubject<DbTx<'a>> for Rows {
    fn kind(&self) -> &'static str {
        "prices"
    }
    fn ref_type(&self) -> &'static str {
        "book"
    }
    async fn collect(&self, _: &DbTx<'a>, ids: &[Uuid]) -> Result<Vec<ItemRef>, ApprovalError> {
        let live = self.live.lock();
        Ok(ids
            .iter()
            .map(|id| {
                let (author, amount) = live[id];
                ItemRef {
                    item_type: "price".into(),
                    item_id: *id,
                    created_by: author,
                    before: None,
                    after: serde_json::json!({ "amount": amount }),
                }
            })
            .collect())
    }
    async fn validate_submit(&self, _: &DbTx<'a>, items: &[ItemRef]) -> Result<(), ApprovalError> {
        if items.is_empty() {
            Err(ApprovalError::Empty)
        } else {
            Ok(())
        }
    }
    async fn lock(&self, _: &DbTx<'a>, unit: Uuid, items: &[ItemRef]) -> Result<(), ApprovalError> {
        let mut l = self.locked.lock();
        for i in items {
            if l.contains_key(&i.item_id) {
                return Err(ApprovalError::Locked {
                    item_type: i.item_type.clone(),
                    item_id: i.item_id,
                });
            }
        }
        for i in items {
            l.insert(i.item_id, unit);
        }
        Ok(())
    }
    fn snapshot(&self, items: &[ItemRef], _: Option<Date>) -> serde_json::Value {
        serde_json::json!({ "n": items.len() })
    }
    async fn apply(&self, _: &DbTx<'a>, _: &Unit, items: &[ItemRef]) -> Result<(), ApprovalError> {
        if *self.refuse_apply.lock() {
            return Err(ApprovalError::ApplyRefused {
                code: "SKU_NAME_TAKEN",
                detail: "taken meanwhile".into(),
            });
        }
        self.applied.lock().extend(items.iter().map(|i| i.item_id));
        Ok(())
    }
    async fn unlock(
        &self,
        _: &DbTx<'a>,
        _: &Unit,
        items: &[ItemRef],
        approved: bool,
    ) -> Result<(), ApprovalError> {
        let mut l = self.locked.lock();
        for i in items {
            l.remove(&i.item_id);
            self.unlocked.lock().push((i.item_id, approved));
        }
        Ok(())
    }
}

fn rows(author: Uuid, n: usize) -> (Rows, Vec<Uuid>) {
    let ids: Vec<Uuid> = (0..n).map(|_| Uuid::new_v4()).collect();
    let r = Rows::default();
    {
        let mut live = r.live.lock();
        for id in &ids {
            live.insert(*id, (author, 10));
        }
    }
    (r, ids)
}
const T0: OffsetDateTime = datetime!(2026-09-24 10:00 UTC);
const T1: OffsetDateTime = datetime!(2026-09-24 11:00 UTC);
fn policy(q: u32) -> Policy {
    Policy {
        default_quorum: q,
        overrides: BTreeMap::new(),
    }
}

async fn submit(
    db: &Db,
    store: &Mem,
    subject: &Rows,
    ids: Vec<Uuid>,
    actor: Uuid,
    q: u32,
) -> Result<bss_approval::Submitted, ApprovalError> {
    submit_noted(db, store, subject, ids, actor, q, None).await
}
/// [`submit`] with the submitter's note.
async fn submit_noted(
    db: &Db,
    store: &Mem,
    subject: &Rows,
    ids: Vec<Uuid>,
    actor: Uuid,
    q: u32,
    note: Option<&'static str>,
) -> Result<bss_approval::Submitted, ApprovalError> {
    let (store, subject) = (store.clone(), subject.clone());
    in_tx(db, move |tx| {
        let (store, subject, ids) = (store.clone(), subject.clone(), ids.clone());
        Box::pin(async move {
            Engine::submit(
                &store,
                &subject,
                tx,
                SubmitRequest {
                    tenant_id: Uuid::new_v4(),
                    ref_id: Uuid::new_v4(),
                    item_ids: &ids,
                    actor,
                    policy: &policy(q),
                    common_effective_date: None,
                    note,
                    now: T0,
                },
            )
            .await
        })
    })
    .await
}
async fn withdraw(db: &Db, store: &Mem, subject: &Rows, unit: Uuid, actor: Uuid) {
    let (store, subject) = (store.clone(), subject.clone());
    in_tx(db, move |tx| {
        let (store, subject) = (store.clone(), subject.clone());
        Box::pin(async move { Engine::withdraw(&store, &subject, tx, unit, actor, T1).await })
    })
    .await
    .unwrap();
}
async fn approve(
    db: &Db,
    store: &Mem,
    subject: &Rows,
    unit: Uuid,
    actor: Uuid,
) -> Result<ApproveOutcome, ApprovalError> {
    let (store, subject) = (store.clone(), subject.clone());
    in_tx(db, move |tx| {
        let (store, subject) = (store.clone(), subject.clone());
        Box::pin(async move {
            let g = store.unit(tx, unit).await?.map_or(1, |u| u.generation);
            Engine::approve(&store, &subject, tx, unit, actor, g, None, T1).await
        })
    })
    .await
}
#[tokio::test]
async fn a_vote_against_an_older_generation_is_refused() {
    let db = db().await;
    let author = Uuid::new_v4();
    let (subject, ids) = rows(author, 1);
    let store = Mem::default();
    let s = submit(&db, &store, &subject, ids.clone(), author, 2)
        .await
        .unwrap();
    subject.live.lock().get_mut(&ids[0]).unwrap().1 = 99;
    assert!(matches!(
        approve(&db, &store, &subject, s.unit.id, Uuid::new_v4())
            .await
            .unwrap(),
        ApproveOutcome::Refreshed { generation: 2 }
    ));
    let (st, su) = (store.clone(), subject.clone());
    let id = s.unit.id;
    let late = in_tx(&db, move |tx| {
        let (st, su) = (st.clone(), su.clone());
        Box::pin(
            async move { Engine::approve(&st, &su, tx, id, Uuid::new_v4(), 1, None, T1).await },
        )
    })
    .await;
    assert!(matches!(
        late,
        Err(ApprovalError::GenerationMismatch {
            seen: 1,
            current: 2
        })
    ));
}

#[tokio::test]
async fn quorum_zero_applies_at_submit_and_records_the_unit() {
    let db = db().await;
    let author = Uuid::new_v4();
    let (subject, ids) = rows(author, 2);
    let store = Mem::default();
    let s = submit(&db, &store, &subject, ids.clone(), author, 0)
        .await
        .unwrap();
    assert_eq!(unit_of(&db, &store, s.unit.id).await, s.unit);
    assert!(store.decisions.lock().is_empty());
    assert!(s.applied);
    assert_eq!(s.unit.state, UnitState::Approved);
    assert_eq!(s.unit.decided_at, Some(T0));
    assert_eq!(subject.applied.lock().len(), 2);
    assert!(subject.locked.lock().is_empty());
    assert_eq!(
        *subject.unlocked.lock(),
        ids.iter().map(|id| (*id, true)).collect::<Vec<_>>(),
        "applied at submit: every lock turns into its approval"
    );
}
#[tokio::test]
async fn quorum_one_pends_then_an_independent_approve_applies() {
    let db = db().await;
    let author = Uuid::new_v4();
    let (subject, ids) = rows(author, 1);
    let store = Mem::default();
    let s = submit(&db, &store, &subject, ids.clone(), author, 1)
        .await
        .unwrap();
    assert!(!s.applied);
    assert!(matches!(
        approve(&db, &store, &subject, s.unit.id, author).await,
        Err(ApprovalError::SodViolation)
    ));
    assert!(
        subject.unlocked.lock().is_empty(),
        "a refusal unlocks nothing"
    );
    assert!(matches!(
        approve(&db, &store, &subject, s.unit.id, Uuid::new_v4())
            .await
            .unwrap(),
        ApproveOutcome::Applied
    ));
    assert_eq!(*subject.applied.lock(), ids);
    assert!(subject.locked.lock().is_empty());
    assert_eq!(
        *subject.unlocked.lock(),
        [(ids[0], true)],
        "unlocked as approved"
    );
    let applied = unit_of(&db, &store, s.unit.id).await;
    assert_eq!(applied.state, UnitState::Approved);
    assert_eq!(applied.decided_at, Some(T1));
    assert_eq!(applied.decided_note, None);
    assert!(matches!(
        approve(&db, &store, &subject, s.unit.id, Uuid::new_v4()).await,
        Err(ApprovalError::AlreadyDecided)
    ));
}
#[tokio::test]
async fn a_locked_item_refuses_a_second_unit() {
    let db = db().await;
    let author = Uuid::new_v4();
    let (subject, ids) = rows(author, 1);
    let store = Mem::default();
    submit(&db, &store, &subject, ids.clone(), author, 1)
        .await
        .unwrap();
    assert!(matches!(
        submit(&db, &store, &subject, ids, author, 1).await,
        Err(ApprovalError::Locked { .. })
    ));
    // the second unit row was inserted by the fake and would be rolled back by a real store; the fake keeps it — that is the fake's limit, not the engine's
}
#[tokio::test]
async fn content_drift_refreshes_the_generation_and_the_same_reviewer_votes_again() {
    let db = db().await;
    let author = Uuid::new_v4();
    let (subject, ids) = rows(author, 1);
    let store = Mem::default();
    let s = submit(&db, &store, &subject, ids.clone(), author, 2)
        .await
        .unwrap();
    let first = Uuid::new_v4();
    assert!(matches!(
        approve(&db, &store, &subject, s.unit.id, first)
            .await
            .unwrap(),
        ApproveOutcome::Pending { have: 1, need: 2 }
    ));
    assert!(matches!(
        approve(&db, &store, &subject, s.unit.id, first).await,
        Err(ApprovalError::DuplicateVote)
    ));
    subject.live.lock().get_mut(&ids[0]).unwrap().1 = 99; // the proposed content changed under the reviewers
    assert!(matches!(
        approve(&db, &store, &subject, s.unit.id, author).await,
        Err(ApprovalError::SodViolation)
    ));
    assert!(matches!(
        approve(&db, &store, &subject, s.unit.id, first).await,
        Err(ApprovalError::DuplicateVote)
    ));
    assert_eq!(
        unit_of(&db, &store, s.unit.id).await.generation,
        1,
        "ineligible reviewers cannot refresh"
    );
    let second = Uuid::new_v4();
    assert!(matches!(
        approve(&db, &store, &subject, s.unit.id, second)
            .await
            .unwrap(),
        ApproveOutcome::Refreshed { generation: 2 }
    ));
    let u = unit_of(&db, &store, s.unit.id).await;
    assert_ne!(u.snapshot_hash, s.unit.snapshot_hash);
    assert_eq!(u.generation, 2);
    assert_eq!(u.state, UnitState::Pending);
    assert_eq!(store.decisions.lock().len(), 1, "refresh adds no vote");
    assert!(
        store.decisions.lock().iter().all(|d| d.stale),
        "the first vote is stale"
    );
    assert!(
        matches!(
            approve(&db, &store, &subject, s.unit.id, first)
                .await
                .unwrap(),
            ApproveOutcome::Pending { have: 1, need: 2 }
        ),
        "the first reviewer votes again on what they now see"
    );
    assert!(matches!(
        approve(&db, &store, &subject, s.unit.id, second)
            .await
            .unwrap(),
        ApproveOutcome::Applied
    ));
    assert_eq!(*subject.unlocked.lock(), [(ids[0], true)]);
    assert_eq!(
        unit_of(&db, &store, s.unit.id).await.state,
        UnitState::Approved
    );
}
#[tokio::test]
async fn a_lost_version_race_is_contended_and_writes_nothing() {
    let db = db().await;
    let author = Uuid::new_v4();
    let (subject, ids) = rows(author, 1);
    let store = Mem::default();
    let s = submit(&db, &store, &subject, ids, author, 1).await.unwrap();
    *store.steal_next_bump.lock() = true; // the fake's bump_version answers false once: another writer won between our read and our CAS
    let before = store.decisions.lock().len();
    assert!(matches!(
        approve(&db, &store, &subject, s.unit.id, Uuid::new_v4()).await,
        Err(ApprovalError::Contended)
    ));
    assert_eq!(store.decisions.lock().len(), before);
    assert!(subject.applied.lock().is_empty());
    assert_eq!(
        unit_of(&db, &store, s.unit.id).await.state,
        UnitState::Pending
    );
}
#[tokio::test]
async fn an_apply_refusal_keeps_the_unit_pending() {
    let db = db().await;
    let author = Uuid::new_v4();
    let (subject, ids) = rows(author, 1);
    let store = Mem::default();
    let s = submit(&db, &store, &subject, ids, author, 1).await.unwrap();
    *subject.refuse_apply.lock() = true;
    assert!(matches!(
        approve(&db, &store, &subject, s.unit.id, Uuid::new_v4()).await,
        Err(ApprovalError::ApplyRefused { .. })
    ));
    assert!(subject.applied.lock().is_empty());
    assert!(!subject.locked.lock().is_empty());
    // the fake cannot roll back; a real store rolls the vote back with the transaction. The assertion that matters here:
    assert_eq!(
        unit_of(&db, &store, s.unit.id).await.state,
        UnitState::Pending
    );
}
#[tokio::test]
async fn reject_needs_a_note_and_unlocks_withdraw_is_the_submitters() {
    let db = db().await;
    let author = Uuid::new_v4();
    let (subject, ids) = rows(author, 1);
    let store = Mem::default();
    let s = submit(&db, &store, &subject, ids.clone(), author, 1)
        .await
        .unwrap();
    let (st, su) = (store.clone(), subject.clone());
    let id = s.unit.id;
    let w = in_tx(&db, move |tx| {
        let (st, su) = (st.clone(), su.clone());
        Box::pin(async move { Engine::withdraw(&st, &su, tx, id, Uuid::new_v4(), T1).await })
    })
    .await;
    assert!(matches!(w, Err(ApprovalError::NotSubmitter)));
    let (st, su) = (store.clone(), subject.clone());
    let r = in_tx(&db, move |tx| {
        let (st, su) = (st.clone(), su.clone());
        Box::pin(async move { Engine::reject(&st, &su, tx, id, Uuid::new_v4(), 1, "  ", T1).await })
    })
    .await;
    assert!(matches!(r, Err(ApprovalError::NoteRequired)));
    let (st, su) = (store.clone(), subject.clone());
    let outcome = in_tx(&db, move |tx| {
        let (st, su) = (st.clone(), su.clone());
        Box::pin(async move {
            Engine::reject(&st, &su, tx, id, Uuid::new_v4(), 1, "wrong amount", T1).await
        })
    })
    .await
    .unwrap();
    assert_eq!(outcome, RejectOutcome::Rejected);
    let rejected = unit_of(&db, &store, id).await;
    assert_eq!(rejected.state, UnitState::Rejected);
    assert_eq!(rejected.decided_note.as_deref(), Some("wrong amount"));
    assert_eq!(rejected.decided_at, Some(T1));
    assert!(subject.locked.lock().is_empty());
    assert_eq!(
        *subject.unlocked.lock(),
        [(ids[0], false)],
        "unlocked, not approved"
    );
    assert!(subject.applied.lock().is_empty());
    let (subject, ids) = rows(author, 1);
    let s = submit(&db, &store, &subject, ids.clone(), author, 1)
        .await
        .unwrap();
    let (st, su) = (store.clone(), subject.clone());
    let id = s.unit.id;
    in_tx(&db, move |tx| {
        let (st, su) = (st.clone(), su.clone());
        Box::pin(async move { Engine::withdraw(&st, &su, tx, id, author, T1).await })
    })
    .await
    .unwrap();
    let withdrawn = unit_of(&db, &store, id).await;
    assert_eq!(withdrawn.state, UnitState::Withdrawn);
    assert_eq!(withdrawn.decided_at, Some(T1));
    assert!(subject.locked.lock().is_empty());
    assert_eq!(*subject.unlocked.lock(), [(ids[0], false)]);
    assert!(subject.applied.lock().is_empty());
}

/// Products P-D-219, pricing D-445: the submitter's note is stored on the unit as sent, and it is
/// not content. Three submits of the same items (each withdrawn to free the lock) that differ only
/// by their note — two notes and none — record the same fingerprint. (The fake's snapshot never
/// sees the note, and `Store::refresh` takes none: the gears' store tests cover both.)
#[tokio::test]
async fn the_submitters_note_is_stored_on_the_unit_and_is_not_content() {
    let db = db().await;
    let author = Uuid::new_v4();
    let (subject, ids) = rows(author, 2);
    let store = Mem::default();
    let mut units = Vec::new();
    for note in [Some("raise for Q4"), Some("  another reason  "), None] {
        let s = submit_noted(&db, &store, &subject, ids.clone(), author, 1, note)
            .await
            .unwrap();
        assert_eq!(s.unit.submit_note.as_deref(), note);
        let stored = unit_of(&db, &store, s.unit.id).await;
        assert_eq!(stored.submit_note.as_deref(), note, "stored as sent");
        withdraw(&db, &store, &subject, s.unit.id, author).await;
        units.push(stored);
    }
    for u in &units[1..] {
        assert_eq!(
            u.snapshot_hash, units[0].snapshot_hash,
            "a note is not content"
        );
    }
    // The withdrawal decided the unit; the submitter's note stays beside the decision.
    let withdrawn = unit_of(&db, &store, units[0].id).await;
    assert_eq!(withdrawn.state, UnitState::Withdrawn);
    assert_eq!(withdrawn.submit_note.as_deref(), Some("raise for Q4"));
}

/// A reject with the unit's current generation, under the fake's transaction.
async fn reject(
    db: &Db,
    store: &Mem,
    subject: &Rows,
    unit: Uuid,
    actor: Uuid,
    generation: i32,
    note: &'static str,
) -> Result<RejectOutcome, ApprovalError> {
    let (store, subject) = (store.clone(), subject.clone());
    in_tx(db, move |tx| {
        let (store, subject) = (store.clone(), subject.clone());
        Box::pin(async move {
            Engine::reject(&store, &subject, tx, unit, actor, generation, note, T1).await
        })
    })
    .await
}
/// An approve carrying a note.
async fn approve_noted(
    db: &Db,
    store: &Mem,
    subject: &Rows,
    unit: Uuid,
    actor: Uuid,
    note: String,
) -> Result<ApproveOutcome, ApprovalError> {
    let (store, subject) = (store.clone(), subject.clone());
    in_tx(db, move |tx| {
        let (store, subject, note) = (store.clone(), subject.clone(), note.clone());
        Box::pin(async move {
            Engine::approve(&store, &subject, tx, unit, actor, 1, Some(&note), T1).await
        })
    })
    .await
}

/// A reject on content that drifted under the reviewers refreshes the unit as an approve does:
/// the unit stays pending at the next generation, records no vote and keeps its locks; the
/// reviewer then rejects what they now see.
#[tokio::test]
async fn a_reject_on_drifted_content_refreshes_the_unit_and_records_no_vote() {
    let db = db().await;
    let author = Uuid::new_v4();
    let (subject, ids) = rows(author, 1);
    let store = Mem::default();
    let s = submit(&db, &store, &subject, ids.clone(), author, 1)
        .await
        .unwrap();
    subject.live.lock().get_mut(&ids[0]).unwrap().1 = 99;
    let reviewer = Uuid::new_v4();
    assert_eq!(
        reject(&db, &store, &subject, s.unit.id, reviewer, 1, "too cheap")
            .await
            .unwrap(),
        RejectOutcome::Refreshed { generation: 2 }
    );
    let u = unit_of(&db, &store, s.unit.id).await;
    assert_eq!(
        u.state,
        UnitState::Pending,
        "a stale unit is refreshed, not rejected"
    );
    assert_eq!(u.generation, 2);
    assert_ne!(u.snapshot_hash, s.unit.snapshot_hash);
    assert!(
        store.decisions.lock().is_empty(),
        "the refresh records no vote"
    );
    assert!(
        !subject.locked.lock().is_empty(),
        "the refresh keeps the locks"
    );
    assert!(subject.unlocked.lock().is_empty());
    assert_eq!(
        reject(&db, &store, &subject, s.unit.id, reviewer, 1, "late")
            .await
            .unwrap_err()
            .code(),
        "GENERATION_MISMATCH",
        "the old generation is refused"
    );
    assert_eq!(
        reject(
            &db,
            &store,
            &subject,
            s.unit.id,
            reviewer,
            2,
            "still too cheap"
        )
        .await
        .unwrap(),
        RejectOutcome::Rejected
    );
    let u = unit_of(&db, &store, s.unit.id).await;
    assert_eq!(u.state, UnitState::Rejected);
    assert_eq!(u.decided_note.as_deref(), Some("still too cheap"));
    assert!(subject.locked.lock().is_empty());
    assert_eq!(*subject.unlocked.lock(), [(ids[0], false)]);
}

/// The same reviewer may not approve and then reject one generation: the reject is a duplicate
/// vote, and the unit stays pending with its locks held.
#[tokio::test]
async fn an_approve_then_a_reject_by_one_reviewer_is_a_duplicate_vote() {
    let db = db().await;
    let author = Uuid::new_v4();
    let (subject, ids) = rows(author, 1);
    let store = Mem::default();
    let s = submit(&db, &store, &subject, ids, author, 2).await.unwrap();
    let reviewer = Uuid::new_v4();
    assert!(matches!(
        approve(&db, &store, &subject, s.unit.id, reviewer)
            .await
            .unwrap(),
        ApproveOutcome::Pending { have: 1, need: 2 }
    ));
    assert!(matches!(
        reject(
            &db,
            &store,
            &subject,
            s.unit.id,
            reviewer,
            1,
            "changed my mind"
        )
        .await,
        Err(ApprovalError::DuplicateVote)
    ));
    let u = unit_of(&db, &store, s.unit.id).await;
    assert_eq!(u.state, UnitState::Pending);
    assert_eq!(store.decisions.lock().len(), 1);
    assert!(!subject.locked.lock().is_empty());
}

/// A vote's note is at most `NOTE_MAX_CHARS` characters (Unicode scalar values), on approve and
/// on reject alike; a longer one is refused before any write.
#[tokio::test]
async fn a_vote_note_longer_than_the_cap_is_refused_before_any_write() {
    let db = db().await;
    let author = Uuid::new_v4();
    let (subject, ids) = rows(author, 1);
    let store = Mem::default();
    let s = submit(&db, &store, &subject, ids, author, 2).await.unwrap();
    let long = "e".repeat(2001);
    let refused = approve_noted(
        &db,
        &store,
        &subject,
        s.unit.id,
        Uuid::new_v4(),
        long.clone(),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.code(), "NOTE_TOO_LONG", "{refused}");
    let long: &'static str = Box::leak(long.into_boxed_str());
    let refused = reject(&db, &store, &subject, s.unit.id, Uuid::new_v4(), 1, long)
        .await
        .unwrap_err();
    assert_eq!(refused.code(), "NOTE_TOO_LONG", "{refused}");
    assert!(store.decisions.lock().is_empty(), "nothing was written");
    assert_eq!(
        unit_of(&db, &store, s.unit.id).await.version,
        s.unit.version
    );
    // 2000 two-byte characters are within the cap.
    let at_cap = "\u{e9}".repeat(2000);
    assert!(matches!(
        approve_noted(&db, &store, &subject, s.unit.id, Uuid::new_v4(), at_cap)
            .await
            .unwrap(),
        ApproveOutcome::Pending { have: 1, need: 2 }
    ));
}

/// A unit the store does not hold is the engine's own typed refusal, not a store failure.
#[tokio::test]
async fn a_missing_unit_is_unit_not_found_at_every_vote() {
    let db = db().await;
    let (subject, _) = rows(Uuid::new_v4(), 1);
    let store = Mem::default();
    let missing = Uuid::new_v4();
    let approve = approve(&db, &store, &subject, missing, Uuid::new_v4())
        .await
        .unwrap_err();
    assert_eq!(approve.code(), "UNIT_NOT_FOUND", "{approve}");
    let reject = reject(&db, &store, &subject, missing, Uuid::new_v4(), 1, "no")
        .await
        .unwrap_err();
    assert_eq!(reject.code(), "UNIT_NOT_FOUND", "{reject}");
    let (st, su) = (store.clone(), subject.clone());
    let withdraw = in_tx(&db, move |tx| {
        let (st, su) = (st.clone(), su.clone());
        Box::pin(async move { Engine::withdraw(&st, &su, tx, missing, Uuid::new_v4(), T1).await })
    })
    .await
    .unwrap_err();
    assert_eq!(withdraw.code(), "UNIT_NOT_FOUND", "{withdraw}");
}

/// W2: a reader's view of a unit (the vote counts, whether its reader may approve) comes from
/// `approve_eligibility` over the unit, its stored items and its decisions, and the engine's own
/// approve is judged by the same function: before every vote of a run through quorum 2, a
/// refused actor, a duplicate, a content drift and its refresh, the stale vote that no longer
/// counts and the apply, the predicate's refusal is the vote's error, and an eligible vote pends
/// at the predicate's approvals plus one, applies, or refreshes a drifted unit. Quorum 0 applies
/// at the submit, and the predicate then answers what every vote meets: the unit is decided.
#[tokio::test]
async fn the_approve_eligibility_is_the_engines_own_answer() {
    async fn judged(
        db: &Db,
        store: &Mem,
        unit: Uuid,
        actor: Uuid,
    ) -> bss_approval::ApproveEligibility {
        let store = store.clone();
        in_tx(db, move |tx| {
            let store = store.clone();
            Box::pin(async move {
                let u = store.unit(tx, unit).await?.unwrap();
                let items = store.items(tx, unit).await?;
                let decisions = store.decisions(tx, unit).await?;
                Ok(approve_eligibility(
                    &u,
                    items.iter().map(|i| i.created_by),
                    &decisions,
                    actor,
                ))
            })
        })
        .await
        .unwrap()
    }
    let db = db().await;
    let author = Uuid::new_v4();
    let (subject, ids) = rows(author, 1);
    let store = Mem::default();
    let s = submit(&db, &store, &subject, ids.clone(), author, 2)
        .await
        .unwrap();
    let (first, second, late) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let mut seen = Vec::new();
    for (step, actor) in [
        ("the submitter", author),
        ("a first reviewer", first),
        ("the same reviewer again", first),
        ("after a drift", second),
        ("the first reviewer on the new generation", first),
        ("the quorum", second),
        ("after the apply", late),
    ] {
        if step == "after a drift" {
            subject.live.lock().get_mut(&ids[0]).unwrap().1 = 99;
        }
        let before = judged(&db, &store, s.unit.id, actor).await;
        let outcome = approve(&db, &store, &subject, s.unit.id, actor).await;
        match (&before.refusal, &outcome) {
            (Some(refusal), Err(error)) => {
                assert_eq!(ApprovalError::from(*refusal).code(), error.code(), "{step}");
            }
            (None, Ok(ApproveOutcome::Pending { have, need })) => {
                assert_eq!(*have, before.approvals + 1, "{step}");
                assert_eq!(*need, 2, "{step}");
            }
            (None, Ok(ApproveOutcome::Applied)) => {
                assert!(before.approvals + 1 >= 2, "{step}: {before:?}");
            }
            (None, Ok(ApproveOutcome::Refreshed { .. })) => {}
            other => panic!("{step}: the predicate and the engine disagree: {other:?}"),
        }
        seen.push((step, before.approvals, outcome.map_err(|e| e.code())));
    }
    assert_eq!(
        seen.iter().map(|(_, n, _)| *n).collect::<Vec<_>>(),
        [0, 0, 1, 1, 0, 1, 2],
        "the drift made the first vote stale, so it no longer counts: {seen:?}"
    );
    assert!(matches!(seen[6].2, Err("UNIT_ALREADY_DECIDED")), "{seen:?}");
    let (subject, ids) = rows(author, 1);
    let at_once = submit(&db, &store, &subject, ids, author, 0).await.unwrap();
    assert!(at_once.applied);
    let judged = judged(&db, &store, at_once.unit.id, late).await;
    assert_eq!(
        judged.refusal.map(|r| ApprovalError::from(r).code()),
        Some("UNIT_ALREADY_DECIDED")
    );
    assert!(matches!(
        approve(&db, &store, &subject, at_once.unit.id, late).await,
        Err(ApprovalError::AlreadyDecided)
    ));
}
