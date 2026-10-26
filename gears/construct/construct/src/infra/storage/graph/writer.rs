use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use construct_sdk::gts::{HAS_FACT_TYPE, PROFILE_ROOT_TYPE};
use graph_storage_sdk::GraphStorageClientV1;
use graph_storage_sdk::models::{EdgeSpec, IngestOptions, IngestOutcome, IngestRequest, NodeSpec};
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::fact::fact_node;
use super::reader::{MAX_FACTS, Stored, read_stored};
use super::root::{ProfileVersion, Root, root_key};
use super::types::GraphTypes;
use super::{
    CAS_CONFLICT, ContextSubject, DEFAULT_CALL_TIMEOUT, bounded, graph_error, subject_in_context,
};
use crate::domain::error::DomainError;
use crate::domain::plan::{Change, NodeKey, Plan};
use crate::domain::planner::processing::{PlanWriter, Written};
use crate::domain::profile::{FactCount, FullProfile};
use crate::domain::subject_settings::SubjectKey;

/// The reason graph storage gives a write that lost a serialization race; it
/// committed nothing and can be sent again.
const SERIALIZATION: &str = "SERIALIZATION";

/// How many times one write is sent while its result is unknown.
pub const DEFAULT_ATTEMPTS: NonZeroU32 = NonZeroU32::MIN.saturating_add(2);
/// The pause before the second send; it doubles for every later one, and a
/// random share of it is added, so writers that failed together do not come
/// back together.
const FIRST_PAUSE: Duration = Duration::from_millis(100);

/// Removes a fact from graph storage once the write that dropped it has
/// committed. Soft delete until graph storage ships a hard delete (#5194);
/// that swap stays inside this interface.
#[async_trait]
pub trait FactRemoval: Send + Sync {
    /// Remove the fact under `key`. A fact already removed is removed.
    ///
    /// # Errors
    ///
    /// When graph storage cannot remove it now, does not answer in time, or
    /// finds no fact under `key`; the key stays listed on the root and a
    /// later write tries again.
    // cancel-safe: the writer drops a removal that runs past its timeout. A
    // dropped removal has either tombstoned the fact or not; its key stays
    // listed on the root either way, and removing it again is safe.
    async fn remove(&self, ctx: &SecurityContext, key: &NodeKey) -> Result<(), DomainError>;
}

/// [`FactRemoval`] as graph storage's soft delete, which tombstones the node
/// and its edges.
pub struct SoftDelete {
    client: Arc<dyn GraphStorageClientV1>,
    timeout: Duration,
}

impl SoftDelete {
    #[must_use]
    pub fn new(client: Arc<dyn GraphStorageClientV1>) -> Self {
        Self {
            client,
            timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    /// Replace [`DEFAULT_CALL_TIMEOUT`] for one delete.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[async_trait]
impl FactRemoval for SoftDelete {
    async fn remove(&self, ctx: &SecurityContext, key: &NodeKey) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-remove
        // A fact already tombstoned answers with nothing deleted. Not found
        // means graph storage holds no such fact for this context, which a
        // listed key of a live fact never is, so it is no success.
        bounded(
            self.timeout,
            &format!("removal of fact {}", key.0),
            self.client.delete_node(ctx, &key.0),
        )
        .await?;
        Ok(())
        // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-remove
    }
}

/// The collaborators of the writer.
pub struct WriterPorts {
    pub client: Arc<dyn GraphStorageClientV1>,
    pub types: Arc<GraphTypes>,
    pub version: Arc<dyn ProfileVersion>,
    pub removal: Arc<dyn FactRemoval>,
}

/// [`PlanWriter`] over graph storage: one ingest per plan, with the root's
/// expected version as the compare-and-set of the whole profile.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-writer-write:p1
pub struct GraphPlanWriter {
    ports: WriterPorts,
    attempts: NonZeroU32,
    timeout: Duration,
    max_facts: usize,
}

impl GraphPlanWriter {
    #[must_use]
    pub fn new(ports: WriterPorts) -> Self {
        Self {
            ports,
            attempts: DEFAULT_ATTEMPTS,
            timeout: DEFAULT_CALL_TIMEOUT,
            max_facts: MAX_FACTS,
        }
    }

    /// Replace [`DEFAULT_ATTEMPTS`].
    #[must_use]
    pub fn with_attempts(mut self, attempts: NonZeroU32) -> Self {
        self.attempts = attempts;
        self
    }

    /// Replace [`DEFAULT_CALL_TIMEOUT`] for the profile read and each send.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Replace [`MAX_FACTS`], the most facts one profile may hold.
    ///
    /// # Errors
    ///
    /// When `max_facts` is more than one read takes, [`MAX_FACTS`]: such a
    /// profile could not be read whole.
    pub fn with_max_facts(mut self, max_facts: usize) -> Result<Self, DomainError> {
        if max_facts > MAX_FACTS {
            return Err(DomainError::internal(format!(
                "a profile of {max_facts} facts is more than one read takes ({MAX_FACTS})"
            )));
        }
        self.max_facts = max_facts;
        Ok(self)
    }

    /// Remove the facts under `keys`, one after another, within one call
    /// timeout. The first that fails ends the cleanup: it and the rest stay
    /// listed on the root, and a later write finishes them.
    async fn remove(&self, ctx: &SecurityContext, keys: &BTreeSet<NodeKey>) {
        let cleanup = async {
            for (done, key) in keys.iter().enumerate() {
                if let Err(err) = self.ports.removal.remove(ctx, key).await {
                    return Err((done, key, err));
                }
            }
            Ok(())
        };
        match tokio::time::timeout(self.timeout, cleanup).await {
            Ok(Ok(())) => {}
            Ok(Err((done, key, err))) => unfinished(key, &err, keys.len() - done),
            Err(_) => tracing::warn!(
                listed = keys.len(),
                timeout = ?self.timeout,
                "removed facts stay listed on the root until a later write finishes them"
            ),
        }
    }

    /// Send the ingest, again with the same idempotency key while its result
    /// is unknown, with a growing, jittered pause between sends.
    ///
    /// A send without an answer keeps running in graph storage, which cannot
    /// cut off a statement it has started. A later send of the same request
    /// can miss that send's receipt, wait for the root, and lose the
    /// compare-and-set to it. So once a result was unknown, a refused
    /// compare-and-set is sent once more with the same key: when the earlier
    /// send committed, its receipt now answers stored.
    async fn ingest(
        &self,
        ctx: &SecurityContext,
        request: IngestRequest,
    ) -> Result<Written, DomainError> {
        let mut attempt = 1;
        let mut pause = FIRST_PAUSE;
        let mut unknown = false;
        let mut replayed = false;
        loop {
            // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-ingest
            let sent =
                tokio::time::timeout(self.timeout, self.ports.client.ingest(ctx, request.clone()))
                    .await;
            let failed = match classify(sent, self.timeout) {
                Sent::Stored => return Ok(Written::Stored),
                Sent::Failed(err) => return Err(err),
                Sent::Refused if unknown && !replayed => {
                    replayed = true;
                    tracing::warn!(
                        idempotency_key = ?request.idempotency_key,
                        "profile write refused after a send without an answer; sent once more \
                         in case that send committed"
                    );
                    continue;
                }
                Sent::Refused => return Ok(Written::Conflict),
                Sent::Unknown(failed) => failed,
            };
            unknown = true;
            if attempt >= self.attempts.get() {
                return Err(DomainError::Unavailable(format!(
                    "graph storage profile write: result unknown after {attempt} sends: {failed}"
                )));
            }
            tracing::warn!(
                attempt,
                idempotency_key = ?request.idempotency_key,
                error = %failed,
                "profile write sent again with its idempotency key"
            );
            tokio::time::sleep(pause + jitter(pause)).await;
            attempt += 1;
            pause = pause.saturating_mul(2);
            // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-ingest
        }
    }
}

/// Log a cleanup that stopped at `key`, with `left` removals not done.
fn unfinished(key: &NodeKey, err: &DomainError, left: usize) {
    if matches!(err, DomainError::Internal(_)) {
        // A defect, such as a missing grant for the delete, is not cured by a
        // later write, and the listed facts keep counting toward the limit
        // until it is mended.
        tracing::error!(
            key = %key.0,
            error = %err,
            left,
            "removed facts stay listed on the root: their delete fails"
        );
    } else {
        tracing::warn!(
            key = %key.0,
            error = %err,
            left,
            "removed facts stay listed on the root until a later write finishes them"
        );
    }
}

/// What one send of the ingest gave.
enum Sent {
    Stored,
    /// Graph storage refused the compare-and-set.
    Refused,
    /// The write may or may not have committed, or certainly did not and
    /// can be sent as it is; the reason, for the log.
    Unknown(String),
    Failed(DomainError),
}

fn classify(
    sent: Result<Result<IngestOutcome, CanonicalError>, tokio::time::error::Elapsed>,
    timeout: Duration,
) -> Sent {
    match sent {
        Ok(Ok(_)) => Sent::Stored,
        Ok(Err(CanonicalError::Aborted { ctx: aborted, .. })) if aborted.reason == CAS_CONFLICT => {
            Sent::Refused
        }
        Ok(Err(err)) if resendable(&err) => Sent::Unknown(err.to_string()),
        Ok(Err(err)) => Sent::Failed(graph_error("profile write", &err)),
        // No answer in time: the write may or may not have committed.
        Err(_) => Sent::Unknown(format!("no answer within {timeout:?}")),
    }
}

/// A random share of `pause`, up to all of it.
pub(super) fn jitter(pause: Duration) -> Duration {
    let millis = u64::try_from(pause.as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(rand::random_range(0..=millis))
}

/// Whether a failed send can be sent again as it is, with its key: graph
/// storage was unavailable or out of time, so the write may or may not have
/// committed, or it lost a serialization race and certainly did not. An
/// unknown error is not among them: it says nothing about the write, so it is
/// a defect to report, not a state to wait out.
pub(super) fn resendable(err: &CanonicalError) -> bool {
    match err {
        CanonicalError::Aborted { ctx, .. } => ctx.reason == SERIALIZATION,
        CanonicalError::ServiceUnavailable { .. } | CanonicalError::DeadlineExceeded { .. } => true,
        _ => false,
    }
}

/// What a plan's steps write: the new fact nodes, their edges from the root,
/// and the keys of the facts they remove.
#[derive(Default)]
struct Changes {
    nodes: Vec<NodeSpec>,
    edges: Vec<EdgeSpec>,
    removals: BTreeSet<NodeKey>,
}

/// The changes of a plan's steps.
fn changes(plan: &Plan, root: &NodeKey, now: OffsetDateTime) -> Result<Changes, DomainError> {
    let mut changes = Changes::default();
    for step in &plan.steps {
        if let Some(node) = fact_node(plan, step, now)? {
            changes.edges.push(EdgeSpec {
                type_id: HAS_FACT_TYPE.to_owned(),
                src_node_key: root.0.clone(),
                dst_node_key: node.node_key.clone(),
                discriminator: None,
                payload: None,
            });
            changes.nodes.push(node);
        }
        match &step.change {
            Change::ReplaceEntity { old, .. } | Change::RemoveEntity { entity: old } => {
                changes.removals.insert(old.clone());
            }
            Change::AddEntity { .. }
            | Change::AddProperty { .. }
            | Change::ReplaceProperty { .. }
            | Change::RemoveProperty { .. } => {}
        }
    }
    Ok(changes)
}

/// One plan's ingest, and the removals the root lists after it.
struct Batch {
    request: IngestRequest,
    listed: BTreeSet<NodeKey>,
}

/// The removals the root still lists whose fact is live. One whose fact is
/// gone is done and drops off the list. Only a live fact of this root is
/// ever removed, so a key that is not one of its facts is never deleted.
fn pending(stored: &Stored) -> BTreeSet<NodeKey> {
    stored
        .facts
        .iter()
        .filter(|node| stored.root.removals.contains(node.node_key.as_str()))
        .map(|node| NodeKey(node.node_key.clone()))
        .collect()
}

/// The ingest of `plan` on the profile it was built on: its fact nodes, their
/// edges from the root, and the root with the next version and every removal
/// still to run.
///
/// # Errors
///
/// [`DomainError::ProfileFull`] when the new facts would pass `max_facts`;
/// internal when the plan removes a key that is no live fact of the profile,
/// or a fact or the root cannot be built.
fn batch(
    stored: &Stored,
    subject: &SubjectKey,
    plan: &Plan,
    versions: &dyn ProfileVersion,
    max_facts: usize,
    now: OffsetDateTime,
) -> Result<Batch, DomainError> {
    let live: BTreeSet<&str> = stored
        .facts
        .iter()
        .map(|node| node.node_key.as_str())
        .collect();

    // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-pending
    let mut listed = pending(stored);
    // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-pending

    // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-batch
    let root = root_key(subject);
    let Changes {
        mut nodes,
        edges,
        removals,
    } = changes(plan, &root, now)?;
    if let Some(key) = removals.iter().find(|key| !live.contains(key.0.as_str())) {
        return Err(DomainError::internal(format!(
            "the plan removes {}, which is not a fact of the profile it was built on",
            key.0
        )));
    }
    // A fact listed for removal still counts until its delete has run, so a
    // profile never grows past what one read takes, even when deletes fail.
    let facts = live.len() + nodes.len();
    if facts > max_facts {
        return Err(DomainError::ProfileFull(FullProfile {
            subject: *subject,
            facts: FactCount::WouldHold(facts),
            limit: max_facts,
        }));
    }
    listed.extend(removals);
    let root_now = Root { removals: listed };
    let payload = root_now.payload(subject.subject_id, versions, versions.next(stored.version)?)?;
    nodes.push(NodeSpec {
        node_key: root.0,
        type_id: PROFILE_ROOT_TYPE.to_owned(),
        name: None,
        payload: Some(payload),
        expected_version: Some(versions.expected(stored.version)?),
    });
    Ok(Batch {
        request: IngestRequest {
            nodes,
            edges,
            options: IngestOptions::default(),
            replace_scope: None,
            idempotency_key: Some(Uuid::now_v7().to_string()),
        },
        listed: root_now.removals,
    })
    // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-batch
}

impl GraphPlanWriter {
    /// The profile as graph storage holds it now.
    async fn read(
        &self,
        ctx: &SecurityContext,
        subject: &ContextSubject,
    ) -> Result<Stored, DomainError> {
        read_stored(
            self.ports.client.as_ref(),
            self.ports.version.as_ref(),
            self.timeout,
            ctx,
            subject,
        )
        .await
    }

    /// A compare-and-set refused after a read that matched the plan. Every
    /// write of Construct's moves the counter, so a moved counter is another
    /// write, a conflict. A counter that has not moved is a defect: the root
    /// was written outside Construct or its key is held by a removed node, so
    /// every later write is refused too, or a key a new fact claims is
    /// already held.
    async fn refused(
        &self,
        ctx: &SecurityContext,
        subject: &ContextSubject,
        expected: Option<u64>,
    ) -> Result<Written, DomainError> {
        let now = self.read(ctx, subject).await?.version;
        if now == expected {
            return Err(DomainError::internal(format!(
                "graph storage refused the write of profile root {} while its counter \
                 {expected:?} did not move: the root was written outside Construct, its key is \
                 held by a removed node, or a key a new fact claims is already held",
                root_key(subject.key()).0
            )));
        }
        tracing::warn!(
            expected = ?expected,
            stored = ?now,
            "the profile changed between its read and its write"
        );
        Ok(Written::Conflict)
    }
}

#[async_trait]
impl PlanWriter for GraphPlanWriter {
    #[tracing::instrument(skip_all, fields(tenant_id = %plan.tenant_id, subject_id = %plan.subject_id))]
    async fn write(&self, ctx: &SecurityContext, plan: &Plan) -> Result<Written, DomainError> {
        if plan.steps.is_empty() {
            return Ok(Written::Stored);
        }
        let subject = subject_in_context(
            ctx,
            SubjectKey {
                tenant_id: plan.tenant_id,
                subject_id: plan.subject_id,
            },
        )?;
        self.ports.types.ensure(ctx).await?;

        // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-read-root
        let stored = self.read(ctx, &subject).await?;
        let expected = stored.version;
        if expected != plan.profile_version {
            tracing::debug!(
                stored = ?expected,
                planned = ?plan.profile_version,
                "the plan was built on an older profile"
            );
            return Ok(Written::Conflict);
        }
        // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-read-root

        let build = |stored: &Stored| {
            batch(
                stored,
                subject.key(),
                plan,
                self.ports.version.as_ref(),
                self.max_facts,
                OffsetDateTime::now_utc(),
            )
        };
        let waiting = pending(&stored);
        let Batch { request, listed } = match build(&stored) {
            // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-full
            // Facts listed for removal count until their delete has run, and
            // only a stored write runs it. Run those deletes now and build
            // once more, so failed deletes do not keep the profile full.
            Err(DomainError::ProfileFull(_)) if !waiting.is_empty() => {
                self.remove(ctx, &waiting).await;
                let stored = self.read(ctx, &subject).await?;
                if stored.version != expected {
                    return Ok(Written::Conflict);
                }
                build(&stored)?
            }
            // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-full
            built => built?,
        };
        match self.ingest(ctx, request).await? {
            Written::Stored => {
                // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-cleanup
                // Committed: every listed fact is already left out of reads, so
                // a removal that fails now is finished by a later write.
                self.remove(ctx, &listed).await;
                // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-cleanup
                Ok(Written::Stored)
            }
            Written::Conflict => self.refused(ctx, &subject, expected).await,
        }
    }
}
