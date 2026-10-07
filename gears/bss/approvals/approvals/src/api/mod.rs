//! HTTP surface of the inbox.

pub mod rest;

use std::sync::Arc;

use toolkit::ClientHub;

/// The actors that are not people (AP-D-11): the nil id of the platform's system context. A read
/// names it "System" and never asks Account Management, as it does the actors each configured
/// source declares (`ApprovalSourceV1::system_actors`). The owning gears refuse their own system
/// actors at every door, so none submits or votes on a unit the inbox lists.
pub const SYSTEM_ACTORS: [uuid::Uuid; 1] = [uuid::Uuid::nil()];

/// What a request needs: the configured source names, the hub they are registered in, and the
/// names of the actors a read shows.
pub struct ApiState {
    /// Config order.
    pub sources: Vec<String>,
    /// Scoped `ApprovalSourceV1` clients, one scope per source name.
    pub hub: Arc<ClientHub>,
    /// The submitters' and voters' names (AP-D-11), through Account Management when the hub
    /// holds it.
    pub actor_names: bss_rest::actor_names::ActorNames,
}

impl ApiState {
    /// The state over `sources` in `hub`, naming actors through the hub's Account Management.
    #[must_use]
    pub fn new(sources: Vec<String>, hub: Arc<ClientHub>) -> Self {
        let actor_names =
            bss_rest::actor_names::ActorNames::from_hub(Arc::clone(&hub), &SYSTEM_ACTORS);
        Self {
            sources,
            hub,
            actor_names,
        }
    }

    /// The same state naming actors with `actor_names`; tests install a fake directory.
    #[must_use]
    pub fn with_actor_names(self, actor_names: bss_rest::actor_names::ActorNames) -> Self {
        Self {
            actor_names,
            ..self
        }
    }
}
