use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use construct_sdk::gts::{HAS_FACT_TYPE, PROFILE_ROOT_TYPE};
use graph_storage_sdk::GraphStorageClientV1;
use graph_storage_sdk::models::{EdgeSpec, IngestOptions, IngestRequest, NodeSpec};
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::fact::fact_node;
use super::reader::{MAX_FACTS, read_stored};
use super::root::{ProfileVersion, Root, root_key};
use super::types::GraphTypes;
use super::{DEFAULT_CALL_TIMEOUT, bounded, graph_error, subject_in_context};
use crate::domain::error::DomainError;
use crate::domain::plan::{Change, NodeKey, Plan};
use crate::domain::profile::{PlanWriter, Written};

/// The reason graph storage gives a failed compare-and-set.
const CAS_CONFLICT: &str = "CAS_CONFLICT";
/// The reason graph storage gives a write that lost a serialization race; it
/// committed nothing and can be sent again.
const SERIALIZATION: &str = "SERIALIZATION";

/// How many times one write is sent while its result is unknown.
pub const DEFAULT_ATTEMPTS: u32 = 3;
/// The pause before the second send; it doubles for every later one, and a
/// random share of it is added, so writers that failed together do not come
/// back together.
const FIRST_PAUSE: Duration = Duration::from_millis(100);

/// Removes a fact from graph storage once the write that dropped it has
/// committed. Soft delete until graph storage ships a hard delete (#5194);
/// that swap stays inside this interface.
#[async_trait]
pub trait FactRemoval: Send + Sync {
    /// Remove the fact under `key`. A fact that is already gone is removed.
    ///
    /// # Errors
    ///
    /// When graph storage cannot remove it now, or does not answer in time;
    /// the key stays listed on the root and a later write tries again.
    async fn remove(&self, ctx: &SecurityContext, key: &str) -> Result<(), DomainError>;
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
    async fn remove(&self, ctx: &SecurityContext, key: &str) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-remove
        match bounded(
            self.timeout,
            "fact removal",
            self.client.delete_node(ctx, &key.to_owned()),
        )
        .await?
        {
            // A fact already tombstoned answers with nothing deleted.
            Ok(_) | Err(CanonicalError::NotFound { .. }) => Ok(()),
            Err(err) => Err(graph_error("fact removal", &err)),
        }
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
    attempts: u32,
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
    pub fn with_attempts(mut self, attempts: u32) -> Self {
        self.attempts = attempts.max(1);
        self
    }

    /// Replace [`DEFAULT_CALL_TIMEOUT`] for the profile read and each send.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Replace [`MAX_FACTS`], the most facts one profile may hold. It cannot
    /// exceed what one read takes.
    #[must_use]
    pub fn with_max_facts(mut self, max_facts: usize) -> Self {
        self.max_facts = max_facts.min(MAX_FACTS);
        self
    }

    /// Remove the facts under `keys`. One that fails stays listed on the root.
    async fn remove(&self, ctx: &SecurityContext, keys: &BTreeSet<String>) {
        let mut failed = 0_usize;
        for key in keys {
            if let Err(err) = self.ports.removal.remove(ctx, key).await {
                failed += 1;
                tracing::warn!(%key, error = %err, "a removed fact stays listed on its root");
            }
        }
        if failed > 0 {
            tracing::warn!(
                failed,
                listed = keys.len(),
                "removed facts stay listed until a later write finishes them"
            );
        }
    }

    /// Send the ingest, again with the same idempotency key while its result
    /// is unknown, with a growing, jittered pause between sends.
    async fn ingest(
        &self,
        ctx: &SecurityContext,
        request: IngestRequest,
    ) -> Result<Written, DomainError> {
        let mut attempt = 1;
        let mut pause = FIRST_PAUSE;
        loop {
            // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-ingest
            let sent =
                tokio::time::timeout(self.timeout, self.ports.client.ingest(ctx, request.clone()))
                    .await;
            let failed = match sent {
                Ok(Ok(_)) => return Ok(Written::Stored),
                Ok(Err(CanonicalError::Aborted { ctx: aborted, .. }))
                    if aborted.reason == CAS_CONFLICT =>
                {
                    return Ok(Written::Conflict);
                }
                Ok(Err(err)) if !unknown_result(&err) => {
                    return Err(graph_error("profile write", &err));
                }
                Ok(Err(err)) => err.to_string(),
                // No answer in time: the write may or may not have committed.
                Err(_) => format!("no answer within {:?}", self.timeout),
            };
            if attempt >= self.attempts {
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

/// A random share of `pause`, up to all of it.
fn jitter(pause: Duration) -> Duration {
    let millis = u64::try_from(pause.as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(rand::random_range(0..=millis))
}

/// Whether the write may or may not have committed, or certainly did not and
/// can be sent as it is: unavailable, out of time, unknown, or a lost
/// serialization race. Sending it again with its key is safe in each case.
pub(super) fn unknown_result(err: &CanonicalError) -> bool {
    match err {
        CanonicalError::Aborted { ctx, .. } => ctx.reason == SERIALIZATION,
        CanonicalError::ServiceUnavailable { .. }
        | CanonicalError::DeadlineExceeded { .. }
        | CanonicalError::Unknown { .. } => true,
        _ => false,
    }
}

/// What a plan's steps write: the new fact nodes, their edges from the root,
/// and the keys of the facts they remove.
#[derive(Default)]
struct Changes {
    nodes: Vec<NodeSpec>,
    edges: Vec<EdgeSpec>,
    removals: BTreeSet<String>,
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
                changes.removals.insert(old.0.clone());
            }
            Change::AddEntity { .. }
            | Change::AddProperty { .. }
            | Change::ReplaceProperty { .. }
            | Change::RemoveProperty { .. } => {}
        }
    }
    Ok(changes)
}

#[async_trait]
impl PlanWriter for GraphPlanWriter {
    #[tracing::instrument(skip_all, fields(tenant_id = %plan.tenant_id, subject_id = %plan.subject_id))]
    async fn write(&self, ctx: &SecurityContext, plan: &Plan) -> Result<Written, DomainError> {
        if plan.steps.is_empty() {
            return Ok(Written::Stored);
        }
        let subject = subject_in_context(ctx, plan.tenant_id, plan.subject_id)?;
        self.ports.types.ensure(ctx).await?;

        // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-read-root
        let stored = read_stored(
            self.ports.client.as_ref(),
            self.ports.version.as_ref(),
            self.timeout,
            ctx,
            &subject,
        )
        .await?;
        if stored.version != plan.profile_version {
            return Ok(Written::Conflict);
        }
        let live: BTreeSet<&str> = stored
            .facts
            .iter()
            .map(|node| node.node_key.as_str())
            .collect();
        // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-read-root

        // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-pending
        // Earlier removals still listed: one whose fact is gone is done and
        // drops off the list. Only a live fact of this root is ever removed, so
        // a key that is not one of its facts is never deleted.
        let pending: BTreeSet<String> = stored
            .root
            .removals
            .into_iter()
            .filter(|key| live.contains(key.as_str()))
            .collect();
        // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-pending

        // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-batch
        let root = root_key(&subject);
        let Changes {
            mut nodes,
            edges,
            removals,
        } = changes(plan, &root, OffsetDateTime::now_utc())?;
        if let Some(key) = removals.iter().find(|key| !live.contains(key.as_str())) {
            return Err(DomainError::internal(format!(
                "the plan removes {key}, which is not a fact of the profile it was built on"
            )));
        }
        // A fact listed for removal still counts until its delete has run, so
        // a profile never grows past what one read takes, even when deletes
        // fail.
        if live.len() + nodes.len() > self.max_facts {
            return Err(DomainError::ProfileFull(format!(
                "the profile of subject {} holds {} facts; {} more would pass the limit of {}",
                plan.subject_id,
                live.len(),
                nodes.len(),
                self.max_facts
            )));
        }
        let listed: BTreeSet<String> = pending.into_iter().chain(removals).collect();
        let version = self.ports.version.next(stored.version)?;
        let payload = Root {
            removals: listed.iter().cloned().collect(),
        }
        .payload(plan.subject_id, self.ports.version.as_ref(), version)?;
        nodes.push(NodeSpec {
            node_key: root.0,
            type_id: PROFILE_ROOT_TYPE.to_owned(),
            name: None,
            payload: Some(payload),
            expected_version: Some(self.ports.version.expected(stored.version)?),
        });
        let request = IngestRequest {
            nodes,
            edges,
            options: IngestOptions::default(),
            replace_scope: None,
            idempotency_key: Some(Uuid::now_v7().to_string()),
        };
        // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-batch

        let written = self.ingest(ctx, request).await?;
        if written == Written::Stored {
            // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-cleanup
            // Committed: every listed fact is already left out of reads, so a
            // removal that fails now is finished by a later write.
            self.remove(ctx, &listed).await;
            // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-cleanup
        }
        Ok(written)
    }
}
