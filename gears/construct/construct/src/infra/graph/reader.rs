use std::sync::Arc;

use async_trait::async_trait;
use construct_sdk::gts::HAS_FACT_TYPE;
use graph_storage_sdk::GraphStorageClientV1;
use graph_storage_sdk::models::TraverseRequest;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::fact::fact_of;
use super::graph_error;
use super::root::{ProfileVersion, Root, root_key};
use crate::domain::error::DomainError;
use crate::domain::profile::{Category, Profile, ProfileSource};

/// The most nodes one profile read takes: the root and its facts. Graph
/// storage's own ceiling for a traversal is 1000 by default.
pub const MAX_PROFILE_NODES: u32 = 1000;

/// [`ProfileSource`] over graph storage: the root and every fact one has-fact
/// edge away from it, in one traversal. A fact listed for removal on the root
/// is left out, also before its soft delete has run.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-writer-read:p1
pub struct GraphProfileSource {
    client: Arc<dyn GraphStorageClientV1>,
    version: Arc<dyn ProfileVersion>,
}

impl GraphProfileSource {
    #[must_use]
    pub fn new(client: Arc<dyn GraphStorageClientV1>, version: Arc<dyn ProfileVersion>) -> Self {
        Self { client, version }
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
        // @cpt-begin:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-traverse
        let root_key = root_key(tenant_id, subject_id);
        let response = self
            .client
            .traverse(
                ctx,
                TraverseRequest {
                    seeds: vec![root_key.clone()],
                    depth: 1,
                    edge_type_patterns: vec![HAS_FACT_TYPE.to_owned()],
                    node_type_patterns: Vec::new(),
                    max_nodes: Some(MAX_PROFILE_NODES),
                },
            )
            .await
            .map_err(|err| graph_error("profile read", &err))?;
        // @cpt-end:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-traverse

        // @cpt-begin:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-root
        let Some(root_node) = response.nodes.iter().find(|node| node.node_key == root_key) else {
            return Ok(Profile {
                version: None,
                facts: Vec::new(),
            });
        };
        // A cut profile would let the planner add what is already stored.
        if let Some(reason) = response.truncated {
            return Err(DomainError::internal(format!(
                "the profile is larger than one read takes ({reason:?})"
            )));
        }
        let root = Root::from_payload(root_node.payload.as_ref())?;
        let version = self.version.read(root_node.payload.as_ref())?;
        // @cpt-end:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-root

        // @cpt-begin:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-facts
        let facts = response
            .nodes
            .iter()
            .filter(|node| node.node_key != root_key && !root.removals.contains(&node.node_key))
            .filter(|node| Category::from_type_id(&node.type_id).is_some())
            .filter_map(fact_of)
            .collect();
        Ok(Profile {
            version: Some(version),
            facts,
        })
        // @cpt-end:cpt-cf-construct-algo-profile-writer-read:p1:inst-read-facts
    }
}
