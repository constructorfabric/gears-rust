use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::AccessRequest;
use modkit_macros::domain_model;
use modkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::models::ResolvedModel;
use crate::domain::repos::ModelResolver;

use super::{DbProvider, actions, resources};

/// Service handling model listing and selection.
#[domain_model]
pub struct ModelService {
    _db: Arc<DbProvider>,
    enforcer: PolicyEnforcer,
    model_resolver: Arc<dyn ModelResolver>,
}

impl ModelService {
    pub(crate) fn new(
        db: Arc<DbProvider>,
        enforcer: PolicyEnforcer,
        model_resolver: Arc<dyn ModelResolver>,
    ) -> Self {
        Self {
            _db: db,
            enforcer,
            model_resolver,
        }
    }

    /// Resolve a model ID + provider from the policy catalog.
    pub(crate) async fn resolve_model(
        &self,
        user_id: Uuid,
        model: Option<String>,
    ) -> Result<ResolvedModel, DomainError> {
        self.model_resolver.resolve_model(user_id, model).await
    }

    /// List all globally enabled models visible to the authenticated user.
    pub(crate) async fn list_models(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<ResolvedModel>, DomainError> {
        // Permission check only — no constraints needed for catalog data.
        self.enforcer
            .access_scope_with(
                ctx,
                &resources::MODEL,
                actions::LIST,
                None,
                &AccessRequest::new().require_constraints(false),
            )
            .await?;

        self.model_resolver
            .list_visible_models(ctx.subject_id())
            .await
    }

    /// Get a single globally enabled model by ID.
    ///
    /// Returns `ModelNotFound` if the model does not exist or is globally disabled
    /// (to avoid leaking catalog details).
    pub(crate) async fn get_model(
        &self,
        ctx: &SecurityContext,
        model_id: &str,
    ) -> Result<ResolvedModel, DomainError> {
        // Permission check only — no constraints needed for catalog data.
        self.enforcer
            .access_scope_with(
                ctx,
                &resources::MODEL,
                actions::READ,
                None,
                &AccessRequest::new().require_constraints(false),
            )
            .await?;

        self.model_resolver
            .get_visible_model(ctx.subject_id(), model_id)
            .await
    }
}

#[cfg(test)]
#[path = "model_service_test.rs"]
mod tests;
