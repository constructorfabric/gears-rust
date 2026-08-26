//! In-process adapter over the domain services, registered in `ClientHub`.
//!
//! Same admission, same authorization, same error taxonomy as REST: every
//! call goes through the identical `GraphServices` methods, and failures
//! render through the identical `DomainError -> CanonicalError` mapping.

use std::sync::Arc;

use async_trait::async_trait;
use graph_storage_sdk::GraphStorageClientV1;
use graph_storage_sdk::models::{
    DeleteOutcome, EdgeKey, GraphRevision, GtsTypeId, IngestOutcome, IngestRequest,
    NeighborhoodRequest, NodeKey, NodeRow, NodeView, Page, SearchRequest, SearchResponse,
    TraversalResponse, TraverseRequest, TypeQuery, TypeRecord, TypeRegistration,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

use crate::domain::service::GraphServices;

#[domain_model]
pub struct GraphStorageLocalClient {
    services: Arc<GraphServices>,
}

impl GraphStorageLocalClient {
    #[must_use]
    pub fn new(services: Arc<GraphServices>) -> Self {
        Self { services }
    }
}

#[async_trait]
impl GraphStorageClientV1 for GraphStorageLocalClient {
    async fn register_types(
        &self,
        ctx: &SecurityContext,
        batch: Vec<TypeRegistration>,
    ) -> Result<Vec<TypeRecord>, CanonicalError> {
        self.services
            .register_types(ctx, batch)
            .await
            .map_err(Into::into)
    }

    async fn get_type(
        &self,
        ctx: &SecurityContext,
        type_id: &GtsTypeId,
    ) -> Result<TypeRecord, CanonicalError> {
        self.services
            .get_type(ctx, type_id)
            .await
            .map_err(Into::into)
    }

    async fn list_types(
        &self,
        ctx: &SecurityContext,
        query: TypeQuery,
    ) -> Result<Page<TypeRecord>, CanonicalError> {
        self.services
            .list_types(ctx, query)
            .await
            .map_err(Into::into)
    }

    async fn ingest(
        &self,
        ctx: &SecurityContext,
        request: IngestRequest,
    ) -> Result<IngestOutcome, CanonicalError> {
        self.services.ingest(ctx, request).await.map_err(Into::into)
    }

    async fn delete_node(
        &self,
        ctx: &SecurityContext,
        node_key: &NodeKey,
    ) -> Result<DeleteOutcome, CanonicalError> {
        self.services
            .delete_node(ctx, node_key)
            .await
            .map_err(Into::into)
    }

    async fn delete_edge(
        &self,
        ctx: &SecurityContext,
        edge_key: &EdgeKey,
    ) -> Result<DeleteOutcome, CanonicalError> {
        self.services
            .delete_edge(ctx, edge_key)
            .await
            .map_err(Into::into)
    }

    async fn get_node(
        &self,
        ctx: &SecurityContext,
        node_key: &NodeKey,
        adjacency_limit: Option<u32>,
    ) -> Result<NodeView, CanonicalError> {
        self.services
            .get_node(ctx, node_key, adjacency_limit)
            .await
            .map_err(Into::into)
    }

    async fn project_nodes(
        &self,
        ctx: &SecurityContext,
        type_patterns: &[String],
        query: toolkit_odata::ODataQuery,
    ) -> Result<toolkit_odata::Page<NodeRow>, CanonicalError> {
        self.services
            .project_nodes(ctx, type_patterns, query)
            .await
            .map_err(Into::into)
    }

    async fn search(
        &self,
        ctx: &SecurityContext,
        request: SearchRequest,
    ) -> Result<SearchResponse, CanonicalError> {
        self.services.search(ctx, request).await.map_err(Into::into)
    }

    async fn traverse(
        &self,
        ctx: &SecurityContext,
        request: TraverseRequest,
    ) -> Result<TraversalResponse, CanonicalError> {
        self.services
            .traverse(ctx, request)
            .await
            .map_err(Into::into)
    }

    async fn neighborhood(
        &self,
        ctx: &SecurityContext,
        request: NeighborhoodRequest,
    ) -> Result<TraversalResponse, CanonicalError> {
        self.services
            .neighborhood(ctx, request)
            .await
            .map_err(Into::into)
    }

    async fn revision(&self, ctx: &SecurityContext) -> Result<GraphRevision, CanonicalError> {
        self.services.revision(ctx).await.map_err(Into::into)
    }
}
