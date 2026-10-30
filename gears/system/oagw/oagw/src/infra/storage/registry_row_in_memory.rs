//! `InMemoryRegistryRowRepo`: `RegistryRowRepository` over the in-memory
//! upstream and route repositories.

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use super::{InMemoryRouteRepo, InMemoryUpstreamRepo};
use crate::domain::model::{ListQuery, ManagedBy, Route, Upstream};
use crate::domain::repo::{
    DeleteOutcome, RegistryRowRepository, RepositoryError, RouteRepository, UpstreamRepository,
};

/// Registry rows of the in-memory repositories.
pub(crate) struct InMemoryRegistryRowRepo {
    upstreams: Arc<InMemoryUpstreamRepo>,
    routes: Arc<InMemoryRouteRepo>,
}

impl InMemoryRegistryRowRepo {
    /// Registry rows over the repositories the Control Plane writes.
    pub(crate) fn new(
        upstreams: Arc<InMemoryUpstreamRepo>,
        routes: Arc<InMemoryRouteRepo>,
    ) -> Self {
        Self { upstreams, routes }
    }
}

#[async_trait]
impl RegistryRowRepository for InMemoryRegistryRowRepo {
    async fn list_upstreams(&self) -> Result<Vec<Upstream>, RepositoryError> {
        Ok(self.upstreams.registry_rows())
    }

    async fn list_routes(&self) -> Result<Vec<Route>, RepositoryError> {
        Ok(self.routes.registry_rows())
    }

    async fn delete_upstream_if_unused(
        &self,
        tenant_id: Uuid,
        id: Uuid,
    ) -> Result<DeleteOutcome, RepositoryError> {
        match self.upstreams.get_by_id(tenant_id, id).await {
            Ok(upstream) if upstream.managed_by == ManagedBy::Registry => {}
            Ok(_) | Err(RepositoryError::NotFound { .. }) => return Ok(DeleteOutcome::NotFound),
            Err(e) => return Err(e),
        }
        // The route store is separate, so the check comes first; one process
        // and no other replica, so only a route create already past its
        // upstream check can slip in (see the trait).
        let first = ListQuery { top: 1, skip: 0 };
        if !self
            .routes
            .list(tenant_id, Some(id), &first)
            .await?
            .is_empty()
        {
            return Ok(DeleteOutcome::InUse);
        }
        match self.upstreams.delete(tenant_id, id).await {
            Ok(()) => Ok(DeleteOutcome::Deleted),
            Err(RepositoryError::NotFound { .. }) => Ok(DeleteOutcome::NotFound),
            Err(e) => Err(e),
        }
    }
}
