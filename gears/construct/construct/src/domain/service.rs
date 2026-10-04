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

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            max_text_length: 1000,
        }
    }
}

#[domain_model]
pub struct Service<R: NoteRepository> {
    db: Arc<DbProvider>,
    repo: Arc<R>,
    policy_enforcer: PolicyEnforcer,
    config: ServiceConfig,
}

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

    pub async fn create_note(
        &self,
        ctx: &SecurityContext,
        new: NewFoundationNote,
    ) -> Result<FoundationNote, DomainError> {
        self.validate_text(&new.text)?;

        let tenant_id = ctx.subject_tenant_id();
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

        let conn = self.db.conn().map_err(DomainError::from)?;
        let note = FoundationNote {
            id: Uuid::new_v4(),
            tenant_id,
            text: new.text,
        };
        self.repo.insert(&conn, &scope, note).await
    }

    pub async fn get_note(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<FoundationNote, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
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

        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repo
            .find_by_id(&conn, &scope, id)
            .await?
            .ok_or(DomainError::NotFound)
    }

    fn validate_text(&self, text: &str) -> Result<(), DomainError> {
        if text.trim().is_empty() {
            return Err(DomainError::validation("text", "must not be empty"));
        }
        if text.contains('\0') {
            return Err(DomainError::validation(
                "text",
                "must not contain the NUL character",
            ));
        }
        if text.len() > self.config.max_text_length {
            return Err(DomainError::validation(
                "text",
                format!("exceeds maximum length of {}", self.config.max_text_length),
            ));
        }
        Ok(())
    }
}
