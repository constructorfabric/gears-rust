use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use simple_user_settings_sdk::models::{
    SimpleUserSettings, SimpleUserSettingsPatch, SimpleUserSettingsUpdate,
};
use simple_user_settings_sdk::owner::SettingsOwnerResolver;
use toolkit::client_hub::{ClientHub, ClientHubError};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::DBProvider;
use toolkit_macros::domain_model;
use toolkit_security::{SecurityContext, pep_properties};
use uuid::Uuid;

use super::error::DomainError;
use super::fields::SettingsFields;
use super::repo::SettingsRepository;

pub(crate) type DbProvider = DBProvider<toolkit_db::DbError>;

/// Authorization resource type for user settings.
///
/// Settings are scoped by tenant + user (resource). The PDP uses
/// `supported_properties` to decide which predicates it can return.
pub(crate) const SETTINGS_RESOURCE: ResourceType = ResourceType::from_static(
    "simple_user_settings.settings",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) mod actions {
    pub const GET: &str = "get";
    pub const UPDATE: &str = "update";
}

// ============================================================================
// Service Configuration
// ============================================================================

#[domain_model]
pub struct ServiceConfig {
    pub max_field_length: usize,
    /// Upper bound on one call to the deployment's [`SettingsOwnerResolver`].
    pub owner_resolver_timeout: Duration,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            max_field_length: 100,
            owner_resolver_timeout: Duration::from_secs(2),
        }
    }
}

// ============================================================================
// Service Implementation
// ============================================================================

#[domain_model]
pub struct Service<R: SettingsRepository> {
    db: Arc<DbProvider>,
    repo: Arc<R>,
    policy_enforcer: PolicyEnforcer,
    config: ServiceConfig,
    /// Where the deployment's [`SettingsOwnerResolver`] is published, if it
    /// publishes one. Read on every request rather than once at startup, so
    /// there is no window in which the service is reachable but has not yet
    /// seen the resolver.
    hub: Arc<ClientHub>,
}

impl<R: SettingsRepository> Service<R> {
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<R>,
        policy_enforcer: PolicyEnforcer,
        config: ServiceConfig,
        hub: Arc<ClientHub>,
    ) -> Self {
        Self {
            db,
            repo,
            policy_enforcer,
            config,
            hub,
        }
    }

    /// The key this caller's settings are filed under.
    ///
    /// The token subject unless the deployment published a resolver that knows
    /// better — see [`SettingsOwnerResolver`]. One place decides it, so a read
    /// and a write can never disagree about whose settings they are.
    async fn owner_of(&self, ctx: &SecurityContext, action: &str) -> Result<Uuid, DomainError> {
        let Some(resolver) = self.owner_resolver(action)? else {
            return Ok(ctx.subject_id());
        };

        let timeout = self.config.owner_resolver_timeout;
        match tokio::time::timeout(timeout, resolver.settings_owner(ctx)).await {
            Ok(Ok(owner)) => Ok(owner.unwrap_or_else(|| ctx.subject_id())),
            Ok(Err(e)) => Err(resolver_failed(ctx, action, &e)),
            Err(_) => Err(resolver_timed_out(ctx, action, timeout)),
        }
    }

    /// The resolver the deployment has published, if any.
    fn owner_resolver(
        &self,
        action: &str,
    ) -> Result<Option<Arc<dyn SettingsOwnerResolver>>, DomainError> {
        match self.hub.get::<dyn SettingsOwnerResolver>() {
            Ok(resolver) => Ok(Some(resolver)),
            Err(ClientHubError::NotFound { .. }) => Ok(None),
            // Something is registered under the resolver's key but is not a
            // resolver. Keying on the subject anyway would fork the settings
            // of every caller the deployment meant to resolve.
            Err(e) => {
                tracing::error!(error = %e, action, "settings owner resolver lookup failed");
                Err(DomainError::internal(format!(
                    "settings owner resolver lookup failed: {e}"
                )))
            }
        }
    }

    pub async fn get_settings(
        &self,
        ctx: &SecurityContext,
    ) -> Result<SimpleUserSettings, DomainError> {
        let user_id = self.owner_of(ctx, actions::GET).await?;
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &SETTINGS_RESOURCE,
                actions::GET,
                Some(user_id),
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        if let Some(settings) = self.repo.find_by_user(&conn, &scope).await? {
            Ok(settings)
        } else {
            Ok(SimpleUserSettings {
                user_id,
                tenant_id,
                theme: None,
                language: None,
            })
        }
    }

    pub async fn update_settings(
        &self,
        ctx: &SecurityContext,
        update: SimpleUserSettingsUpdate,
    ) -> Result<SimpleUserSettings, DomainError> {
        self.validate_field(SettingsFields::THEME, &update.theme)?;
        self.validate_field(SettingsFields::LANGUAGE, &update.language)?;

        let user_id = self.owner_of(ctx, actions::UPDATE).await?;
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &SETTINGS_RESOURCE,
                actions::UPDATE,
                Some(user_id),
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        let settings = self
            .repo
            .upsert_full(
                &conn,
                &scope,
                user_id,
                tenant_id,
                Some(update.theme),
                Some(update.language),
            )
            .await?;
        Ok(settings)
    }

    pub async fn patch_settings(
        &self,
        ctx: &SecurityContext,
        patch: SimpleUserSettingsPatch,
    ) -> Result<SimpleUserSettings, DomainError> {
        if let Some(ref theme) = patch.theme {
            self.validate_field(SettingsFields::THEME, theme)?;
        }
        if let Some(ref language) = patch.language {
            self.validate_field(SettingsFields::LANGUAGE, language)?;
        }

        let user_id = self.owner_of(ctx, "patch").await?;
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &SETTINGS_RESOURCE,
                actions::UPDATE,
                Some(user_id),
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        let settings = self
            .repo
            .upsert_patch(&conn, &scope, user_id, tenant_id, patch)
            .await?;
        Ok(settings)
    }

    fn validate_field(&self, field: &str, value: &str) -> Result<(), DomainError> {
        if value.len() > self.config.max_field_length {
            return Err(DomainError::validation(
                field,
                format!("exceeds maximum length of {}", self.config.max_field_length),
            ));
        }
        Ok(())
    }
}

/// Log a resolver's failure where the caller is known, and say what it means.
fn resolver_failed(ctx: &SecurityContext, action: &str, e: &CanonicalError) -> DomainError {
    tracing::error!(
        error = %cause(e),
        subject_id = %ctx.subject_id(),
        tenant_id = %ctx.subject_tenant_id(),
        action,
        "settings owner resolution failed"
    );
    owner_error(e)
}

/// Log a resolver that ran over its bound; the request is worth retrying.
fn resolver_timed_out(ctx: &SecurityContext, action: &str, timeout: Duration) -> DomainError {
    tracing::error!(
        timeout_ms = timeout.as_millis(),
        subject_id = %ctx.subject_id(),
        tenant_id = %ctx.subject_tenant_id(),
        action,
        "settings owner resolver did not answer in time"
    );
    DomainError::unavailable(format!(
        "settings owner resolver did not answer within {}ms",
        timeout.as_millis()
    ))
}

/// What a resolver's failure means for the request, category by category.
///
/// A resolver that is down is worth retrying, one that refuses the caller is a
/// denial, and anything else is ours to report as internal. The resolver's text
/// travels only as the diagnostic, which the canonical mapping keeps off the
/// wire.
fn owner_error(e: &CanonicalError) -> DomainError {
    let cause = cause(e);
    match e {
        CanonicalError::ServiceUnavailable { .. }
        | CanonicalError::DeadlineExceeded { .. }
        | CanonicalError::ResourceExhausted { .. }
        | CanonicalError::Aborted { .. } => {
            DomainError::unavailable(format!("settings owner resolver unavailable: {cause}"))
        }
        CanonicalError::PermissionDenied { .. } | CanonicalError::Unauthenticated { .. } => {
            DomainError::forbidden(format!(
                "settings owner resolver refused the caller: {cause}"
            ))
        }
        // Deliberately internal, and listed so the choice is visible:
        // `Internal`, `Unknown`, `DataLoss`, `Cancelled`, `Unimplemented`,
        // `InvalidArgument`, `OutOfRange`, `FailedPrecondition`,
        // `AlreadyExists` and `NotFound`. None of them is something the
        // settings caller did or can fix. In particular, "this caller is not
        // in my directory" is not `NotFound`: the contract answers it with
        // `Ok(None)`. `CanonicalError` is `#[non_exhaustive]`, so a category
        // added later lands here too until it is placed above.
        _ => DomainError::internal(format!("settings owner resolution failed: {cause}")),
    }
}

/// A resolver error as text that keeps its cause.
///
/// `Display` on an internal error prints the fixed public detail, and the
/// resolver's actual reason sits in the diagnostic, so both are spelled out.
fn cause(e: &CanonicalError) -> String {
    match e.diagnostic() {
        Some(diagnostic) => format!("{e} ({diagnostic})"),
        None => e.to_string(),
    }
}
