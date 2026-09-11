//! A `GraphServices` a test can drive: the real service, a real
//! `PolicyEnforcer` in front of a stub PDP, the in-memory store, and a
//! one-hop engine over that same store so traversal runs rather than being
//! mocked away.
//!
//! Shared by the service cases and the REST cases, which differ only in
//! where they enter.

#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]

use super::conformance;

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::api::AuthZResolverApi;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::pep::PolicyEnforcer;
use graph_storage::config::GraphStorageConfig;
use graph_storage::domain::service::GraphServices;
use graph_storage::infra::fake_store::FakeGraphStore;
use graph_storage_sdk::models::{Direction, EdgeRef, GraphRevision, NodeId};
use graph_storage_sdk::plugin_api::{
    EngineCursor, ExpandRequest, ExpandResponse, GraphEngineError, GraphEngineV1, GraphStoreV1,
    PathResponse, PatternRequest, PatternResponse, ShortestPathRequest, StoreCtx,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A PDP that admits everything, scoped to the caller's own tenant.
///
/// Allow-all *at the action level* and tenant-bounded at the data level,
/// which is the deployment posture the gear is written for: the PDP decides
/// who may call, the compiled scope decides what they see. A denying variant
/// below covers the other branch.
struct AllowInOwnTenant;

#[async_trait]
impl AuthZResolverApi for AllowInOwnTenant {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let tenant = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(|value| value.as_str())
            .and_then(|raw| Uuid::parse_str(raw).ok())
            .unwrap_or_else(Uuid::nil);
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [tenant],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

/// A PDP that denies. The gear must answer `permission_denied` from the
/// service rather than reaching the store at all.
struct DenyEverything;

#[async_trait]
impl AuthZResolverApi for DenyEverything {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext {
                constraints: Vec::new(),
                deny_reason: None,
            },
        })
    }
}

/// A one-hop engine over the same store the service uses.
///
/// Enough to drive the traversal service for real -- seeds resolve, hops
/// expand, budgets and filters apply -- without a `PostgreSQL` server. It
/// declares neither shortest path nor pattern matching, so the service's
/// unsupported branches are reached as well.
struct HopOverStore {
    store: Arc<FakeGraphStore>,
}

#[async_trait]
impl GraphEngineV1 for HopOverStore {
    fn capabilities(&self) -> graph_storage_sdk::models::EngineCapabilities {
        graph_storage_sdk::models::EngineCapabilities::default()
    }

    async fn cursor(&self, ctx: &StoreCtx<'_>) -> Result<EngineCursor, GraphEngineError> {
        let revision = self.store.revision(ctx).await.unwrap_or(GraphRevision {
            source_epoch: 1,
            revision: 0,
        });
        Ok(EngineCursor { revision })
    }

    async fn expand(
        &self,
        ctx: &StoreCtx<'_>,
        req: ExpandRequest,
    ) -> Result<ExpandResponse, GraphEngineError> {
        // Hydration answers with the node but not its adjacency, so the
        // frontier is turned back into keys and each one is read: this stub
        // stands in for an engine, and an engine is exactly the thing that
        // knows the edges.
        let frontier = self
            .store
            .hydrate_nodes(ctx, &req.frontier)
            .await
            .map_err(|error| GraphEngineError::Unavailable {
                reason: error.to_string(),
            })?;
        let mut views = Vec::new();
        for node in frontier {
            if let Ok(view) = self.store.get_node(ctx, &node.node_key, 1000).await {
                views.push(view);
            }
        }
        let mut edges: Vec<EdgeRef> = Vec::new();
        let mut neighbours: Vec<String> = Vec::new();
        for view in views {
            for entry in view.adjacency {
                let outgoing = entry.side == graph_storage_sdk::models::AdjacencySide::Outgoing;
                let wanted = match req.direction {
                    Direction::Outgoing => outgoing,
                    Direction::Incoming => !outgoing,
                    Direction::Either => true,
                };
                let admitted = req
                    .edge_types
                    .as_ref()
                    .is_none_or(|set| set.contains(&entry.edge_type_id));
                if !wanted || !admitted {
                    continue;
                }
                let (src, dst) = if outgoing {
                    (view.node_key.clone(), entry.neighbor_key.clone())
                } else {
                    (entry.neighbor_key.clone(), view.node_key.clone())
                };
                edges.push(EdgeRef {
                    edge_key: entry.edge_key,
                    edge_type_id: entry.edge_type_id,
                    src,
                    dst,
                });
                neighbours.push(entry.neighbor_key);
            }
        }
        neighbours.sort();
        neighbours.dedup();
        let reached: Vec<NodeId> = self
            .store
            .resolve_node_ids(ctx, &neighbours)
            .await
            .map_err(|error| GraphEngineError::Unavailable {
                reason: error.to_string(),
            })?
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        Ok(ExpandResponse {
            reached,
            edges,
            truncated: None,
            served_by: graph_storage_sdk::plugin_api::HopBackend::TwoQuery,
        })
    }

    async fn shortest_path(
        &self,
        _ctx: &StoreCtx<'_>,
        _req: ShortestPathRequest,
    ) -> Result<PathResponse, GraphEngineError> {
        Err(GraphEngineError::Unsupported {
            what: "shortest_path",
        })
    }

    async fn match_pattern(
        &self,
        _ctx: &StoreCtx<'_>,
        _req: PatternRequest,
    ) -> Result<PatternResponse, GraphEngineError> {
        Err(GraphEngineError::Unsupported {
            what: "match_pattern",
        })
    }
}

pub struct Harness {
    pub services: Arc<GraphServices>,
    pub tenant: Uuid,
}

impl Harness {
    pub fn with(authz: Arc<dyn AuthZResolverApi>) -> Self {
        let store = Arc::new(FakeGraphStore::new());
        let engine = Arc::new(HopOverStore {
            store: Arc::clone(&store),
        });
        Self {
            services: Arc::new(GraphServices::new(
                GraphStorageConfig::default(),
                store,
                engine,
                PolicyEnforcer::new(authz),
                conformance::coordinator(),
            )),
            tenant: Uuid::now_v7(),
        }
    }

    pub fn allowed() -> Self {
        Self::with(Arc::new(AllowInOwnTenant))
    }

    pub fn denied() -> Self {
        Self::with(Arc::new(DenyEverything))
    }

    pub fn ctx(&self) -> SecurityContext {
        SecurityContext::builder()
            .subject_id(Uuid::now_v7())
            .subject_tenant_id(self.tenant)
            .build()
            .expect("a valid security context")
    }

    /// The base ontology plus the suite's producer types, through the service.
    pub async fn seed_ontology(&self, ctx: &SecurityContext) {
        self.services
            .register_types(ctx, conformance::ontology_batch())
            .await
            .expect("the ontology registers");
    }
}
