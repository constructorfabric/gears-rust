use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use construct_sdk::models::{FoundationNote, NewFoundationNote};
use toolkit_db::DBProvider;
use toolkit_macros::domain_model;
use toolkit_security::{SecurityContext, pep_properties};
use uuid::Uuid;

use super::error::DomainError;
use super::repo::NoteRepository;

pub(crate) type DbProvider = DBProvider<toolkit_db::DbError>;

/// Authorization resource type for foundation notes, scoped by tenant and
/// note id. The PDP uses `supported_properties` to decide which predicates it
/// can return.
pub(crate) const NOTE_RESOURCE: ResourceType = ResourceType::from_static(
    "construct.foundation_note",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) mod actions {
    pub const CREATE: &str = "create";
    pub const GET: &str = "get";
}

#[domain_model]
pub struct ServiceConfig {
    pub max_text_length: usize,
}

#[domain_model]
pub struct Service<R: NoteRepository> {
    db: Arc<DbProvider>,
    repo: Arc<R>,
    policy_enforcer: PolicyEnforcer,
    config: ServiceConfig,
}

/// @cpt-dod:cpt-cf-construct-dod-gear-foundation-access-scope:p1
impl<R: NoteRepository> Service<R> {
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<R>,
        policy_enforcer: PolicyEnforcer,
        config: ServiceConfig,
    ) -> Self {
        Self {
            db,
            repo,
            policy_enforcer,
            config,
        }
    }

    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id()))]
    pub async fn create_note(
        &self,
        ctx: &SecurityContext,
        new: NewFoundationNote,
    ) -> Result<FoundationNote, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-validate
        self.validate_text(&new.text)?;
        // @cpt-end:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-validate

        // @cpt-begin:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-scope
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-tenant
        let tenant_id = ctx.subject_tenant_id();
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-tenant
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-evaluate
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &NOTE_RESOURCE,
                actions::CREATE,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-evaluate
        // @cpt-end:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-scope

        // @cpt-begin:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-insert
        // @cpt-begin:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-insert-fail-return
        let conn = self.db.conn().map_err(DomainError::from)?;
        // @cpt-end:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-insert-fail-return
        // @cpt-end:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-insert
        // @cpt-begin:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-build
        let note = FoundationNote::new(Uuid::new_v4(), tenant_id, new.text);
        // @cpt-end:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-build
        // @cpt-begin:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-insert
        self.repo.insert(&conn, &scope, note).await
        // @cpt-end:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-insert
    }

    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), note_id = %id))]
    pub async fn get_note(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<FoundationNote, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-get-scope
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-tenant
        let tenant_id = ctx.subject_tenant_id();
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-tenant
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-evaluate
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &NOTE_RESOURCE,
                actions::GET,
                Some(id),
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-evaluate
        // @cpt-end:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-get-scope

        // @cpt-begin:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-get-select
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repo
            .find_by_id(&conn, &scope, id)
            .await?
            // @cpt-end:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-get-select
            // @cpt-begin:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-get-missing
            .ok_or(DomainError::NotFound)
        // @cpt-end:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-get-missing
    }

    /// @cpt-dod:cpt-cf-construct-dod-gear-foundation-text-validation:p1
    fn validate_text(&self, text: &str) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-validate-text:p1:inst-text-blank
        if text.trim().is_empty() {
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-validate-text:p1:inst-text-blank-return
            return Err(DomainError::validation("text", "must not be empty"));
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-validate-text:p1:inst-text-blank-return
        }
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-validate-text:p1:inst-text-blank
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-validate-text:p1:inst-text-nul
        if text.contains('\0') {
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-validate-text:p1:inst-text-nul-return
            return Err(DomainError::validation(
                "text",
                "must not contain the NUL character",
            ));
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-validate-text:p1:inst-text-nul-return
        }
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-validate-text:p1:inst-text-nul
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-validate-text:p1:inst-text-long
        if text.len() > self.config.max_text_length {
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-validate-text:p1:inst-text-long-return
            return Err(DomainError::validation(
                "text",
                format!("exceeds maximum length of {}", self.config.max_text_length),
            ));
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-validate-text:p1:inst-text-long-return
        }
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-validate-text:p1:inst-text-long
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-validate-text:p1:inst-text-ok
        Ok(())
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-validate-text:p1:inst-text-ok
    }
}
