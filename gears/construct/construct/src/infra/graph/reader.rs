use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use construct_sdk::gts::HAS_FACT_TYPE;
use graph_storage_sdk::GraphStorageClientV1;
use graph_storage_sdk::models::{NodeView, TraverseRequest};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::fact::fact_of;
use super::root::{ProfileVersion, Root, root_key};
use super::{DEFAULT_CALL_TIMEOUT, bounded, graph_error, subject_in_context};
use crate::domain::error::DomainError;
use crate::domain::plan::NodeKey;
use crate::domain::profile::{Category, Profile, ProfileSource};
use crate::domain::subject_settings::SubjectKey;

/// The most nodes one profile read takes: the root and its facts. Graph
/// storage's own ceiling for a traversal is 1000 by default.
pub const MAX_PROFILE_NODES: u32 = 1000;

/// The most facts one profile holds: every fact, the root besides, has to fit
/// in one read.
pub const MAX_FACTS: usize = MAX_PROFILE_NODES as usize - 1;

/// A profile as graph storage holds it: the root's version and removal list,
/// none for a subject without a profile, and every live fact node, also those
/// the root lists for removal whose soft delete has not run yet.
pub(crate) struct Stored {
    pub version: Option<u64>,
    pub root: Root,
    pub facts: Vec<NodeView>,
}

/// Read the root and every node one has-fact edge from it, in one traversal.
///
/// # Errors
///
/// [`DomainError::ProfileFull`] when the profile is larger than one read
/// takes; unavailable when graph storage does not answer in time; any other
/// failure as [`super::graph_error`] maps it.
pub(crate) async fn read_stored(
    client: &dyn GraphStorageClientV1,
    versions: &dyn ProfileVersion,
    timeout: Duration,
    ctx: &SecurityContext,
    subject: &SubjectKey,
) -> Result<Stored, DomainError> {
    // @cpt-begin:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-traverse
    let NodeKey(key) = root_key(subject);
    let request = TraverseRequest {
        seeds: vec![key.clone()],
        depth: 1,
        edge_type_patterns: vec![HAS_FACT_TYPE.to_owned()],
        node_type_patterns: Vec::new(),
        max_nodes: Some(MAX_PROFILE_NODES),
    };
    let response = bounded(timeout, "profile read", client.traverse(ctx, request))
        .await?
        .map_err(|err| graph_error("profile read", &err))?;
    // @cpt-end:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-traverse

    // @cpt-begin:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-root
    let Some(root_node) = response.nodes.iter().find(|node| node.node_key == key) else {
        return Ok(Stored {
            version: None,
            root: Root::default(),
            facts: Vec::new(),
        });
    };
    // A cut profile would let the planner add what is already stored.
    if let Some(reason) = response.truncated {
        return Err(DomainError::ProfileFull(format!(
            "the profile of subject {} holds more than one read takes ({reason:?})",
            subject.subject_id
        )));
    }
    let root = Root::from_payload(&NodeKey(key.clone()), root_node.payload.as_ref())?;
    let version = versions.read(root_node)?;
    // @cpt-end:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-root
    let facts = response
        .nodes
        .into_iter()
        .filter(|node| node.node_key != key && Category::from_type_id(&node.type_id).is_some())
        .collect();
    Ok(Stored {
        version: Some(version),
        root,
        facts,
    })
}

/// [`ProfileSource`] over graph storage: the root and every fact one has-fact
/// edge away from it, in one traversal. A fact listed for removal on the root
/// is left out, also before its soft delete has run.
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
        let subject = subject_in_context(ctx, tenant_id, subject_id)?;
        let stored = read_stored(
            self.client.as_ref(),
            self.version.as_ref(),
            self.timeout,
            ctx,
            &subject,
        )
        .await?;

        // @cpt-begin:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-facts
        let facts = stored
            .facts
            .iter()
            .filter(|node| !stored.root.removals.contains(&node.node_key))
            .filter_map(fact_of)
            .collect();
        Ok(Profile {
            version: stored.version,
            facts,
        })
        // @cpt-end:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-facts
    }
}
