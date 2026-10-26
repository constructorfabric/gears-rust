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
use super::graph_error;
use super::root::{ProfileVersion, Root, root_key};
use super::types::GraphTypes;
use crate::domain::error::DomainError;
use crate::domain::plan::{Change, Plan, Property};
use crate::domain::profile::{PlanWriter, Written};

/// The reason graph storage gives a failed compare-and-set.
const CAS_CONFLICT: &str = "CAS_CONFLICT";
/// The reason graph storage gives a write that lost a serialization race; it
/// committed nothing and can be sent again.
const SERIALIZATION: &str = "SERIALIZATION";

/// How many times one write is sent while its result is unknown.
pub const DEFAULT_ATTEMPTS: u32 = 3;
/// How long to wait before sending a write again.
const RETRY_PAUSE: Duration = Duration::from_millis(200);

/// Removes a fact from graph storage once the write that dropped it has
/// committed. Soft delete until graph storage ships a hard delete
/// (gears-rust #5194); that swap stays inside this interface.
#[async_trait]
pub trait FactRemoval: Send + Sync {
    /// Remove the fact under `key`. A fact that is already gone is removed.
    ///
    /// # Errors
    ///
    /// When graph storage cannot remove it now; the key stays listed on the
    /// root and the next write tries again.
    async fn remove(&self, ctx: &SecurityContext, key: &str) -> Result<(), DomainError>;
}

/// [`FactRemoval`] as graph storage's soft delete, which tombstones the node
/// and its edges.
pub struct SoftDelete {
    client: Arc<dyn GraphStorageClientV1>,
}

impl SoftDelete {
    #[must_use]
    pub fn new(client: Arc<dyn GraphStorageClientV1>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl FactRemoval for SoftDelete {
    async fn remove(&self, ctx: &SecurityContext, key: &str) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-remove
        match self.client.delete_node(ctx, &key.to_owned()).await {
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
}

impl GraphPlanWriter {
    #[must_use]
    pub fn new(ports: WriterPorts) -> Self {
        Self {
            ports,
            attempts: DEFAULT_ATTEMPTS,
        }
    }

    /// Replace [`DEFAULT_ATTEMPTS`].
    #[must_use]
    pub fn with_attempts(mut self, attempts: u32) -> Self {
        self.attempts = attempts.max(1);
        self
    }

    /// The stored root of the subject, none for a subject without a profile.
    async fn root(&self, ctx: &SecurityContext, key: &str) -> Result<Option<Root>, DomainError> {
        match self
            .ports
            .client
            .get_node(ctx, &key.to_owned(), Some(1))
            .await
        {
            Ok(node) => {
                let root = Root::from_payload(node.payload.as_ref())?;
                // The version comes through the interface, so a source that
                // reads graph storage's own version can replace the counter.
                let version = self.ports.version.read(node.payload.as_ref())?;
                Ok(Some(Root { version, ..root }))
            }
            Err(CanonicalError::NotFound { .. }) => Ok(None),
            Err(err) => Err(graph_error("profile root read", &err)),
        }
    }

    /// Remove `keys`, and give back the ones that could not be removed now.
    async fn remove(&self, ctx: &SecurityContext, keys: Vec<String>) -> Vec<String> {
        let mut pending = Vec::new();
        for key in keys {
            if let Err(err) = self.ports.removal.remove(ctx, &key).await {
                tracing::warn!(error = %err, "a removed fact stays listed on its root until the next write");
                pending.push(key);
            }
        }
        pending
    }

    /// Send the ingest, again with the same idempotency key while its result
    /// is unknown.
    async fn ingest(
        &self,
        ctx: &SecurityContext,
        request: IngestRequest,
    ) -> Result<Written, DomainError> {
        let mut attempt = 1;
        loop {
            // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-ingest
            match self.ports.client.ingest(ctx, request.clone()).await {
                Ok(_) => return Ok(Written::Stored),
                Err(CanonicalError::Aborted { ctx: aborted, .. })
                    if aborted.reason == CAS_CONFLICT =>
                {
                    return Ok(Written::Conflict);
                }
                Err(err) if attempt < self.attempts && unknown_result(&err) => {
                    tracing::debug!(attempt, error = %err, "profile write sent again with its key");
                    attempt += 1;
                    tokio::time::sleep(RETRY_PAUSE).await;
                }
                Err(err) => return Err(graph_error("profile write", &err)),
            }
            // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-ingest
        }
    }
}

/// Whether the write may or may not have committed, or certainly did not and
/// can be sent as it is: unavailable, out of time, unknown, or a lost
/// serialization race. Sending it again with its key is safe in each case.
fn unknown_result(err: &CanonicalError) -> bool {
    match err {
        CanonicalError::Aborted { ctx, .. } => ctx.reason == SERIALIZATION,
        CanonicalError::ServiceUnavailable { .. }
        | CanonicalError::DeadlineExceeded { .. }
        | CanonicalError::Unknown { .. } => true,
        _ => false,
    }
}

/// The one property of a fact node's change.
fn single(properties: &[Property]) -> Result<&Property, DomainError> {
    match properties {
        [property] => Ok(property),
        _ => Err(DomainError::internal(format!(
            "a fact node holds one property, the plan gives {}",
            properties.len()
        ))),
    }
}

/// What a plan's steps write: the new fact nodes, their edges from the root,
/// and the keys of the facts they remove.
#[derive(Default)]
struct Changes {
    nodes: Vec<NodeSpec>,
    edges: Vec<EdgeSpec>,
    removals: Vec<String>,
}

/// The changes of a plan's steps.
fn changes(plan: &Plan, root: &str, now: OffsetDateTime) -> Result<Changes, DomainError> {
    let mut changes = Changes::default();
    for step in &plan.steps {
        let (added, removed) = match &step.change {
            Change::AddEntity { key, properties } => (Some((key, single(properties)?)), None),
            Change::ReplaceEntity {
                old,
                key,
                properties,
            } => (Some((key, single(properties)?)), Some(old)),
            Change::RemoveEntity { entity } => (None, Some(entity)),
            // One node holds one fact, so a property is never changed on
            // its own: the planner replaces the fact's node.
            other => {
                return Err(DomainError::internal(format!(
                    "the writer stores whole facts, not {other:?}"
                )));
            }
        };
        if let Some((key, property)) = added {
            changes
                .nodes
                .push(fact_node(plan, step, &key.0, property, now)?);
            changes.edges.push(EdgeSpec {
                type_id: HAS_FACT_TYPE.to_owned(),
                src_node_key: root.to_owned(),
                dst_node_key: key.0.clone(),
                discriminator: None,
                payload: None,
            });
        }
        if let Some(old) = removed {
            changes.removals.push(old.0.clone());
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
        // Graph storage writes into the context's tenant; the root key names
        // the plan's. They must be the same tenant.
        if ctx.subject_tenant_id() != plan.tenant_id {
            return Err(DomainError::internal(
                "the plan's tenant is not the tenant processing writes for",
            ));
        }
        self.ports.types.ensure(ctx).await?;

        // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-read-root
        let root_key = root_key(plan.tenant_id, plan.subject_id);
        let stored = self.root(ctx, &root_key).await?;
        let version = stored.as_ref().map(|root| root.version);
        if version != plan.profile_version {
            return Ok(Written::Conflict);
        }
        // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-read-root

        // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-pending
        // Removals an earlier write committed but could not finish.
        let pending = match stored {
            Some(root) => self.remove(ctx, root.removals).await,
            None => Vec::new(),
        };
        // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-pending

        // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-batch
        let Changes {
            mut nodes,
            edges,
            removals,
        } = changes(plan, &root_key, OffsetDateTime::now_utc())?;
        let root = Root {
            version: self.ports.version.next(version),
            removals: pending
                .into_iter()
                .chain(removals.iter().cloned())
                .collect(),
        };
        nodes.push(NodeSpec {
            node_key: root_key,
            type_id: PROFILE_ROOT_TYPE.to_owned(),
            name: None,
            payload: Some(root.payload(plan.subject_id)),
            expected_version: Some(self.ports.version.expected(version)),
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
            // Committed: the removed facts are already listed on the root, so
            // a removal that fails now is finished by the next write.
            self.remove(ctx, removals).await;
            // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-cleanup
        }
        Ok(written)
    }
}
