// Updated: 2026-04-14 by Constructor Tech
//! Policy Enforcement Point (`PEP`) object.
//!
//! [`PolicyEnforcer`] encapsulates the full PEP flow:
//! build evaluation request → call PDP → compile constraints to `AccessScope`.
//!
//! Constructed once during service initialisation with the `AuthZ` client.
//! The resource type is supplied per call via a [`ResourceType`] descriptor,
//! so a single enforcer can serve all resource types in a service.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use toolkit_security::{AccessScope, SecurityContext};

use super::IntoPropertyValue;
use uuid::Uuid;

use crate::api::AuthZResolverClient;
use crate::error::AuthZResolverError;
use crate::models::{
    Action, BarrierMode, Capability, EvaluationRequest, EvaluationRequestContext, Resource,
    Subject, TenantContext, TenantMode,
};
use crate::pep::compiler::{ConstraintCompileError, compile_to_access_scope};

/// Error from the PEP enforcement flow.
#[derive(Debug, thiserror::Error)]
pub enum EnforcerError {
    /// The PDP explicitly denied access.
    #[error("access denied by PDP")]
    Denied {
        /// Optional deny reason from the PDP.
        deny_reason: Option<crate::models::DenyReason>,
    },

    /// The `AuthZ` evaluation RPC failed.
    #[error("authorization evaluation failed: {0}")]
    EvaluationFailed(#[from] AuthZResolverError),

    /// Constraint compilation failed (missing or unsupported constraints).
    #[error("constraint compilation failed: {0}")]
    CompileFailed(#[from] ConstraintCompileError),
}

/// Per-request evaluation parameters for advanced authorization scenarios.
///
/// Used with [`PolicyEnforcer::access_scope_with()`] when the simple
/// [`PolicyEnforcer::access_scope()`] defaults don't suffice (ABAC resource
/// properties, custom tenant mode, barrier bypass, etc.).
///
/// All fields default to "not overridden" - only set what you need.
///
/// # Examples
///
/// ```ignore
/// use authz_resolver_sdk::pep::{AccessRequest, PolicyEnforcer, ResourceType};
///
/// // CREATE with target tenant + resource properties (constrained scope)
/// let scope = enforcer.access_scope_with(
///     &ctx, &RESOURCE, "create", None,
///     &AccessRequest::new()
///         .context_tenant_id(target_tenant_id)
///         .tenant_mode(TenantMode::RootOnly)
///         .resource_property(pep_properties::OWNER_TENANT_ID, target_tenant_id),
/// ).await?;
///
/// // Billing - ignore barriers (constrained scope)
/// let scope = enforcer.access_scope_with(
///     &ctx, &RESOURCE, "list", None,
///     &AccessRequest::new().barrier_mode(BarrierMode::Ignore),
/// ).await?;
/// ```
#[derive(Debug, Clone, Default)]
pub struct AccessRequest {
    resource_properties: HashMap<String, serde_json::Value>,
    tenant_context: Option<TenantContext>,
    require_constraints: Option<bool>,
}

impl AccessRequest {
    /// Create a new empty access request (all defaults).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a single resource property for ABAC evaluation.
    #[must_use]
    pub fn resource_property(
        mut self,
        key: impl Into<String>,
        value: impl IntoPropertyValue,
    ) -> Self {
        self.resource_properties
            .insert(key.into(), value.into_filter_value());
        self
    }

    /// Set all resource properties at once (replaces any previously set).
    #[must_use]
    pub fn resource_properties(mut self, props: HashMap<String, serde_json::Value>) -> Self {
        self.resource_properties = props;
        self
    }

    /// Override the context tenant ID (default: subject's tenant).
    #[must_use]
    pub fn context_tenant_id(mut self, id: Uuid) -> Self {
        self.tenant_context.get_or_insert_default().root_id = Some(id);
        self
    }

    /// Override the tenant hierarchy mode (default: `Subtree`).
    #[must_use]
    pub fn tenant_mode(mut self, mode: TenantMode) -> Self {
        self.tenant_context.get_or_insert_default().mode = mode;
        self
    }

    /// Override the barrier enforcement mode (default: `Respect`).
    #[must_use]
    pub fn barrier_mode(mut self, mode: BarrierMode) -> Self {
        self.tenant_context.get_or_insert_default().barrier_mode = mode;
        self
    }

    /// Set a tenant status filter (e.g., `["active"]`).
    #[must_use]
    pub fn tenant_status(mut self, statuses: Vec<String>) -> Self {
        self.tenant_context.get_or_insert_default().tenant_status = Some(statuses);
        self
    }

    /// Set the entire tenant context at once.
    #[must_use]
    pub fn tenant_context(mut self, tc: TenantContext) -> Self {
        self.tenant_context = Some(tc);
        self
    }

    /// Override the `require_constraints` flag (default: `true`).
    ///
    /// When `false`, the PDP is told that constraints are optional.
    /// If the PDP returns no constraints, the resulting scope is
    /// `allow_all()` (no row-level filtering). If the PDP still returns
    /// constraints, they are compiled normally.
    ///
    /// Primary use cases:
    /// - **GET with prefetch**: if scope is unconstrained, return the
    ///   prefetched entity directly; otherwise do a scoped re-read.
    /// - **CREATE**: if scope is unconstrained, skip insert validation;
    ///   otherwise validate the insert against the scope.
    #[must_use]
    pub fn require_constraints(mut self, require: bool) -> Self {
        self.require_constraints = Some(require);
        self
    }
}

/// Descriptor for a resource type and its supported constraint properties.
///
/// Passed per call to [`PolicyEnforcer`] methods so a single enforcer can
/// serve multiple resource types within one service.
///
/// Construct with [`ResourceType::from_static`] for compile-time literals
/// (typical case) or [`ResourceType::new`] for runtime-built names such as
/// chained GTS schema ids (e.g. `gts.cf.core.am.tenant_metadata.v1~<chain>~`).
#[derive(Debug, Clone)]
pub struct ResourceType {
    name: Cow<'static, str>,
    supported_properties: &'static [&'static str],
}

impl ResourceType {
    /// Create a descriptor from a compile-time string literal.
    ///
    /// Usable in `const` context; performs no allocation.
    #[must_use]
    pub const fn from_static(
        name: &'static str,
        supported_properties: &'static [&'static str],
    ) -> Self {
        Self {
            name: Cow::Borrowed(name),
            supported_properties,
        }
    }

    /// Create a descriptor with a name owned or borrowed at runtime.
    ///
    /// Accepts any value convertible to [`Cow<'static, str>`] — `&'static str`,
    /// [`String`], or [`Cow<'static, str>`]. Use this when the resource type
    /// name is built at runtime (e.g. a chained GTS schema id whose tail
    /// depends on a configured plugin).
    #[must_use]
    pub fn new(
        name: impl Into<Cow<'static, str>>,
        supported_properties: &'static [&'static str],
    ) -> Self {
        Self {
            name: name.into(),
            supported_properties,
        }
    }

    /// Dotted resource type name (for example, a `gts_id!(...)` value).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Properties the PEP can compile from PDP constraints.
    #[must_use]
    pub fn supported_properties(&self) -> &'static [&'static str] {
        self.supported_properties
    }
}

/// Policy Enforcement Point.
///
/// Holds the `AuthZ` client and optional PEP capabilities.
/// Constructed once during service init; cloneable and cheap to pass
/// around (`Arc` inside). The resource type is supplied per call via
/// [`ResourceType`].
///
/// # Example
///
/// ```ignore
/// use authz_resolver_sdk::pep::{PolicyEnforcer, ResourceType};
/// use toolkit_security::pep_properties;
///
/// const USER: ResourceType = ResourceType::from_static(
///     gts_id!("cf.core.users.user.v1~"),
///     &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
/// );
///
/// let enforcer = PolicyEnforcer::new(authz.clone());
///
/// // All CRUD operations return AccessScope (PDP always returns constraints)
/// let scope = enforcer.access_scope(&ctx, &USER, "get", Some(id)).await?;
/// let scope = enforcer.access_scope(&ctx, &USER, "create", None).await?;
/// ```
#[derive(Clone)]
pub struct PolicyEnforcer {
    authz: Arc<dyn AuthZResolverClient>,
    capabilities: Vec<Capability>,
}

impl PolicyEnforcer {
    /// Create a new enforcer.
    pub fn new(authz: Arc<dyn AuthZResolverClient>) -> Self {
        Self {
            authz,
            capabilities: Vec::new(),
        }
    }

    /// Set PEP capabilities advertised to the PDP.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: Vec<Capability>) -> Self {
        self.capabilities = capabilities;
        self
    }

    // ── Low-level: build request only ────────────────────────────────

    /// Build an evaluation request using the subject's tenant as context tenant
    /// and default settings.
    #[must_use]
    pub fn build_request(
        &self,
        ctx: &SecurityContext,
        resource: &ResourceType,
        action: &str,
        resource_id: Option<Uuid>,
        require_constraints: bool,
    ) -> EvaluationRequest {
        self.build_request_with(
            ctx,
            resource,
            action,
            resource_id,
            require_constraints,
            &AccessRequest::default(),
        )
    }

    /// Build an evaluation request with per-request overrides from [`AccessRequest`].
    #[must_use]
    pub fn build_request_with(
        &self,
        ctx: &SecurityContext,
        resource: &ResourceType,
        action: &str,
        resource_id: Option<Uuid>,
        require_constraints: bool,
        request: &AccessRequest,
    ) -> EvaluationRequest {
        // Pass through the caller's tenant context as-is.
        // If no context_tenant_id was set, the PDP determines it by its own rules
        // (e.g. falling back to subject.properties["tenant_id"]).
        let tenant_context = request.tenant_context.clone();

        // Put subject's tenant_id into properties per AuthZEN spec
        let mut subject_properties = HashMap::new();
        subject_properties.insert(
            "tenant_id".to_owned(),
            serde_json::Value::String(ctx.subject_tenant_id().to_string()),
        );

        let bearer_token = ctx.bearer_token().cloned();

        EvaluationRequest {
            subject: Subject {
                id: ctx.subject_id(),
                subject_type: ctx.subject_type().map(ToOwned::to_owned),
                properties: subject_properties,
            },
            action: Action {
                name: action.to_owned(),
            },
            resource: Resource {
                resource_type: resource.name.clone().into_owned(),
                id: resource_id,
                properties: request.resource_properties.clone(),
            },
            context: EvaluationRequestContext {
                tenant_context,
                token_scopes: ctx.token_scopes().to_vec(),
                require_constraints,
                capabilities: self.capabilities.clone(),
                supported_properties: resource
                    .supported_properties
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
                bearer_token,
            },
        }
    }

    // ── High-level: full PEP flow (all CRUD operations) ─────────────

    /// Execute the full PEP flow with constraints: build request → evaluate
    /// → compile constraints to `AccessScope`.
    ///
    /// Always sets `require_constraints=true`. PDP returns constraints for
    /// all CRUD operations (GET, LIST, UPDATE, DELETE, CREATE).
    ///
    /// # Errors
    ///
    /// - [`EnforcerError::EvaluationFailed`] if the PDP call fails
    /// - [`EnforcerError::CompileFailed`] if constraint compilation fails (denied, missing, etc.)
    pub async fn access_scope(
        &self,
        ctx: &SecurityContext,
        resource: &ResourceType,
        action: &str,
        resource_id: Option<Uuid>,
    ) -> Result<AccessScope, EnforcerError> {
        self.access_scope_with(
            ctx,
            resource,
            action,
            resource_id,
            &AccessRequest::default(),
        )
        .await
    }

    /// Execute the full PEP flow with constraints and per-request overrides.
    ///
    /// Uses `require_constraints` from [`AccessRequest`] (default: `true`).
    /// When `false`, the PDP may return no constraints; the resulting scope
    /// is `allow_all()`. When `true`, empty constraints trigger a compile error.
    ///
    /// # Errors
    ///
    /// - [`EnforcerError::EvaluationFailed`] if the PDP call fails
    /// - [`EnforcerError::CompileFailed`] if constraint compilation fails (denied, missing, etc.)
    pub async fn access_scope_with(
        &self,
        ctx: &SecurityContext,
        resource: &ResourceType,
        action: &str,
        resource_id: Option<Uuid>,
        request: &AccessRequest,
    ) -> Result<AccessScope, EnforcerError> {
        let require = request.require_constraints.unwrap_or(true);
        let eval_request =
            self.build_request_with(ctx, resource, action, resource_id, require, request);
        let response = self.authz.evaluate(eval_request).await?;

        // Check decision first: if denied, return error immediately
        // without attempting constraint compilation.
        if !response.decision {
            return Err(EnforcerError::Denied {
                deny_reason: response.context.deny_reason,
            });
        }

        Ok(compile_to_access_scope(
            &response,
            require,
            resource.supported_properties,
        )?)
    }
}

impl std::fmt::Debug for PolicyEnforcer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyEnforcer")
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "enforcer_tests.rs"]
mod enforcer_tests;
