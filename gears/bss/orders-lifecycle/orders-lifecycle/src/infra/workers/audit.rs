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
    AuditStoreError, MAX_CHAIN_PAGE, MAX_NAMESPACE_PAGE, chain_heads, checkpoint_members_page,
    checkpoints_page, committed_chain_page, entry_hash_at, latest_checkpoint, members_for_order,
    namespace_clock, namespace_member_count, namespace_orders_page, overflow_rows,
};
use crate::infra::storage::repo::private::{
    insert_audit_checkpoint, insert_audit_checkpoint_member,
};

/// Findings collected per pass before the pass aborts (alerts stay bounded).
const MAX_FINDINGS: usize = 64;

/// Test hooks on the capture: `after_snapshot` lets two contenders hold the same snapshot
/// predecessor before either appends, to prove conflict-safe sequence uniqueness.
#[derive(Clone, Default)]
pub struct CaptureHooks {
    pub after_snapshot: Option<Arc<tokio::sync::Barrier>>,
}
impl std::fmt::Debug for CaptureHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureHooks")
            .field("after_snapshot", &self.after_snapshot.is_some())
            .finish()
    }
}

/// What a checkpoint capture did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// The header and all members were published atomically.
    Appended { sequence: i64, members: u64 },
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

/// Reconcile a previous member against the order's current counter and head.
async fn reconcile_prefix(
    tx: &DbTx<'_>,
    target: &TargetScope,
    recorded: &audit_checkpoint_member::Model,
    counter: i64,
    head_hash: &[u8],
    findings: &mut Vec<IntegrityFinding>,
) -> Result<(), CaptureAbort> {
    let order_id = recorded.order_id;
    if recorded.audit_sequence > counter {
        findings.push(IntegrityFinding::CounterRollback {
            order_id,
            recorded: recorded.audit_sequence,
            counter,
        });
        return Ok(());
    }
    let stored = if recorded.audit_sequence == counter {
        Some(head_hash.to_vec())
    } else {
        entry_hash_at(tx, target, order_id, recorded.audit_sequence).await?
    };
    match stored {
        None => findings.push(IntegrityFinding::PrefixMissing {
            order_id,
            sequence: recorded.audit_sequence,
        }),
        Some(stored) if stored != recorded.entry_hash => {
            findings.push(IntegrityFinding::PrefixDigest {
                order_id,
                sequence: recorded.audit_sequence,
            });
        }
        Some(_) => {}
    }
    Ok(())
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

/// Everything a page of the live inventory needs: its heads and its overflow rows.
struct PageEvidence {
    heads: HashMap<Uuid, Vec<u8>>,
}
async fn page_evidence(
    tx: &DbTx<'_>,
    target: &TargetScope,
    page: &[crate::infra::storage::repo::audit::NamespaceOrder],
    findings: &mut Vec<IntegrityFinding>,
) -> Result<PageEvidence, CaptureAbort> {
    let positions = page
        .iter()
        .filter(|o| o.audit_sequence > 0)
        .map(|o| (o.order_id, o.audit_sequence))
        .collect::<Vec<_>>();
    let heads = chain_heads(tx, target, &positions)
        .await?
        .into_iter()
        .map(|row| (row.order_id, row.entry_hash))
        .collect::<HashMap<_, _>>();
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
    Ok(PageEvidence { heads })
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

/// One live order with its current head and its previous record, ready for reconciliation.
struct Candidate<'c> {
    order: &'c crate::infra::storage::repo::audit::NamespaceOrder,
    head: Option<&'c Vec<u8>>,
    recorded: Option<audit_checkpoint_member::Model>,
}

/// Reconcile one live order and, when it is intact, stream it as a member.
async fn reconcile_order(
    tx: &DbTx<'_>,
    target: &TargetScope,
    snapshot: &Snapshot,
    candidate: Candidate<'_>,
    digest: &mut CheckpointDigest,
    findings: &mut Vec<IntegrityFinding>,
) -> Result<bool, CaptureAbort> {
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
        return Ok(false);
    }
    let Some(head) = head else {
        findings.push(IntegrityFinding::HeadMissing { order_id, counter });
        return Ok(false);
    };
    if digest_of(head).is_none() {
        findings.push(IntegrityFinding::HeadMalformed { order_id, counter });
        return Ok(false);
    }
    if let Some(recorded) = recorded {
        reconcile_prefix(tx, target, &recorded, counter, head, findings).await?;
    }
    let member = CheckpointMember {
        order_id,
        audit_sequence: counter,
        entry_hash: head.clone(),
    };
    digest.push(&member)?;
    insert_audit_checkpoint_member(
        tx,
        target,
        audit_checkpoint_member::Model {
            audit_tenant_id: snapshot.namespace,
            checkpoint_sequence: snapshot.header.checkpoint_sequence,
            order_id: member.order_id,
            audit_sequence: member.audit_sequence,
            entry_hash: member.entry_hash,
        },
    )
    .await?;
    Ok(true)
}

async fn capture_in<'t>(
    tx: &'t DbTx<'t>,
    target: &'t TargetScope,
    hooks: &CaptureHooks,
    cancel: &CancellationToken,
) -> Result<CaptureOutcome, CaptureAbort> {
    let snapshot = match snapshot(tx, target).await? {
        Ok(snapshot) => snapshot,
        Err(finding) => return Err(CaptureAbort::Findings(vec![finding])),
    };
    let mut findings = Vec::new();
    let mut digest = CheckpointDigest::new(&snapshot.header)?;
    if let Some(barrier) = &hooks.after_snapshot {
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
        let evidence = page_evidence(tx, target, &page, &mut findings).await?;
        for order in &page {
            let recorded = recorded_for(&mut prev_members, order.order_id, &mut findings).await?;
            if findings.len() >= MAX_FINDINGS {
                return Err(CaptureAbort::Findings(findings));
            }
            let candidate = Candidate {
                order,
                head: evidence.heads.get(&order.order_id),
                recorded,
            };
            if reconcile_order(tx, target, &snapshot, candidate, &mut digest, &mut findings).await?
            {
                members += 1;
            }
        }
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
    hooks: &CaptureHooks,
    cancel: &CancellationToken,
) -> Result<CaptureOutcome, WorkerError> {
    let target = target.clone();
    let hooks = hooks.clone();
    let cancel = cancel.clone();
    let result: Result<CaptureOutcome, CaptureAbort> = db
        .transaction_ref_mapped_with_config(
            TxConfig::with_isolation(TxIsolationLevel::RepeatableRead),
            move |tx| Box::pin(async move { capture_in(tx, &target, &hooks, &cancel).await }),
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
    pub orders: u64,
    pub findings: Vec<IntegrityFinding>,
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
                    if findings.len() >= MAX_FINDINGS {
                        break;
                    }
                }
                let completed_pass = (page.len() as u64) < limit;
                next.after = page.last().map(|o| o.order_id);
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
}

/// Per-namespace state of this replica's audit worker.
#[derive(Debug, Default)]
pub struct AuditWorkerState {
    namespaces: HashMap<Uuid, NamespaceState>,
}

fn alert(
    namespace: Uuid,
    phase: &'static str,
    findings: &[IntegrityFinding],
    metrics: &dyn WorkerMetrics,
) {
    for finding in findings {
        tracing::error!(
            %namespace,
            phase,
            kind = finding.kind(),
            finding = ?finding,
            "bss-orders-lifecycle: AUDIT INTEGRITY MISMATCH (not repaired)"
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
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    let (_grant, db) = grant_for(parts, MaintenanceTask::AuditCheckpoint)?;
    let age = checkpoint_age(db, target).await?;
    if age.is_some_and(|age| age < parts.settings.checkpoint_interval) {
        parts
            .metrics
            .checkpoint(namespace, CheckpointResult::Current, 0, age);
        return Ok(());
    }
    if state
        .capture_held_until
        .is_some_and(|until| Instant::now() < until)
    {
        parts
            .metrics
            .checkpoint(namespace, CheckpointResult::Withheld, 0, age);
        return Ok(());
    }
    state.capture_held_until = None;
    match capture_checkpoint(db, target, &parts.capture_hooks, cancel).await {
        Ok(CaptureOutcome::Appended { sequence, members }) => {
            tracing::info!(%namespace, sequence, members, "bss-orders-lifecycle: audit checkpoint appended");
            parts.metrics.checkpoint(
                namespace,
                CheckpointResult::Appended,
                members,
                Some(Duration::ZERO),
            );
        }
        Ok(CaptureOutcome::Mismatch(findings)) => {
            alert(namespace, "checkpoint", &findings, parts.metrics.as_ref());
            state.capture_held_until = Some(Instant::now() + parts.settings.checkpoint_interval);
            parts
                .metrics
                .checkpoint(namespace, CheckpointResult::Mismatch, 0, age);
        }
        Ok(CaptureOutcome::Conflict) => {
            tracing::warn!(%namespace, "bss-orders-lifecycle: checkpoint append conflicted with a competing replica; rolled back");
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
    cursor: &mut VerifyCursor,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
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
    parts
        .metrics
        .verification(namespace, slice.orders, cursor.coverage_age());
    Ok(())
}

/// One namespace under its own advisory key: the checkpoint phase, then a verification slice.
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
    let state = state.namespaces.entry(namespace).or_default();
    let checkpoint_granted = parts
        .authority
        .grant(MaintenanceTask::AuditCheckpoint)
        .is_some();
    let verification_granted = parts
        .authority
        .grant(MaintenanceTask::AuditVerification)
        .is_some();
    guarded_pass(parts, WorkerKind::Audit, &key, cancel, |cancel| {
        Box::pin(async move {
            if checkpoint_granted {
                checkpoint_phase(parts, &target, namespace, state, cancel).await?;
            }
            if verification_granted {
                verification_phase(parts, &target, namespace, &mut state.cursor, cancel).await?;
            }
            Ok(())
        })
    })
    .await
}

/// One tick: enumerate namespaces holding committed evidence and run each namespace pass.
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
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let found = match discover_audit_namespaces(db, &grant, after, MAX_NAMESPACE_PAGE).await {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!(error = %error, "bss-orders-lifecycle: audit namespace discovery failed");
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
        }
        if (found.len() as u64) < MAX_NAMESPACE_PAGE {
            break;
        }
    }
}
