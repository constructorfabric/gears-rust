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
    /// so the job no longer checks it; whether it was kept. A record that
    /// lands later leaves a row the job finds the tenant by again; a profile
    /// write leaves none, as graph storage refuses every write of an
    /// offboarded tenant.
    ///
    /// # Errors
    ///
    /// When the store cannot be reached.
    async fn forget_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<bool, DomainError>;
}

/// Why tenant exit left a tenant alone.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    /// The tenant exists and is not deleted.
    Present,
    /// The resolver does not know the tenant: nothing says it was deleted.
    Unknown,
}

/// How one run of tenant exit for a tenant ended.
#[domain_model]
#[derive(Debug)]
pub enum Exit {
    /// The tenant is not deleted, or the resolver does not know it. Nothing
    /// is removed.
    Kept(Kept),
    /// Graph storage is removing the graph and Construct's rows are gone.
    /// The next run asks again.
    UnderWay,
    /// Everything is gone, and the tenant is no longer kept.
    Done,
    /// The tenant had exited before: nothing was left to remove. A tenant
    /// the deployment's config names stays checked after its exit.
    Gone,
    /// The tenant is deleted, but this run stopped: graph storage or the
    /// store failed. Nothing that failed is left half done for good, as every
    /// step can run again.
    Stopped(DomainError),
}

impl Exit {
    /// Whether the platform has deleted the tenant, so retention leaves it
    /// alone. Every outcome is named, so a new one has to say.
    #[must_use]
    pub fn tenant_deleted(&self) -> bool {
        match self {
            Self::Kept(_) => false,
            Self::UnderWay | Self::Done | Self::Gone | Self::Stopped(_) => true,
        }
    }
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

    /// Ask the platform about `tenant_id` and, when it has deleted the
    /// tenant, remove what Construct holds for it: graph storage is asked to
    /// offboard the tenant first, and only once it has, so the graph is
    /// fenced, are Construct's rows removed; once graph storage reports the
    /// graph gone, the tenant is no longer kept. A tenant that is not deleted
    /// is left untouched.
    ///
    /// NOT cancel-safe: the next run does what a dropped one left.
    ///
    /// # Errors
    ///
    /// The tenant's status could not be read: not configured without a
    /// processing identity or a tenant resolver, the resolver's errors
    /// otherwise. A failure after the tenant is known to be deleted is
    /// [`Exit::Stopped`].
    #[tracing::instrument(skip_all, fields(%tenant_id))]
    pub async fn run(&self, tenant_id: Uuid) -> Result<Exit, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-status
        let (construct, scope) = self.ports.contexts.for_tenant(tenant_id).await?;
        match self.ports.statuses.status(&construct, tenant_id).await? {
            TenantStatus::Deleted => {}
            TenantStatus::Present => return Ok(Exit::Kept(Kept::Present)),
            TenantStatus::Unknown => return Ok(Exit::Kept(Kept::Unknown)),
        }
        // @cpt-end:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-status
        Ok(self
            .exit(&construct, &scope, tenant_id)
            .await
            .unwrap_or_else(Exit::Stopped))
    }

    /// The exit of a tenant the platform has deleted.
    async fn exit(
        &self,
        construct: &SecurityContext,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Exit, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-graph
        // Graph storage first: until it has fenced the tenant its graph stays
        // readable and writable, so the subjects' settings, their opt-outs
        // and erasure marks, must stay too. A refusal leaves everything as it
        // was.
        let graph = self.ports.graph.offboard(construct, tenant_id).await?;
        // @cpt-end:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-graph
        // @cpt-begin:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-rows
        let rows = self.ports.tables.remove_tenant(scope, tenant_id).await?;
        if rows != TenantRows::default() {
            self.ports.events.record(DeletionEvent::TenantRowsRemoved {
                tenant_id,
                subject_settings: rows.subject_settings,
                review_requests: rows.review_requests,
                record_ids: rows.record_ids,
            });
        }
        // @cpt-end:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-rows
        if graph == GraphExit::UnderWay {
            return Ok(Exit::UnderWay);
        }
        // @cpt-begin:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-forget
        // The exit is recorded once, by the run that stops keeping the
        // tenant; a tenant the config names is checked again after it, and
        // finds nothing left.
        if self.ports.tables.forget_tenant(scope, tenant_id).await? {
            self.ports
                .events
                .record(DeletionEvent::TenantExited { tenant_id });
            return Ok(Exit::Done);
        }
        Ok(if rows == TenantRows::default() {
            Exit::Gone
        } else {
            Exit::Done
        })
        // @cpt-end:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-forget
    }
}
