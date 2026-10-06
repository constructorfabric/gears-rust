use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use super::DbProvider;
use super::error::DomainError;

/// Authorization resource type for subject settings, scoped by tenant and
/// subject id.
pub(crate) const SUBJECT_SETTINGS_RESOURCE: ResourceType = ResourceType::from_static(
    "construct.subject_settings",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) mod actions {
    pub const GET: &str = "get";
}

/// The two states of one subject that intake, admission and the reader read.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubjectSettings {
    pub personalization_enabled: bool,
    pub erasure_in_progress: bool,
}

impl SubjectSettings {
    /// The settings of a subject Construct holds no row for: the tenant's
    /// personalization default, and no erasure under way.
    #[must_use]
    pub fn new_subject(personalization_default: bool) -> Self {
        Self {
            personalization_enabled: personalization_default,
            erasure_in_progress: false,
        }
    }
}

#[async_trait]
pub trait SubjectSettingsRepository: Send + Sync {
    /// The stored settings of one subject, if they are inside the scope.
    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        subject_id: Uuid,
    ) -> Result<Option<SubjectSettings>, DomainError>;
}

/// The tenant settings a new subject starts from. The tenant settings live in
/// the settings service; Construct keeps none of its own.
#[async_trait]
pub trait TenantDefaults: Send + Sync {
    /// Whether personalization is on for a new subject of the tenant.
    async fn personalization_default(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<bool, DomainError>;
}

#[domain_model]
pub struct SubjectSettingsService<R: SubjectSettingsRepository> {
    db: Arc<DbProvider>,
    repo: Arc<R>,
    defaults: Arc<dyn TenantDefaults>,
    policy_enforcer: PolicyEnforcer,
}

/// @cpt-dod:cpt-cf-construct-dod-subject-settings-read:p1
impl<R: SubjectSettingsRepository> SubjectSettingsService<R> {
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<R>,
        defaults: Arc<dyn TenantDefaults>,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            db,
            repo,
            defaults,
            policy_enforcer,
        }
    }

    /// The settings of one subject in the caller's tenant. A subject without a
    /// row gets the tenant's default; reading does not store it.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), subject_id = %subject_id))]
    pub async fn settings(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
    ) -> Result<SubjectSettings, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-scope
        let tenant_id = ctx.subject_tenant_id();
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &SUBJECT_SETTINGS_RESOURCE,
                actions::GET,
                Some(subject_id),
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;
        // @cpt-end:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-scope

        // @cpt-begin:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-select
        let conn = self.db.conn().map_err(DomainError::from)?;
        let stored = self.repo.find(&conn, &scope, subject_id).await?;
        // @cpt-end:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-select

        // @cpt-begin:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-stored
        if let Some(stored) = stored {
            return Ok(stored);
        }
        // @cpt-end:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-stored

        // @cpt-begin:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-default
        let personalization_default = self
            .defaults
            .personalization_default(ctx, tenant_id)
            .await?;
        Ok(SubjectSettings::new_subject(personalization_default))
        // @cpt-end:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-default
    }
}
