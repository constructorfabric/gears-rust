//! The audit worker (DESIGN §3.8 "the fifth Orders-owned worker"; D-100): per immutable audit
//! namespace under the key `audit/<namespace>`, a separately privileged daily checkpoint
//! phase (checkpoint role: one consistent `REPEATABLE READ` snapshot, members streamed in
//! ascending binary `order_id` order, counter and prior-prefix reconciliation, atomic
//! header/member publication, conflict-safe sequence uniqueness) and a read-only rolling
//! verification phase (verifier role: every committed chain recomputed, every checkpoint digest
//! recomputed and chained, recorded members compared with the recomputed chain). A mismatch
//! alerts and rolls back; nothing is ever repaired, rehashed or blessed.
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use toolkit_db::secure::{ScopeError, TxConfig, TxIsolationLevel};
use toolkit_db::{Db, DbTx};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::metrics::{CheckpointResult, IntegrityFinding, WorkerMetrics};
use super::{PassResult, WorkerError, WorkerKind, WorkerParts, grant_for, guarded_pass};
use crate::domain::audit::{
    CHECKPOINT_FORMAT_VERSION, ChainVerifier, CheckpointDigest, CheckpointHeader, CheckpointMember,
    DIGEST_LEN, checkpoint_genesis, normalize_instant,
};
use crate::infra::maintenance::scope::discover_audit_namespaces;
use crate::infra::maintenance::{MaintenanceTask, TargetScope};
use crate::infra::storage::entity::{audit_checkpoint, audit_checkpoint_member};
use crate::infra::storage::repo::audit::{
    AuditStoreError, MAX_CHAIN_PAGE, MAX_NAMESPACE_PAGE, NamespaceOrder, chain_heads,
    checkpoint_members_page, checkpoints_page, committed_chain_page, latest_checkpoint,
    members_for_order, namespace_clock, namespace_member_count, namespace_orders_page,
    overflow_rows,
};
use crate::infra::storage::repo::private::{
    insert_audit_checkpoint, insert_audit_checkpoint_members,
};

/// Findings collected per pass before the pass aborts (alerts stay bounded).
const MAX_FINDINGS: usize = 64;

#[cfg(test)]
tokio::task_local! {
    /// Awaited after the snapshot is taken and before the capture appends (test builds only).
    static AFTER_SNAPSHOT: std::sync::Arc<tokio::sync::Barrier>;
}

/// Test seam (PR #5259 #104): run `future` so every checkpoint capture it polls waits on
/// `barrier` after its snapshot is taken and before it appends, which lets two contenders hold
/// the same snapshot predecessor to prove conflict-safe sequence uniqueness. The pause lives on
/// the calling task (pricing's `with_create_faults` precedent), so it exists in test builds only
/// and no production signature or struct carries it.
#[cfg(test)]
pub async fn with_capture_paused_after_snapshot<T>(
    barrier: std::sync::Arc<tokio::sync::Barrier>,
    future: impl std::future::Future<Output = T>,
) -> T {
    AFTER_SNAPSHOT.scope(barrier, future).await
}

/// What a checkpoint capture did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// The header and all members were published atomically.
    Appended {
        /// The sequence of the new checkpoint.
        sequence: i64,
        /// Members published with it.
        members: u64,
    },
    /// Discrepancies were found; the snapshot was rolled back and the last checkpoint stands.
    Mismatch(Vec<IntegrityFinding>),
    /// A competing append took the sequence; the whole snapshot rolled back (no fork).
    Conflict,
}

enum CaptureAbort {
    Findings(Vec<IntegrityFinding>),
    Conflict,
    Cancelled,
    Store(anyhow::Error),
}
/// A unique-violation (SQLSTATE 23505) on the checkpoint sequence: a competing append won.
fn is_conflict(error: &ScopeError) -> bool {
    use sea_orm::{DbErr, RuntimeErr, sqlx};
    let ScopeError::Db(DbErr::Exec(runtime) | DbErr::Query(runtime)) = error else {
        return false;
    };
    let RuntimeErr::SqlxError(sqlx_error) = runtime else {
        return false;
    };
    let sqlx::Error::Database(db) = sqlx_error.as_ref() else {
        return false;
    };
    db.code().is_some_and(|code| code == "23505")
}
impl From<toolkit_db::DbError> for CaptureAbort {
    fn from(error: toolkit_db::DbError) -> Self {
        Self::Store(error.into())
    }
}
impl From<ScopeError> for CaptureAbort {
    fn from(error: ScopeError) -> Self {
        if is_conflict(&error) {
            Self::Conflict
        } else {
            Self::Store(error.into())
        }
    }
}
impl From<AuditStoreError> for CaptureAbort {
    fn from(error: AuditStoreError) -> Self {
        match error {
            AuditStoreError::Store(scope) => scope.into(),
            other => Self::Store(other.into()),
        }
    }
}
impl From<crate::domain::audit::AuditError> for CaptureAbort {
    fn from(error: crate::domain::audit::AuditError) -> Self {
        Self::Store(error.into())
    }
}

/// Lazily paged members of the previous checkpoint, ascending by `order_id`.
struct PrevMembers<'t> {
    tx: &'t DbTx<'t>,
    target: &'t TargetScope,
    sequence: Option<i64>,
    buffer: VecDeque<audit_checkpoint_member::Model>,
    after: Option<Uuid>,
    exhausted: bool,
}
impl<'t> PrevMembers<'t> {
    fn new(tx: &'t DbTx<'t>, target: &'t TargetScope, sequence: Option<i64>) -> Self {
        Self {
            tx,
            target,
            sequence,
            buffer: VecDeque::new(),
            after: None,
            exhausted: sequence.is_none(),
        }
    }
    async fn fill(&mut self) -> Result<(), CaptureAbort> {
        if !self.buffer.is_empty() || self.exhausted {
            return Ok(());
        }
        let Some(sequence) = self.sequence else {
            self.exhausted = true;
            return Ok(());
        };
        let page = checkpoint_members_page(
            self.tx,
            self.target,
            sequence,
            self.after,
            MAX_NAMESPACE_PAGE,
        )
        .await?;
        if (page.len() as u64) < MAX_NAMESPACE_PAGE {
            self.exhausted = true;
        }
        self.after = page.last().map(|m| m.order_id);
        self.buffer.extend(page);
        Ok(())
    }
    async fn peek(&mut self) -> Result<Option<&audit_checkpoint_member::Model>, CaptureAbort> {
        self.fill().await?;
        Ok(self.buffer.front())
    }
    fn pop(&mut self) -> Option<audit_checkpoint_member::Model> {
        self.buffer.pop_front()
    }
}

fn digest_of(bytes: &[u8]) -> Option<[u8; DIGEST_LEN]> {
    (bytes.len() == DIGEST_LEN).then(|| {
        let mut out = [0u8; DIGEST_LEN];
        out.copy_from_slice(bytes);
        out
    })
}

/// Reconcile a previous member against the order's current counter and head. A prefix behind
/// the head is looked up in the page's batched `prefixes`.
fn reconcile_prefix(
    recorded: &audit_checkpoint_member::Model,
    counter: i64,
    head_hash: &[u8],
    prefixes: &HashMap<(Uuid, i64), Vec<u8>>,
    findings: &mut Vec<IntegrityFinding>,
) {
    let order_id = recorded.order_id;
    if recorded.audit_sequence > counter {
        findings.push(IntegrityFinding::CounterRollback {
            order_id,
            recorded: recorded.audit_sequence,
            counter,
        });
        return;
    }
    let stored = if recorded.audit_sequence == counter {
        Some(head_hash)
    } else {
        prefixes
            .get(&(order_id, recorded.audit_sequence))
            .map(Vec::as_slice)
    };
    match stored {
        None => findings.push(IntegrityFinding::PrefixMissing {
            order_id,
            sequence: recorded.audit_sequence,
        }),
        Some(stored) if stored != recorded.entry_hash.as_slice() => {
            findings.push(IntegrityFinding::PrefixDigest {
                order_id,
                sequence: recorded.audit_sequence,
            });
        }
        Some(_) => {}
    }
}

/// The consistent snapshot a capture starts from: predecessor, header and expected count.
struct Snapshot {
    namespace: Uuid,
    previous_sequence: Option<i64>,
    header: CheckpointHeader,
}

async fn snapshot(
    tx: &DbTx<'_>,
    target: &TargetScope,
) -> Result<Result<Snapshot, IntegrityFinding>, CaptureAbort> {
    let namespace = target
        .audit_namespace()
        .ok_or_else(|| CaptureAbort::Store(anyhow::anyhow!("not an audit namespace target")))?;
    let previous = latest_checkpoint(tx, target).await?;
    let (sequence, prev_hash) = match &previous {
        Some(prev) => {
            let Some(hash) = digest_of(&prev.checkpoint_hash) else {
                return Ok(Err(IntegrityFinding::CheckpointMembers {
                    sequence: prev.checkpoint_sequence,
                    reason: "stored checkpoint digest length".to_owned(),
                }));
            };
            (prev.checkpoint_sequence + 1, hash.to_vec())
        }
        None => (1, checkpoint_genesis(namespace).to_vec()),
    };
    let expected = namespace_member_count(tx, target).await?;
    let captured_at = normalize_instant(namespace_clock(tx, target).await?)?;
    Ok(Ok(Snapshot {
        namespace,
        previous_sequence: previous.as_ref().map(|p| p.checkpoint_sequence),
        header: CheckpointHeader {
            format_version: CHECKPOINT_FORMAT_VERSION,
            audit_tenant_id: namespace,
            checkpoint_sequence: sequence,
            captured_at,
            member_count: i64::try_from(expected).unwrap_or(i64::MAX),
            prev_checkpoint_hash: prev_hash,
        },
    }))
}

/// Everything a page of the live inventory needs: its heads, the recorded prefixes behind them
/// and its overflow rows.
struct PageEvidence {
    heads: HashMap<Uuid, Vec<u8>>,
    prefixes: HashMap<(Uuid, i64), Vec<u8>>,
}
/// The page's evidence in a fixed number of statements: one `chain_heads` call for the heads,
/// one for every recorded prefix behind its head (no per-order lookup), one overflow probe.
async fn page_evidence(
    tx: &DbTx<'_>,
    target: &TargetScope,
    page: &[Matched<'_>],
    findings: &mut Vec<IntegrityFinding>,
) -> Result<PageEvidence, CaptureAbort> {
    let positions = page
        .iter()
        .filter(|m| m.order.audit_sequence > 0)
        .map(|m| (m.order.order_id, m.order.audit_sequence))
        .collect::<Vec<_>>();
    let heads = chain_heads(tx, target, &positions)
        .await?
        .into_iter()
        .map(|row| (row.order_id, row.entry_hash))
        .collect::<HashMap<_, _>>();
    let behind = page
        .iter()
        .filter_map(|m| {
            m.recorded
                .as_ref()
                .filter(|r| r.audit_sequence < m.order.audit_sequence)
                .map(|r| (r.order_id, r.audit_sequence))
        })
        .collect::<Vec<_>>();
    let prefixes = chain_heads(tx, target, &behind)
        .await?
        .into_iter()
        .map(|row| ((row.order_id, row.sequence), row.entry_hash))
        .collect::<HashMap<_, _>>();
    let page = page.iter().map(|m| m.order).collect::<Vec<_>>();
    let ids = page.iter().map(|o| o.order_id).collect::<Vec<_>>();
    let counters = page
        .iter()
        .map(|o| (o.order_id, o.audit_sequence))
        .collect::<BTreeMap<_, _>>();
    for row in overflow_rows(tx, target, &ids).await? {
        findings.push(IntegrityFinding::Overflow {
            order_id: row.order_id,
            sequence: row.audit_sequence,
            counter: counters.get(&row.order_id).copied().unwrap_or_default(),
        });
    }
    Ok(PageEvidence { heads, prefixes })
}

/// Advance the previous-member stream to this order: everything recorded before it is an
/// order that vanished from the namespace; a record for it is returned.
async fn recorded_for(
    prev_members: &mut PrevMembers<'_>,
    order_id: Uuid,
    findings: &mut Vec<IntegrityFinding>,
) -> Result<Option<audit_checkpoint_member::Model>, CaptureAbort> {
    while let Some(recorded) = prev_members.peek().await? {
        if recorded.order_id.as_bytes() >= order_id.as_bytes() {
            break;
        }
        findings.push(IntegrityFinding::OrderMissing {
            order_id: recorded.order_id,
            recorded: recorded.audit_sequence,
        });
        prev_members.pop();
    }
    Ok(match prev_members.peek().await? {
        Some(recorded) if recorded.order_id == order_id => prev_members.pop(),
        _ => None,
    })
}

/// One live order of the page with the previous record it matched and the `OrderMissing`
/// findings of the records the stream passed on the way to it.
struct Matched<'p> {
    order: &'p NamespaceOrder,
    missing: Vec<IntegrityFinding>,
    recorded: Option<audit_checkpoint_member::Model>,
}

/// One live order with its current head and its previous record, ready for reconciliation.
struct Candidate<'c> {
    order: &'c NamespaceOrder,
    head: Option<&'c Vec<u8>>,
    recorded: Option<audit_checkpoint_member::Model>,
}

/// Reconcile one live order and, when it is intact, fold it into the digest and return the
/// member row to publish with the page.
fn reconcile_order(
    snapshot: &Snapshot,
    candidate: Candidate<'_>,
    prefixes: &HashMap<(Uuid, i64), Vec<u8>>,
    digest: &mut CheckpointDigest,
    findings: &mut Vec<IntegrityFinding>,
) -> Result<Option<audit_checkpoint_member::Model>, CaptureAbort> {
    let Candidate {
        order,
        head,
        recorded,
    } = candidate;
    let (order_id, counter) = (order.order_id, order.audit_sequence);
    if counter == 0 {
        if let Some(recorded) = recorded {
            findings.push(IntegrityFinding::CounterRollback {
                order_id,
                recorded: recorded.audit_sequence,
                counter: 0,
            });
        }
        return Ok(None);
    }
    let Some(head) = head else {
        findings.push(IntegrityFinding::HeadMissing { order_id, counter });
        return Ok(None);
    };
    if digest_of(head).is_none() {
        findings.push(IntegrityFinding::HeadMalformed { order_id, counter });
        return Ok(None);
    }
    if let Some(recorded) = recorded {
        reconcile_prefix(&recorded, counter, head, prefixes, findings);
    }
    let member = CheckpointMember {
        order_id,
        audit_sequence: counter,
        entry_hash: head.clone(),
    };
    digest.push(&member)?;
    Ok(Some(audit_checkpoint_member::Model {
        audit_tenant_id: snapshot.namespace,
        checkpoint_sequence: snapshot.header.checkpoint_sequence,
        order_id: member.order_id,
        audit_sequence: member.audit_sequence,
        entry_hash: member.entry_hash,
    }))
}

async fn capture_in<'t>(
    tx: &'t DbTx<'t>,
    target: &'t TargetScope,
    cancel: &CancellationToken,
) -> Result<CaptureOutcome, CaptureAbort> {
    let snapshot = match snapshot(tx, target).await? {
        Ok(snapshot) => snapshot,
        Err(finding) => return Err(CaptureAbort::Findings(vec![finding])),
    };
    let mut findings = Vec::new();
    let mut digest = CheckpointDigest::new(&snapshot.header)?;
    #[cfg(test)]
    if let Ok(barrier) = AFTER_SNAPSHOT.try_with(std::sync::Arc::clone) {
        barrier.wait().await;
    }
    let mut prev_members = PrevMembers::new(tx, target, snapshot.previous_sequence);
    let mut after = None;
    let mut members: u64 = 0;
    loop {
        if cancel.is_cancelled() {
            return Err(CaptureAbort::Cancelled);
        }
        let page = namespace_orders_page(tx, target, after, MAX_NAMESPACE_PAGE).await?;
        // Phase 1: walk the previous members alongside the page (the stream pages itself), so
        // every recorded prefix of the page is known before any evidence is read.
        let mut matched = Vec::with_capacity(page.len());
        for order in &page {
            let mut missing = Vec::new();
            let recorded = recorded_for(&mut prev_members, order.order_id, &mut missing).await?;
            matched.push(Matched {
                order,
                missing,
                recorded,
            });
        }
        let evidence = page_evidence(tx, target, &matched, &mut findings).await?;
        // Phase 2: reconcile in order, with the findings in the order a per-order walk reports
        // them, and publish the page's intact members in one statement.
        let mut rows = Vec::with_capacity(matched.len());
        for Matched {
            order,
            missing,
            recorded,
        } in matched
        {
            findings.extend(missing);
            if findings.len() >= MAX_FINDINGS {
                return Err(CaptureAbort::Findings(findings));
            }
            let candidate = Candidate {
                order,
                head: evidence.heads.get(&order.order_id),
                recorded,
            };
            if let Some(row) = reconcile_order(
                &snapshot,
                candidate,
                &evidence.prefixes,
                &mut digest,
                &mut findings,
            )? {
                rows.push(row);
                members += 1;
            }
        }
        insert_audit_checkpoint_members(tx, target, rows).await?;
        if (page.len() as u64) < MAX_NAMESPACE_PAGE {
            break;
        }
        after = page.last().map(|o| o.order_id);
    }
    while let Some(recorded) = prev_members.peek().await? {
        findings.push(IntegrityFinding::OrderMissing {
            order_id: recorded.order_id,
            recorded: recorded.audit_sequence,
        });
        prev_members.pop();
        if findings.len() >= MAX_FINDINGS {
            break;
        }
    }
    if !findings.is_empty() {
        return Err(CaptureAbort::Findings(findings));
    }
    let checkpoint_hash = digest.finish()?;
    let header = snapshot.header;
    insert_audit_checkpoint(
        tx,
        target,
        audit_checkpoint::Model {
            audit_tenant_id: snapshot.namespace,
            checkpoint_sequence: header.checkpoint_sequence,
            format_version: header.format_version,
            captured_at: header.captured_at,
            member_count: header.member_count,
            prev_checkpoint_hash: header.prev_checkpoint_hash,
            checkpoint_hash: checkpoint_hash.to_vec(),
        },
    )
    .await?;
    Ok(CaptureOutcome::Appended {
        sequence: header.checkpoint_sequence,
        members,
    })
}

/// Capture one checkpoint of the namespace from a single consistent snapshot under the
/// checkpoint role (D-100 items 1-3). Header and members commit together or not at all.
///
/// # Errors
/// A non-namespace target, cancellation, or a store failure; findings and conflicts are
/// outcomes, not errors.
pub async fn capture_checkpoint(
    db: &Db,
    target: &TargetScope,
    cancel: &CancellationToken,
) -> Result<CaptureOutcome, WorkerError> {
    let target = target.clone();
    let cancel = cancel.clone();
    let result: Result<CaptureOutcome, CaptureAbort> = db
        .transaction_ref_mapped_with_config(
            TxConfig::with_isolation(TxIsolationLevel::RepeatableRead),
            move |tx| Box::pin(async move { capture_in(tx, &target, &cancel).await }),
        )
        .await;
    match result {
        Ok(outcome) => Ok(outcome),
        Err(CaptureAbort::Findings(findings)) => Ok(CaptureOutcome::Mismatch(findings)),
        Err(CaptureAbort::Conflict) => Ok(CaptureOutcome::Conflict),
        Err(CaptureAbort::Cancelled) => Err(WorkerError::Cancelled),
        Err(CaptureAbort::Store(error)) => Err(WorkerError::Store(error)),
    }
}

// ---------------------------------------------------------------------------------------------
// Rolling verification (read-only, verifier role)

/// Where the rolling pass over one namespace stands. Local to the replica: another replica
/// taking the namespace key continues its own cursor, so coverage only progresses.
#[derive(Debug, Default, Clone)]
pub struct VerifyCursor {
    after: Option<Uuid>,
    checkpoints_checked: bool,
    last_full_pass: Option<Instant>,
}
impl VerifyCursor {
    /// Age of the last completed full pass of this namespace on this replica.
    #[must_use]
    pub fn coverage_age(&self) -> Option<Duration> {
        self.last_full_pass.map(|at| at.elapsed())
    }
}

/// One verification slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifySlice {
    /// Orders whose committed chain was recomputed in this slice.
    pub orders: u64,
    /// Integrity findings of the slice (bounded per pass).
    pub findings: Vec<IntegrityFinding>,
    /// Whether this slice finished a full pass over the namespace.
    pub completed_pass: bool,
}

async fn verify_checkpoints(
    tx: &DbTx<'_>,
    target: &TargetScope,
    namespace: Uuid,
    findings: &mut Vec<IntegrityFinding>,
) -> Result<(), WorkerError> {
    let mut expected = 1;
    let mut prev_hash = checkpoint_genesis(namespace).to_vec();
    let mut after = 0;
    'pages: loop {
        let page = checkpoints_page(tx, target, after, MAX_NAMESPACE_PAGE).await?;
        for cp in &page {
            if cp.checkpoint_sequence != expected {
                findings.push(IntegrityFinding::CheckpointSequence {
                    expected,
                    found: cp.checkpoint_sequence,
                });
                break 'pages;
            }
            if cp.prev_checkpoint_hash != prev_hash {
                findings.push(IntegrityFinding::CheckpointPredecessor {
                    sequence: cp.checkpoint_sequence,
                });
            }
            let header = CheckpointHeader {
                format_version: cp.format_version,
                audit_tenant_id: cp.audit_tenant_id,
                checkpoint_sequence: cp.checkpoint_sequence,
                captured_at: cp.captured_at,
                member_count: cp.member_count,
                prev_checkpoint_hash: cp.prev_checkpoint_hash.clone(),
            };
            let computed = recompute_checkpoint(tx, target, &header).await?;
            match computed {
                Err(reason) => findings.push(IntegrityFinding::CheckpointMembers {
                    sequence: cp.checkpoint_sequence,
                    reason,
                }),
                Ok(digest) if digest.as_slice() != cp.checkpoint_hash.as_slice() => {
                    findings.push(IntegrityFinding::CheckpointDigest {
                        sequence: cp.checkpoint_sequence,
                    });
                }
                Ok(_) => {}
            }
            prev_hash.clone_from(&cp.checkpoint_hash);
            expected += 1;
            if findings.len() >= MAX_FINDINGS {
                break 'pages;
            }
        }
        if (page.len() as u64) < MAX_NAMESPACE_PAGE {
            break;
        }
        after = expected - 1;
    }
    Ok(())
}

/// Recompute one stored checkpoint's digest from its stored header and streamed members.
async fn recompute_checkpoint(
    tx: &DbTx<'_>,
    target: &TargetScope,
    header: &CheckpointHeader,
) -> Result<Result<[u8; DIGEST_LEN], String>, WorkerError> {
    let mut digest = match CheckpointDigest::new(header) {
        Ok(digest) => digest,
        Err(error) => return Ok(Err(error.to_string())),
    };
    let mut after = None;
    loop {
        let page = checkpoint_members_page(
            tx,
            target,
            header.checkpoint_sequence,
            after,
            MAX_NAMESPACE_PAGE,
        )
        .await?;
        for member in &page {
            let member = CheckpointMember {
                order_id: member.order_id,
                audit_sequence: member.audit_sequence,
                entry_hash: member.entry_hash.clone(),
            };
            if let Err(error) = digest.push(&member) {
                return Ok(Err(error.to_string()));
            }
        }
        if (page.len() as u64) < MAX_NAMESPACE_PAGE {
            break;
        }
        after = page.last().map(|m| m.order_id);
    }
    Ok(digest.finish().map_err(|e| e.to_string()))
}

async fn verify_order(
    tx: &DbTx<'_>,
    target: &TargetScope,
    namespace: Uuid,
    order_id: Uuid,
    counter: i64,
    findings: &mut Vec<IntegrityFinding>,
) -> Result<(), WorkerError> {
    // Every checkpoint's recorded position of this order, by audit sequence.
    let mut recorded: BTreeMap<i64, Vec<Vec<u8>>> = BTreeMap::new();
    let mut after = 0;
    loop {
        let page = members_for_order(tx, target, order_id, after, MAX_NAMESPACE_PAGE).await?;
        for member in &page {
            recorded
                .entry(member.audit_sequence)
                .or_default()
                .push(member.entry_hash.clone());
        }
        if (page.len() as u64) < MAX_NAMESPACE_PAGE {
            break;
        }
        after = page.last().map_or(after, |m| m.checkpoint_sequence);
    }
    let scope = AccessScope::for_resources(vec![order_id]);
    let mut verifier = ChainVerifier::new(namespace, order_id);
    let mut after_sequence = 0;
    loop {
        let page =
            committed_chain_page(tx, &scope, order_id, after_sequence, MAX_CHAIN_PAGE).await?;
        for (row, stored_hash) in &page {
            if let Err(error) = verifier.push(row, stored_hash) {
                findings.push(IntegrityFinding::Chain { order_id, error });
                return Ok(());
            }
            if let Some(head) = verifier.head() {
                if recorded.get(&head.sequence).is_some_and(|hashes| {
                    hashes
                        .iter()
                        .any(|h| h.as_slice() != head.entry_hash.as_slice())
                }) {
                    findings.push(IntegrityFinding::MemberDigest {
                        order_id,
                        sequence: head.sequence,
                    });
                }
                after_sequence = head.sequence;
            }
        }
        if (page.len() as u64) < MAX_CHAIN_PAGE {
            break;
        }
    }
    if let Err(error) = verifier.finish(counter) {
        findings.push(IntegrityFinding::Chain { order_id, error });
    }
    for sequence in recorded.keys().filter(|s| **s > counter) {
        findings.push(IntegrityFinding::MemberBeyondCounter {
            order_id,
            sequence: *sequence,
            counter,
        });
    }
    Ok(())
}

/// Verify one bounded slice of the namespace in a read-only repeatable-read snapshot: the
/// checkpoint history at the start of each pass, then up to `limit` orders after the cursor.
/// A full pass completes when a page comes back short.
///
/// # Errors
/// A non-namespace target, cancellation or a store failure.
pub async fn verify_slice(
    db: &Db,
    target: &TargetScope,
    cursor: &mut VerifyCursor,
    limit: u64,
    cancel: &CancellationToken,
) -> Result<VerifySlice, WorkerError> {
    let namespace = target
        .audit_namespace()
        .ok_or(ScopeError::Invalid("not an audit namespace target"))?;
    let limit = limit.clamp(1, MAX_NAMESPACE_PAGE);
    let snapshot = cursor.clone();
    let target = target.clone();
    let cancel = cancel.clone();
    let config = TxConfig {
        isolation: Some(TxIsolationLevel::RepeatableRead),
        access_mode: Some(toolkit_db::secure::TxAccessMode::ReadOnly),
    };
    let (slice, next): (VerifySlice, VerifyCursor) = db
        .transaction_ref_mapped_with_config(config, move |tx| {
            Box::pin(async move {
                let mut findings = Vec::new();
                let mut next = snapshot;
                if !next.checkpoints_checked {
                    verify_checkpoints(tx, &target, namespace, &mut findings).await?;
                    next.checkpoints_checked = true;
                }
                let page = namespace_orders_page(tx, &target, next.after, limit).await?;
                let mut orders = 0;
                let mut last_checked = None;
                for order in &page {
                    if cancel.is_cancelled() {
                        return Err(WorkerError::Cancelled);
                    }
                    verify_order(
                        tx,
                        &target,
                        namespace,
                        order.order_id,
                        order.audit_sequence,
                        &mut findings,
                    )
                    .await?;
                    orders += 1;
                    last_checked = Some(order.order_id);
                    if findings.len() >= MAX_FINDINGS {
                        break;
                    }
                }
                // The findings bound may stop the slice mid-page: resume after the last order
                // actually checked, and complete the pass only once its final page is covered.
                let stopped_early = orders < page.len() as u64;
                let completed_pass = (page.len() as u64) < limit && !stopped_early;
                next.after = last_checked;
                if completed_pass {
                    next = VerifyCursor {
                        after: None,
                        checkpoints_checked: false,
                        last_full_pass: Some(Instant::now()),
                    };
                }
                Ok((
                    VerifySlice {
                        orders,
                        findings,
                        completed_pass,
                    },
                    next,
                ))
            })
        })
        .await?;
    *cursor = next;
    Ok(slice)
}

// ---------------------------------------------------------------------------------------------
// Scheduling

/// Replica-local state of one namespace: the rolling verification cursor and, after a
/// checkpoint mismatch, the instant before which no new capture is attempted.
#[derive(Debug, Default, Clone)]
pub struct NamespaceState {
    cursor: VerifyCursor,
    /// Set after a mismatch: the full reconciliation is not re-run on every tick while the
    /// evidence stays wrong; the verifier keeps alerting and the checkpoint age keeps growing
    /// until the configured interval elapses or the worker restarts.
    capture_held_until: Option<Instant>,
    /// The ages the last pass of this namespace reported (reset when a pass starts), folded into
    /// the cycle's roster-wide maxima by [`tick`].
    reported: ReportedAges,
}

/// The ages one namespace pass reported to the metrics sink.
#[derive(Debug, Default, Clone, Copy)]
struct ReportedAges {
    checkpoint: Option<Duration>,
    coverage: Option<Duration>,
}
impl ReportedAges {
    /// The per-field maximum of two reports (`None` only where both are `None`).
    fn max(self, other: Self) -> Self {
        Self {
            checkpoint: self.checkpoint.max(other.checkpoint),
            coverage: self.coverage.max(other.coverage),
        }
    }
}

/// The monotonic clock the capture hold is measured on (the pricing gear's injectable
/// `infra/clock.rs` precedent): production reads [`Instant::now`], a test advances its own clock
/// instead of sleeping through the hold.
pub trait AuditClock: Send + Sync + std::fmt::Debug {
    /// The current instant.
    fn now(&self) -> Instant;
}

/// The process monotonic clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;
impl AuditClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Per-namespace state of this replica's audit worker.
#[derive(Debug)]
pub struct AuditWorkerState {
    namespaces: HashMap<Uuid, NamespaceState>,
    clock: Arc<dyn AuditClock>,
}
impl Default for AuditWorkerState {
    fn default() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }
}
impl AuditWorkerState {
    /// Empty state whose capture holds are measured on `clock`.
    #[must_use]
    pub fn with_clock(clock: Arc<dyn AuditClock>) -> Self {
        Self {
            namespaces: HashMap::new(),
            clock,
        }
    }
}

fn alert(
    namespace: Uuid,
    phase: &'static str,
    findings: &[IntegrityFinding],
    metrics: &dyn WorkerMetrics,
) {
    for finding in findings {
        tracing::error!(
            target: "orders.audit",
            %namespace,
            phase,
            kind = finding.kind(),
            finding = ?finding,
            "bss-orders: AUDIT INTEGRITY MISMATCH (not repaired)"
        );
        metrics.integrity_finding(namespace, finding);
    }
}

async fn checkpoint_age(db: &Db, target: &TargetScope) -> Result<Option<Duration>, WorkerError> {
    let target = target.clone();
    let age: Result<Option<Duration>, WorkerError> = db
        .transaction_ref_mapped_with_config(TxConfig::read_only(), move |tx| {
            Box::pin(async move {
                let now = namespace_clock(tx, &target).await?;
                Ok(latest_checkpoint(tx, &target)
                    .await?
                    .map(|cp| (now - cp.captured_at).try_into().unwrap_or(Duration::ZERO)))
            })
        })
        .await;
    age
}

/// The checkpoint phase: capture when no checkpoint exists or the last one is older than the
/// configured interval (baseline 24 h), then report the result and age. After a mismatch the
/// capture is withheld for one interval: the last successful checkpoint stands, the verifier
/// keeps alerting on every slice, and the reported age crosses the 24-hour alert line, without
/// re-running a full namespace reconciliation on every tick against evidence known to be wrong.
async fn checkpoint_phase(
    parts: &WorkerParts,
    target: &TargetScope,
    namespace: Uuid,
    state: &mut NamespaceState,
    clock: &dyn AuditClock,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    let (_grant, db) = grant_for(parts, MaintenanceTask::AuditCheckpoint)?;
    let age = checkpoint_age(db, target).await?;
    if age.is_some_and(|age| age < parts.settings.checkpoint_interval) {
        state.reported.checkpoint = age;
        parts
            .metrics
            .checkpoint(namespace, CheckpointResult::Current, 0, age);
        return Ok(());
    }
    if state
        .capture_held_until
        .is_some_and(|until| clock.now() < until)
    {
        state.reported.checkpoint = age;
        parts
            .metrics
            .checkpoint(namespace, CheckpointResult::Withheld, 0, age);
        return Ok(());
    }
    state.capture_held_until = None;
    state.reported.checkpoint = age;
    match capture_checkpoint(db, target, cancel).await {
        Ok(CaptureOutcome::Appended { sequence, members }) => {
            state.reported.checkpoint = Some(Duration::ZERO);
            tracing::info!(target: "orders.audit", %namespace, sequence, members, "bss-orders: audit checkpoint appended");
            parts.metrics.checkpoint(
                namespace,
                CheckpointResult::Appended,
                members,
                Some(Duration::ZERO),
            );
        }
        Ok(CaptureOutcome::Mismatch(findings)) => {
            alert(namespace, "checkpoint", &findings, parts.metrics.as_ref());
            state.capture_held_until = Some(clock.now() + parts.settings.checkpoint_interval);
            parts
                .metrics
                .checkpoint(namespace, CheckpointResult::Mismatch, 0, age);
        }
        Ok(CaptureOutcome::Conflict) => {
            tracing::warn!(target: "orders.audit", %namespace, "bss-orders: checkpoint append conflicted with a competing replica; rolled back");
            parts
                .metrics
                .checkpoint(namespace, CheckpointResult::Conflict, 0, age);
        }
        Err(error) => {
            parts
                .metrics
                .checkpoint(namespace, CheckpointResult::Failed, 0, age);
            return Err(error);
        }
    }
    Ok(())
}

async fn verification_phase(
    parts: &WorkerParts,
    target: &TargetScope,
    namespace: Uuid,
    state: &mut NamespaceState,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    let cursor = &mut state.cursor;
    let (_grant, db) = grant_for(parts, MaintenanceTask::AuditVerification)?;
    let slice = verify_slice(
        db,
        target,
        cursor,
        parts.settings.verification_orders_per_pass,
        cancel,
    )
    .await?;
    alert(
        namespace,
        "verification",
        &slice.findings,
        parts.metrics.as_ref(),
    );
    let coverage = cursor.coverage_age();
    state.reported.coverage = coverage;
    parts
        .metrics
        .verification(namespace, slice.orders, coverage);
    Ok(())
}

/// One namespace under its own advisory key: the checkpoint phase, then a verification slice.
///
/// The pass body is cancel-safe ([`super::guarded_pass_within`] drops it at any await): a dropped
/// checkpoint capture rolls back with its one transaction and records nothing, the
/// verification slice is a read-only snapshot whose cursor is stored only after it returns, and
/// a cleared capture hold had already expired.
pub async fn namespace_pass(
    parts: &WorkerParts,
    state: &mut AuditWorkerState,
    target: TargetScope,
    cancel: &CancellationToken,
) -> PassResult {
    let Some(namespace) = target.audit_namespace() else {
        return PassResult::Unavailable;
    };
    let key = WorkerKind::namespace_key(namespace);
    let clock = Arc::clone(&state.clock);
    let state = state.namespaces.entry(namespace).or_default();
    let checkpoint_granted = parts
        .authority
        .grant(MaintenanceTask::AuditCheckpoint)
        .is_some();
    let verification_granted = parts
        .authority
        .grant(MaintenanceTask::AuditVerification)
        .is_some();
    state.reported = ReportedAges::default();
    guarded_pass(parts, WorkerKind::Audit, &key, cancel, |cancel| {
        Box::pin(async move {
            if checkpoint_granted {
                checkpoint_phase(parts, &target, namespace, state, clock.as_ref(), cancel).await?;
            }
            if verification_granted {
                verification_phase(parts, &target, namespace, state, cancel).await?;
            }
            Ok(())
        })
    })
    .await
}

/// One tick: enumerate namespaces holding committed evidence and run each namespace pass, then
/// report the oldest checkpoint and coverage ages among the namespaces visited (D-223: the age
/// gauges carry no namespace attribute). A namespace another replica held this tick reports
/// nothing here; that replica reports it. An interrupted cycle reports nothing.
pub(super) async fn tick(
    parts: &WorkerParts,
    state: &mut AuditWorkerState,
    cancel: &CancellationToken,
) {
    let enumerator = grant_for(parts, MaintenanceTask::AuditVerification)
        .or_else(|_| grant_for(parts, MaintenanceTask::AuditCheckpoint));
    let Ok((grant, db)) = enumerator else {
        parts
            .metrics
            .pass(WorkerKind::Audit, PassResult::Unavailable, Duration::ZERO);
        return;
    };
    let mut after = None;
    let mut cycle = ReportedAges::default();
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let found = match discover_audit_namespaces(db, &grant, after, MAX_NAMESPACE_PAGE).await {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!(target: "orders.audit", error = %error, "bss-orders: audit namespace discovery failed");
                parts
                    .metrics
                    .pass(WorkerKind::Audit, PassResult::Failed, Duration::ZERO);
                return;
            }
        };
        for namespace in &found {
            let target = TargetScope::from_discovered_audit_namespace(namespace);
            after = target.audit_namespace();
            namespace_pass(parts, state, target, cancel).await;
            if let Some(visited) = after.and_then(|ns| state.namespaces.get(&ns)) {
                cycle = cycle.max(visited.reported);
            }
        }
        if (found.len() as u64) < MAX_NAMESPACE_PAGE {
            break;
        }
    }
    report_cycle(parts.metrics.as_ref(), cycle);
}

/// Report one completed audit cycle's roster-wide maxima.
fn report_cycle(metrics: &dyn WorkerMetrics, cycle: ReportedAges) {
    tracing::debug!(
        target: "orders.audit",
        oldest_checkpoint_seconds = cycle.checkpoint.map(|d| d.as_secs()),
        oldest_coverage_seconds = cycle.coverage.map(|d| d.as_secs()),
        "bss-orders: audit cycle completed"
    );
    metrics.audit_cycle(cycle.checkpoint, cycle.coverage);
}
