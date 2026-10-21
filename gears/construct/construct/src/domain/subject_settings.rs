use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use toolkit_db::DBProvider;
use toolkit_db::secure::DBRunner;
use toolkit_gts::gts_id;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use super::DbProvider;
use super::error::DomainError;
pub use super::settings_scope::{ScopeRefusal, SettingsOutOfScope, UncheckablePredicate};

/// GTS type id of a subject's settings: the authorization resource type, and
/// the resource type the settings routes' errors name.
pub(crate) const SUBJECT_SETTINGS_TYPE: &str = gts_id!("cf.construct.subject.settings.v1~");

/// Authorization resource type for subject settings, scoped by tenant, subject
/// id and owner. The subject owns its settings, so the owner is the subject id
/// too, and a policy can say "owner only" itself.
pub(crate) const SUBJECT_SETTINGS_RESOURCE: ResourceType = ResourceType::from_static(
    SUBJECT_SETTINGS_TYPE,
    &[
        pep_properties::OWNER_TENANT_ID,
        pep_properties::RESOURCE_ID,
        pep_properties::OWNER_ID,
    ],
);

/// What the settings routes and the read tell the policy service about one
/// subject's settings: the tenant that owns them, and the subject, who is
/// their owner.
fn settings_request(key: SubjectKey) -> AccessRequest {
    AccessRequest::new()
        .resource_property(pep_properties::OWNER_TENANT_ID, key.tenant_id)
        .resource_property(pep_properties::OWNER_ID, key.subject_id)
}

pub(crate) mod actions {
    pub const GET: &str = "get";
    pub const UPDATE: &str = "update";
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

/// One subject in one tenant: the key of a settings row. The same subject id
/// can have a row in more than one tenant, so a row is never looked up by the
/// subject alone. Named fields keep the two ids from being swapped.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubjectKey {
    pub tenant_id: Uuid,
    pub subject_id: Uuid,
}

/// What a lookup of one subject's settings under a policy scope found. A row
/// the scope leaves out is never reported as absent, so it can never turn into
/// the tenant's default.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsLookup {
    /// The row exists and the scope covers it.
    Stored(SubjectSettings),
    /// The scope covers the key and there is no row: a new subject.
    Absent,
    /// The scope does not let the read see the key's row.
    OutOfScope(ScopeRefusal),
}

/// What a write of one subject's settings under a policy scope did. A key the
/// scope leaves out is refused before anything is written, with the same
/// decision as the read.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsWrite {
    /// The scope covers the key and the row is stored.
    Written,
    /// The scope does not let the write reach the key's row; nothing is stored.
    OutOfScope(ScopeRefusal),
}

#[async_trait]
pub trait SubjectSettingsRepository: Send + Sync {
    /// The settings of the subject in its tenant under `scope`. Whether the
    /// scope covers the key is decided from the key, before the row is read,
    /// so the outcome tells a new subject from one the scope leaves out.
    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: SubjectKey,
    ) -> Result<SettingsLookup, DomainError>;

    /// Store the subject's personalization setting, creating the row when
    /// there is none. A new row starts with no erasure under way; an existing
    /// row keeps its erasure flag. Whether the scope covers the key is decided
    /// as in `find`, before anything is written.
    async fn set_personalization<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: SubjectKey,
        personalization_enabled: bool,
    ) -> Result<SettingsWrite, DomainError>;
}

/// Owner only: a subject reads and changes only its own settings. Another
/// subject, in this tenant or another, is refused.
fn ensure_owner(ctx: &SecurityContext, subject_id: Uuid) -> Result<(), DomainError> {
    // @cpt-begin:cpt-cf-construct-algo-subject-settings-owner-only:p1:inst-owner-compare
    if ctx.subject_id() == subject_id {
        return Ok(());
    }
    // @cpt-end:cpt-cf-construct-algo-subject-settings-owner-only:p1:inst-owner-compare
    // @cpt-begin:cpt-cf-construct-algo-subject-settings-owner-only:p1:inst-owner-refuse
    // The ids go into the message, which the REST mapping logs and keeps out
    // of the answer, so the log says who was refused which settings.
    Err(DomainError::forbidden(format!(
        "caller {} of tenant {} may not read or change the settings of another subject, {subject_id}",
        ctx.subject_id(),
        ctx.subject_tenant_id()
    )))
    // @cpt-end:cpt-cf-construct-algo-subject-settings-owner-only:p1:inst-owner-refuse
}

/// The refusal of a read or a write whose scope leaves the key out.
fn out_of_scope(ctx: &SecurityContext, key: SubjectKey, refusal: ScopeRefusal) -> DomainError {
    DomainError::OutOfScope(SettingsOutOfScope {
        caller: ctx.subject_id(),
        caller_tenant: ctx.subject_tenant_id(),
        tenant_id: key.tenant_id,
        subject_id: key.subject_id,
        refusal,
    })
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

/// The read of a subject's settings for a caller that already holds a scope:
/// record intake under its "send records" decision, Admission under the scope
/// processing builds for the tenant.
#[async_trait]
pub trait SubjectSettingsReader: Send + Sync {
    async fn read(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        key: SubjectKey,
    ) -> Result<SubjectSettings, DomainError>;
}

#[domain_model]
pub struct SubjectSettingsService<R: SubjectSettingsRepository> {
    db: Arc<DbProvider>,
    repo: Arc<R>,
    defaults: Arc<dyn TenantDefaults>,
    policy_enforcer: PolicyEnforcer,
}

/// @cpt-dod:cpt-cf-construct-dod-subject-settings-read:p1
impl<R: SubjectSettingsRepository + 'static> SubjectSettingsService<R> {
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
        let key = SubjectKey {
            tenant_id,
            subject_id,
        };
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &SUBJECT_SETTINGS_RESOURCE,
                actions::GET,
                Some(subject_id),
                &settings_request(key),
            )
            .await?;
        // @cpt-end:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-scope

        self.settings_within(ctx, &scope, key).await
    }

    /// The settings of one subject in its tenant, read under a scope the
    /// caller already holds from its own permission check. Record intake uses
    /// it: its "send records" decision covers the read, so a connector needs no
    /// second permission. A scope that does not cover the key is refused, with
    /// or without a row, so a row it leaves out never turns into the tenant's
    /// default.
    pub(crate) async fn settings_within(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        key: SubjectKey,
    ) -> Result<SubjectSettings, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-select
        let conn = self.db.conn().map_err(DomainError::from)?;
        let lookup = self.repo.find(&conn, scope, key).await?;
        // @cpt-end:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-select

        match lookup {
            // @cpt-begin:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-stored
            SettingsLookup::Stored(stored) => return Ok(stored),
            // @cpt-end:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-stored
            SettingsLookup::Absent => {}
            // @cpt-begin:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-cover
            SettingsLookup::OutOfScope(refusal) => {
                return Err(out_of_scope(ctx, key, refusal));
            } // @cpt-end:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-cover
        }

        // @cpt-begin:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-default
        let personalization_default = self
            .defaults
            .personalization_default(ctx, key.tenant_id)
            .await?;
        Ok(SubjectSettings::new_subject(personalization_default))
        // @cpt-end:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-default
    }

    /// The subject's own settings, for the subject's settings route. Owner
    /// only; otherwise the same read as [`Self::settings`].
    ///
    /// @cpt-dod:cpt-cf-construct-dod-subject-settings-routes:p1
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), subject_id = %subject_id))]
    pub async fn owner_settings(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
    ) -> Result<SubjectSettings, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-subject-settings-view:p1:inst-view-owner
        ensure_owner(ctx, subject_id)?;
        // @cpt-end:cpt-cf-construct-flow-subject-settings-view:p1:inst-view-owner
        // @cpt-begin:cpt-cf-construct-flow-subject-settings-view:p1:inst-view-read
        self.settings(ctx, subject_id).await
        // @cpt-end:cpt-cf-construct-flow-subject-settings-view:p1:inst-view-read
    }

    /// Turn personalization on or off for the subject, which is the caller.
    /// The erasure flag is left as it is, so turning personalization on during
    /// an erasure stores the choice but does not end the erasure. Returns the
    /// settings as stored.
    ///
    /// cancel-safe: the write and its read-back run in one transaction. A drop
    /// before the commit stores nothing; a drop after it leaves the one
    /// committed write, which a repeat of the same request repeats without
    /// harm, and no answer.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), subject_id = %subject_id))]
    pub async fn set_personalization(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
        personalization_enabled: bool,
    ) -> Result<SubjectSettings, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-owner
        ensure_owner(ctx, subject_id)?;
        // @cpt-end:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-owner

        // @cpt-begin:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-scope
        let tenant_id = ctx.subject_tenant_id();
        let key = SubjectKey {
            tenant_id,
            subject_id,
        };
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &SUBJECT_SETTINGS_RESOURCE,
                actions::UPDATE,
                Some(subject_id),
                &settings_request(key),
            )
            .await?;
        // @cpt-end:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-scope

        let repo = Arc::clone(&self.repo);
        let ctx = ctx.clone();
        // One transaction for the write and its read-back, so the answer is
        // this request's write even when the subject sends two at once.
        DBProvider::<DomainError>::new(self.db.db())
            .transaction(move |tx| {
                Box::pin(async move {
                    // @cpt-begin:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-store
                    match repo
                        .set_personalization(tx, &scope, key, personalization_enabled)
                        .await?
                    {
                        SettingsWrite::Written => {}
                        SettingsWrite::OutOfScope(refusal) => {
                            return Err(out_of_scope(&ctx, key, refusal));
                        }
                    }
                    // @cpt-end:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-store
                    // @cpt-begin:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-return
                    match repo.find(tx, &scope, key).await? {
                        SettingsLookup::Stored(stored) => Ok(stored),
                        SettingsLookup::Absent => Err(DomainError::internal(format!(
                            "subject settings row missing after the write: tenant {}, subject {}",
                            key.tenant_id, key.subject_id
                        ))),
                        // The write was decided covered with the same scope
                        // and key, so a refused read-back is a defect.
                        SettingsLookup::OutOfScope(refusal) => Err(DomainError::internal(format!(
                            "subject settings read-back refused after an accepted write \
                             ({refusal:?}): tenant {}, subject {}",
                            key.tenant_id, key.subject_id
                        ))),
                    }
                    // @cpt-end:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-return
                })
            })
            .await
    }
}

#[async_trait]
impl<R: SubjectSettingsRepository + 'static> SubjectSettingsReader for SubjectSettingsService<R> {
    async fn read(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        key: SubjectKey,
    ) -> Result<SubjectSettings, DomainError> {
        self.settings_within(ctx, scope, key).await
    }
}
