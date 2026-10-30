//! [`RegistryProvisionerImpl`]: the registry reconcile's side of the Control
//! Plane. It writes through the Control Plane's own validation, as
//! `managed_by = registry`, and alone holds the registry rows across tenants.

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::ControlPlaneServiceImpl;
use super::validation::validate_upstream_request;
use crate::domain::error::DomainError;
use crate::domain::model::{
    CreateRouteRequest, CreateUpstreamRequest, ListQuery, ManagedBy, Route, UpdateRouteRequest,
    UpdateUpstreamRequest, Upstream,
};
use crate::domain::repo::{DeleteOutcome, RegistryRowRepository};
use crate::domain::services::{ControlPlaneService, RegistryProvisioner, UpstreamCheck};

/// The registry write path over a Control Plane, with the registry rows
/// across tenants. Only the gear holds it.
#[domain_model]
pub(crate) struct RegistryProvisionerImpl {
    cp: Arc<ControlPlaneServiceImpl>,
    rows: Arc<dyn RegistryRowRepository>,
}

impl RegistryProvisionerImpl {
    /// The registry write path over `cp`, reading registry rows from `rows`
    /// (the same store as `cp`'s repositories).
    #[must_use]
    pub(crate) fn new(
        cp: Arc<ControlPlaneServiceImpl>,
        rows: Arc<dyn RegistryRowRepository>,
    ) -> Self {
        Self { cp, rows }
    }
}

#[async_trait]
impl RegistryProvisioner for RegistryProvisionerImpl {
    async fn get_upstream(&self, ctx: &SecurityContext, id: Uuid) -> Result<Upstream, DomainError> {
        self.cp.get_upstream(ctx, id).await
    }

    async fn get_route(&self, ctx: &SecurityContext, id: Uuid) -> Result<Route, DomainError> {
        self.cp.get_route(ctx, id).await
    }

    async fn list_routes(
        &self,
        ctx: &SecurityContext,
        upstream_id: Option<Uuid>,
        query: &ListQuery,
    ) -> Result<Vec<Route>, DomainError> {
        self.cp.list_routes(ctx, upstream_id, query).await
    }

    async fn create_registry_upstream(
        &self,
        ctx: &SecurityContext,
        req: CreateUpstreamRequest,
    ) -> Result<Upstream, DomainError> {
        self.cp
            .create_upstream_as(ctx, req, ManagedBy::Registry)
            .await
    }

    async fn update_registry_upstream(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateUpstreamRequest,
    ) -> Result<Upstream, DomainError> {
        self.cp
            .update_upstream_as(ctx, id, req, ManagedBy::Registry)
            .await
    }

    fn check_registry_upstream(&self, req: &CreateUpstreamRequest) -> Result<String, DomainError> {
        validate_upstream_request(&self.cp.ssrf_guard, req)
    }

    async fn create_registry_route(
        &self,
        ctx: &SecurityContext,
        req: CreateRouteRequest,
        upstream: UpstreamCheck,
    ) -> Result<Route, DomainError> {
        self.cp
            .create_route_as(ctx, req, ManagedBy::Registry, upstream)
            .await
    }

    async fn update_registry_route(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        upstream_id: Uuid,
        req: UpdateRouteRequest,
        upstream: UpstreamCheck,
    ) -> Result<Route, DomainError> {
        self.cp
            .update_route_as(
                ctx,
                id,
                req,
                ManagedBy::Registry,
                Some((upstream_id, upstream)),
            )
            .await
    }

    async fn delete_registry_route(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), DomainError> {
        self.cp.delete_route_as(ctx, id, ManagedBy::Registry).await
    }

    async fn delete_registry_upstream_if_unused(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<DeleteOutcome, DomainError> {
        self.rows
            .delete_upstream_if_unused(ctx.subject_tenant_id(), id)
            .await
            .map_err(DomainError::from)
    }

    async fn registry_upstreams(&self) -> Result<Vec<Upstream>, DomainError> {
        self.rows.list_upstreams().await.map_err(DomainError::from)
    }

    async fn registry_routes(&self) -> Result<Vec<Route>, DomainError> {
        self.rows.list_routes().await.map_err(DomainError::from)
    }
}
