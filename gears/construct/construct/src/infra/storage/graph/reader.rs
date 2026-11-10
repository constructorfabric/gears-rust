use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use construct_sdk::gts::{HAS_FACT_TYPE, PROFILE_ROOT_TYPE};
use graph_storage_sdk::GraphStorageClientV1;
use graph_storage_sdk::models::{NodeView, TraverseRequest, TruncationReason};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::fact::{fact_of, stored_fact_of};
use super::root::{ProfileVersion, Root, root_key};
use super::{ContextSubject, DEFAULT_CALL_TIMEOUT, graph_error, subject_in_context, unanswered};
use crate::domain::error::DomainError;
use crate::domain::plan::NodeKey;
use crate::domain::planner::processing::ProfileSource;
use crate::domain::profile::{FactCount, FullProfile, Profile};
use crate::domain::profile_read::{StoredFact, StoredProfiles};
use crate::domain::profile_writes::{SubjectFacts, SubjectProfiles};
use crate::domain::subject_settings::SubjectKey;

/// The most nodes one profile read takes: the root and its facts. Graph
/// storage's ceiling for a traversal, `traversal_max_nodes`, is 1000 by
/// default and must not be set lower for Construct: graph storage refuses a
/// read that asks for more, see [`read_refused`].
pub const MAX_PROFILE_NODES: u32 = 1000;

/// The most facts one profile holds: every fact, the root besides, has to fit
/// in one read.
pub const MAX_FACTS: usize = MAX_PROFILE_NODES as usize - 1;

/// How often a profile read is sent when graph storage answers that the read
/// saw more than one graph state: a write landed while it ran.
const SNAPSHOT_TRIES: u32 = 3;

/// A profile as graph storage holds it: the root's version and removal list,
/// none for a subject without a profile, and every live fact node, also those
/// the root lists for removal whose soft delete has not run yet.
pub(crate) struct Stored {
    pub version: Option<u64>,
    pub root: Root,
    pub facts: Vec<NodeView>,
}

/// A failed profile read. Out of range is graph storage refusing the read's
/// node budget: its `traversal_max_nodes` is set below what one profile read
/// takes, which only the deployment can mend.
fn read_refused(err: &CanonicalError) -> DomainError {
    match err {
        CanonicalError::OutOfRange { .. } => DomainError::internal(format!(
            "graph storage takes fewer nodes in one traversal than a profile read needs \
             ({MAX_PROFILE_NODES}); set its traversal_max_nodes to {MAX_PROFILE_NODES} or more: {err}"
        )),
        _ => graph_error("profile read", err),
    }
}

/// Read the root and every node one has-fact edge from it, in one traversal.
///
/// # Errors
///
/// [`DomainError::ProfileFull`] when the profile is larger than one read
/// takes; internal when graph storage cut the read short for another reason
/// or refuses the read's node budget; unavailable when graph storage does not
/// answer in time or no read of [`SNAPSHOT_TRIES`] saw one graph state; any
/// other failure as [`super::graph_error`] maps it.
pub(crate) async fn read_stored(
    client: &dyn GraphStorageClientV1,
    versions: &dyn ProfileVersion,
    timeout: Duration,
    ctx: &SecurityContext,
    subject: &ContextSubject,
) -> Result<Stored, DomainError> {
    // @cpt-begin:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-traverse
    let subject = subject.key();
    let NodeKey(key) = root_key(subject);
    let request = TraverseRequest {
        seeds: vec![key.clone()],
        depth: 1,
        edge_type_patterns: vec![HAS_FACT_TYPE.to_owned()],
        node_type_patterns: Vec::new(),
        max_nodes: Some(MAX_PROFILE_NODES),
        // Both directions and every relation, as before these existed.
        direction: None,
        edge_discriminators: Vec::new(),
    };
    // Without a snapshot, the root and the facts may come from different
    // graph states: a fresh version with stale facts would pass the write's
    // compare-and-set. Such a read is sent again.
    let mut tries = 1;
    let response = loop {
        let response = tokio::time::timeout(timeout, client.traverse(ctx, request.clone()))
            .await
            .map_err(|_| unanswered("profile read", timeout))?
            .map_err(|err| read_refused(&err))?;
        if response.consistent_snapshot {
            break response;
        }
        if tries == SNAPSHOT_TRIES {
            return Err(DomainError::Unavailable(format!(
                "no profile read of subject {} saw one graph state in {SNAPSHOT_TRIES} tries",
                subject.subject_id
            )));
        }
        tracing::debug!(
            tries,
            "construct: a profile read saw more than one graph state"
        );
        tries += 1;
    };
    // @cpt-end:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-traverse

    // @cpt-begin:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-root
    let Some(root_node) = response.nodes.iter().find(|node| node.node_key == key) else {
        return Ok(Stored {
            version: None,
            root: Root::default(),
            facts: Vec::new(),
        });
    };
    // A cut profile would let the planner add what is already stored. Only a
    // node budget means the profile holds too many facts; a cut for any other
    // reason is a limit of graph storage's, not a full profile.
    match response.truncated {
        None => {}
        Some(TruncationReason::NodeBudget) => {
            // The read took the root and as many facts as it could, and
            // there were more.
            return Err(DomainError::ProfileFull(FullProfile {
                subject: *subject,
                facts: FactCount::AtLeast(response.nodes.len()),
                limit: MAX_FACTS,
            }));
        }
        Some(reason) => {
            return Err(DomainError::internal(format!(
                "graph storage cut the profile read of subject {} short: {reason:?}",
                subject.subject_id
            )));
        }
    }
    // Graph storage keys are unique across types, so another type can hold
    // the root's key; its node is no root, and its edges decide nothing.
    if root_node.type_id != PROFILE_ROOT_TYPE {
        return Err(DomainError::internal(format!(
            "the key of profile root {key} is held by a node of type {}",
            root_node.type_id
        )));
    }
    let root = Root::from_payload(
        &NodeKey(key.clone()),
        subject.subject_id,
        root_node.payload.as_ref(),
    )?;
    let version = versions.read(root_node)?;
    // @cpt-end:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-root
    // The traversal walks edges both ways and matches derived edge types, so a
    // fact is only a node that a has-fact edge from this root points to. One of
    // no person type is kept, so the read reports it and the writer counts it.
    let pointed: BTreeSet<&str> = response
        .edges
        .iter()
        .filter(|edge| edge.src == key && edge.edge_type_id == HAS_FACT_TYPE)
        .map(|edge| edge.dst.as_str())
        .collect();
    let facts = response
        .nodes
        .into_iter()
        .filter(|node| pointed.contains(node.node_key.as_str()))
        .collect();
    Ok(Stored {
        version: Some(version),
        root,
        facts,
    })
}

/// [`ProfileSource`] and [`StoredProfiles`] over graph storage: the root and
/// every fact one has-fact edge away from it, in one traversal. A fact listed
/// for removal on the root is left out, also before its soft delete has run.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-writer-read:p1
pub struct GraphProfileSource {
    client: Arc<dyn GraphStorageClientV1>,
    version: Arc<dyn ProfileVersion>,
    timeout: Duration,
}

impl GraphProfileSource {
    #[must_use]
    pub fn new(client: Arc<dyn GraphStorageClientV1>, version: Arc<dyn ProfileVersion>) -> Self {
        Self {
            client,
            version,
            timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    /// Replace [`DEFAULT_CALL_TIMEOUT`] for the read.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[async_trait]
impl ProfileSource for GraphProfileSource {
    #[tracing::instrument(skip_all, fields(%tenant_id, %subject_id))]
    async fn read(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        subject_id: Uuid,
    ) -> Result<Profile, DomainError> {
        let subject = subject_in_context(
            ctx,
            SubjectKey {
                tenant_id,
                subject_id,
            },
        )?;
        let stored = read_stored(
            self.client.as_ref(),
            self.version.as_ref(),
            self.timeout,
            ctx,
            &subject,
        )
        .await?;

        // @cpt-begin:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-facts
        let listed = &stored.root.removals;
        let facts = stored
            .facts
            .iter()
            .filter(|node| !listed.contains(node.node_key.as_str()))
            .map(fact_of)
            .collect::<Result<_, _>>()?;
        Ok(Profile {
            version: stored.version,
            facts,
        })
        // @cpt-end:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-facts
    }
}

/// @cpt-dod:cpt-cf-construct-dod-profile-reader-stored:p1
#[async_trait]
impl StoredProfiles for GraphProfileSource {
    #[tracing::instrument(skip_all, fields(tenant_id = %key.tenant_id, subject_id = %key.subject_id))]
    async fn stored(
        &self,
        ctx: &SecurityContext,
        key: SubjectKey,
    ) -> Result<Vec<StoredFact>, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-stored
        let subject = subject_in_context(ctx, key)?;
        let stored = read_stored(
            self.client.as_ref(),
            self.version.as_ref(),
            self.timeout,
            ctx,
            &subject,
        )
        .await?;
        Ok(served(&stored))
        // @cpt-end:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-stored
    }
}

/// The facts a reader is served: every live fact the root does not list for
/// removal. A malformed fact is left out here, while the planner's read fails
/// on it: a reader is better served by the rest of the profile, whereas a
/// plan built on a partial profile could add what is stored.
fn served(stored: &Stored) -> Vec<StoredFact> {
    let listed = &stored.root.removals;
    stored
        .facts
        .iter()
        .filter(|node| !listed.contains(node.node_key.as_str()))
        .filter_map(|node| {
            stored_fact_of(node)
                .inspect_err(|defect| {
                    // Only Construct writes these nodes, so this is a
                    // defect, not a state a reader should see in silence.
                    tracing::error!(%defect, "a stored fact cannot be read; it is not served");
                })
                .ok()
        })
        .collect()
}

/// @cpt-dod:cpt-cf-construct-dod-subject-control-view:p1
#[async_trait]
impl SubjectProfiles for GraphProfileSource {
    #[tracing::instrument(skip_all, fields(tenant_id = %key.tenant_id, subject_id = %key.subject_id))]
    async fn facts(
        &self,
        ctx: &SecurityContext,
        key: SubjectKey,
    ) -> Result<SubjectFacts, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-subject-control-read-facts:p1:inst-facts-read
        let subject = subject_in_context(ctx, key)?;
        let stored = read_stored(
            self.client.as_ref(),
            self.version.as_ref(),
            self.timeout,
            ctx,
            &subject,
        )
        .await?;
        // Unlike the profile read, a fact that cannot be read fails the read:
        // the subject must see, export and delete every fact it has.
        let listed = &stored.root.removals;
        let facts = stored
            .facts
            .iter()
            .filter(|node| !listed.contains(node.node_key.as_str()))
            .map(stored_fact_of)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SubjectFacts {
            version: stored.version,
            facts,
        })
        // @cpt-end:cpt-cf-construct-algo-subject-control-read-facts:p1:inst-facts-read
    }
}
