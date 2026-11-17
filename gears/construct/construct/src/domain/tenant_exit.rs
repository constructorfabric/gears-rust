//! Tenant exit: removing everything Construct holds for a tenant the
//! platform has deleted. Construct's own rows go from its tables, and graph
//! storage is asked to offboard the tenant, which removes its graph, the
//! profiles included. The leader's deletion job checks each tenant it knows
//! on every retention run: no platform event announces a deleted tenant, so
//! the job asks the tenant resolver. A run that stops part way is finished
//! by a later one, as every step can run again.

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::deletion::{DeletionEvent, DeletionEvents};
use super::error::DomainError;
use super::planner::processing::ProcessingContexts;

/// What the platform says about a tenant.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TenantStatus {
    /// The platform has marked the tenant deleted: Construct removes its data.
    Deleted,
    /// The tenant exists and is not deleted: active or suspended.
    Present,
    /// The resolver does not know the tenant, or does not show it to
    /// Construct. Nothing says it was deleted, so nothing is removed.
    Unknown,
}

/// The platform's tenant resolver, as tenant exit asks it.
#[async_trait]
pub trait TenantStatuses: Send + Sync {
    /// The status of `tenant_id`, asked as `ctx`.
    ///
    /// # Errors
    ///
    /// Not configured without a resolver; unavailable when it does not
    /// answer.
    async fn status(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<TenantStatus, DomainError>;
}

/// How far graph storage's offboarding of a tenant has gone.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphExit {
    /// The tenant is fenced and its graph is being removed.
    UnderWay,
    /// Every row of the tenant is gone from graph storage.
    Done,
}

/// Graph storage's tenant offboarding.
#[async_trait]
pub trait GraphOffboarding: Send + Sync {
    /// Start the offboarding of `tenant_id` when it has not started, and say
    /// how far it has gone. Asking again is safe: graph storage answers the
    /// offboarding it already has.
    ///
    /// # Errors
    ///
    /// Graph storage's: unavailable when a retry may cure it, internal when
    /// it refuses, as when Construct is not granted the offboarding.
    async fn offboard(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<GraphExit, DomainError>;
}

/// How many of a deleted tenant's rows tenant exit removed from Construct's
/// tables.
#[domain_model]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TenantRows {
    pub subject_settings: u64,
    pub review_requests: u64,
    pub record_ids: u64,
}

/// Construct's own tables, as tenant exit empties them.
#[async_trait]
pub trait TenantTables: Send + Sync {
    /// Remove every row of `tenant_id`'s from Construct's tables: subject
    /// settings, review requests and record identities. Removing again is
    /// safe.
    ///
    /// # Errors
    ///
    /// When the store cannot be reached.
    async fn remove_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<TenantRows, DomainError>;

    /// Stop keeping `tenant_id` among the tenants Construct holds data for,
    /// so the job no longer checks it. A write that lands later notes it
    /// again, and the next run removes that too.
    ///
    /// # Errors
    ///
    /// When the store cannot be reached.
    async fn forget_tenant(&self, tenant_id: Uuid) -> Result<(), DomainError>;
}

/// How one run of a deleted tenant's exit ended.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Construct's rows are gone; graph storage is still removing the graph.
    /// The next run asks again.
    UnderWay,
    /// Everything is gone, and the tenant is no longer kept.
    Done,
}

/// The collaborators of tenant exit.
#[domain_model]
pub struct TenantExitPorts {
    pub contexts: Arc<dyn ProcessingContexts>,
    pub statuses: Arc<dyn TenantStatuses>,
    pub graph: Arc<dyn GraphOffboarding>,
    pub tables: Arc<dyn TenantTables>,
    pub events: Arc<dyn DeletionEvents>,
}

/// Tenant exit, run as Construct in the tenant that leaves.
///
/// @cpt-dod:cpt-cf-construct-dod-retention-tenant-exit:p2
#[domain_model]
pub struct TenantExit {
    ports: TenantExitPorts,
}

impl TenantExit {
    #[must_use]
    pub fn new(ports: TenantExitPorts) -> Self {
        Self { ports }
    }

    /// Whether the platform has deleted `tenant_id`.
    ///
    /// # Errors
    ///
    /// Not configured without a processing identity or a tenant resolver;
    /// the resolver's errors otherwise.
    pub async fn deleted(&self, tenant_id: Uuid) -> Result<bool, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-status
        let (construct, _) = self.ports.contexts.for_tenant(tenant_id).await?;
        let status = self.ports.statuses.status(&construct, tenant_id).await?;
        Ok(status == TenantStatus::Deleted)
        // @cpt-end:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-status
    }

    /// Remove what Construct holds for `tenant_id`, which the platform has
    /// deleted: its rows in Construct's tables, then its graph through graph
    /// storage's offboarding. Once graph storage reports the graph gone, the
    /// tenant is no longer kept.
    ///
    /// NOT cancel-safe: the next run does what a dropped one left.
    ///
    /// # Errors
    ///
    /// Not configured without a processing identity; the store's and graph
    /// storage's errors otherwise. Nothing that failed is left half done for
    /// good, as every step can run again.
    #[tracing::instrument(skip_all, fields(%tenant_id))]
    pub async fn exit(&self, tenant_id: Uuid) -> Result<Exit, DomainError> {
        let (construct, scope) = self.ports.contexts.for_tenant(tenant_id).await?;
        // @cpt-begin:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-rows
        let rows = self.ports.tables.remove_tenant(&scope, tenant_id).await?;
        if rows != TenantRows::default() {
            self.ports.events.record(DeletionEvent::TenantRowsRemoved {
                tenant_id,
                subject_settings: rows.subject_settings,
                review_requests: rows.review_requests,
                record_ids: rows.record_ids,
            });
        }
        // @cpt-end:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-rows
        // @cpt-begin:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-graph
        if self.ports.graph.offboard(&construct, tenant_id).await? == GraphExit::UnderWay {
            return Ok(Exit::UnderWay);
        }
        // @cpt-end:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-graph
        // @cpt-begin:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-forget
        self.ports.tables.forget_tenant(tenant_id).await?;
        self.ports
            .events
            .record(DeletionEvent::TenantExited { tenant_id });
        Ok(Exit::Done)
        // @cpt-end:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-forget
    }
}
