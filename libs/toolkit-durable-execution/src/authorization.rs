//! Authorization port supplied by the embedding gear.
//!
//! The engine never decides privileges itself. Every caller operation and every
//! background step asks the [`Authorizer`] for a fresh [`AccessScope`] and applies it
//! to its secure queries. The embedding gear usually implements the port with the
//! platform policy enforcer and a service identity of its own.
//!
//! The engine keeps the checks that do not depend on policy: it rejects callers
//! without a subject or tenant, narrows owner operations to the calling subject,
//! hides runs of other owners, and bounds every call by the
//! [authorization timeout](crate::EngineBuilder::authorization_timeout).
use std::{error::Error as StdError, fmt, future::Future, sync::Arc, time::Duration};

use async_trait::async_trait;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};

/// Service-scope seam used by background storage operations.
pub mod worker;

/// Resolves access scopes for durable-execution operations.
///
/// Implementations must be cheap to call. [`Self::service_scope`] runs before every
/// worker step and for every row of a recovery or dispatch page, so that a revoked
/// grant stops background work promptly; cache service credentials instead of
/// exchanging them per call. Both methods may be called concurrently from many tasks.
///
/// The engine bounds every call by the
/// [authorization timeout](crate::EngineBuilder::authorization_timeout) and drops the
/// future when it expires, so both futures must be cancel-safe: refresh shared
/// credentials in a single-flight task rather than inline under a lock.
///
/// A scope returned for an action whose [`constraints`](CallerAction::constraints) are
/// [`ConstraintPolicy::Required`] must carry row constraints; the engine rejects an
/// unconstrained scope for such an action as denied.
#[async_trait]
pub trait Authorizer: Send + Sync + 'static {
    /// Scope for an operation requested by `ctx`.
    ///
    /// The engine calls this only for a context with a subject and a tenant.
    ///
    /// # Errors
    /// [`AuthorizationErrorKind::Denied`] when policy rejects the action,
    /// [`AuthorizationErrorKind::Unavailable`] when it cannot be evaluated.
    async fn caller_scope(
        &self,
        ctx: &SecurityContext,
        action: CallerAction,
    ) -> Result<AccessScope, AuthorizationError>;

    /// Scope for background work and trusted catalog management under the
    /// embedding gear's own identity.
    ///
    /// # Errors
    /// Same as [`Self::caller_scope`]. The engine pauses background work while
    /// service authorization fails and retries on the next tick.
    async fn service_scope(&self, action: ServiceAction)
    -> Result<AccessScope, AuthorizationError>;
}

/// Resource an action applies to; forward it to the policy decision point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Resource {
    /// A durable run. Rows are constrained by owner tenant, owner and run id.
    Run,
    /// The workflow definition catalog. Rows are constrained by definition name only.
    Definition,
}

impl Resource {
    /// Stable resource type name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Run => "durable_execution.run",
            Self::Definition => "durable_execution.definition",
        }
    }

    /// Properties the engine can enforce for this resource.
    #[must_use]
    pub const fn properties(self) -> &'static [&'static str] {
        match self {
            Self::Run => &[
                pep_properties::OWNER_TENANT_ID,
                pep_properties::OWNER_ID,
                pep_properties::RESOURCE_ID,
            ],
            Self::Definition => &[pep_properties::RESOURCE_ID],
        }
    }
}

/// Whether the returned scope must carry row constraints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ConstraintPolicy {
    /// The scope must constrain rows; an unconstrained grant is a policy error.
    Required,
    /// An unconstrained scope is expected, for example for catalog management.
    Optional,
}

/// Operation requested by a caller through [`ExecutionClient`](crate::ExecutionClient)
/// or [`ExecutionInspector`](crate::ExecutionInspector).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CallerAction {
    /// Start a run. The engine narrows the scope to the caller as owner.
    Create,
    /// Read progress, events, history or results of an owned run.
    Get,
    /// Cancel, resume, retry or abort an owned run.
    Update,
    /// Cancel or abort a run of another owner.
    Manage,
    /// Read administrative progress of any run in scope.
    Inspect,
    /// List administrative progress of runs in scope.
    List,
}

impl CallerAction {
    /// Stable action name for the policy decision point.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Get => "get",
            Self::Update => "update",
            Self::Manage => "manage",
            Self::Inspect => "inspect",
            Self::List => "list",
        }
    }

    /// Resource the action applies to; always [`Resource::Run`].
    #[must_use]
    pub const fn resource(self) -> Resource {
        Resource::Run
    }

    /// Caller actions always require row constraints.
    #[must_use]
    pub const fn constraints(self) -> ConstraintPolicy {
        ConstraintPolicy::Required
    }
}

/// Background or trusted catalog operation performed under the service identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ServiceAction {
    /// Claim and execute an activity.
    Execute,
    /// Extend an activity lease.
    Heartbeat,
    /// Recover expired leases and stalled runs.
    Recover,
    /// Publish due deliveries to the queue.
    Dispatch,
    /// Cancel runs of a draining definition; needs an unconstrained scope.
    CancelDefinition,
    /// Register a definition in the catalog.
    DefinitionRegister,
    /// Read a definition from the catalog.
    DefinitionGet,
    /// Retire or drain a definition.
    DefinitionUnregister,
    /// Reactivate a definition.
    DefinitionActivate,
    /// List catalog definitions.
    DefinitionList,
    /// Release the bindings of a drained definition.
    DefinitionRelease,
}

impl ServiceAction {
    /// Stable action name for the policy decision point.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Heartbeat => "heartbeat",
            Self::Recover => "recover",
            Self::Dispatch => "dispatch",
            Self::CancelDefinition => "cancel_definition",
            Self::DefinitionRegister => "register",
            Self::DefinitionGet => "get",
            Self::DefinitionUnregister => "unregister",
            Self::DefinitionActivate => "activate",
            Self::DefinitionList => "list",
            Self::DefinitionRelease => "release",
        }
    }

    /// Resource the action applies to.
    #[must_use]
    pub const fn resource(self) -> Resource {
        match self {
            Self::DefinitionRegister
            | Self::DefinitionGet
            | Self::DefinitionUnregister
            | Self::DefinitionActivate
            | Self::DefinitionList
            | Self::DefinitionRelease => Resource::Definition,
            Self::Execute
            | Self::Heartbeat
            | Self::Recover
            | Self::Dispatch
            | Self::CancelDefinition => Resource::Run,
        }
    }

    /// Whether the scope must carry row constraints.
    #[must_use]
    pub const fn constraints(self) -> ConstraintPolicy {
        match (self, self.resource()) {
            (Self::CancelDefinition, _) | (_, Resource::Definition) => ConstraintPolicy::Optional,
            (_, Resource::Run) => ConstraintPolicy::Required,
        }
    }
}

impl fmt::Display for CallerAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for ServiceAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Category of an [`AuthorizationError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AuthorizationErrorKind {
    /// Policy rejected the action. Callers receive `PermissionDenied`.
    Denied,
    /// Policy or credentials cannot be evaluated now. Callers receive `Unavailable`;
    /// background work pauses and retries.
    Unavailable,
}

/// Failure returned by an [`Authorizer`].
///
/// The engine logs the action, resource, rejection reason and, for callers, the
/// subject and tenant ids. The source is never written to logs by the engine; it is
/// returned through [`crate::Error`]'s error chain.
#[derive(Clone)]
pub struct AuthorizationError {
    kind: AuthorizationErrorKind,
    source: Option<Arc<dyn StdError + Send + Sync>>,
}

impl AuthorizationError {
    /// Policy rejected the action.
    #[must_use]
    pub fn denied() -> Self {
        Self {
            kind: AuthorizationErrorKind::Denied,
            source: None,
        }
    }

    /// Policy or credentials are unavailable, without a cause.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            kind: AuthorizationErrorKind::Unavailable,
            source: None,
        }
    }

    /// Attach the underlying cause, for example an `anyhow::Error` or any
    /// `std::error::Error`.
    #[must_use]
    pub fn with_source(mut self, source: impl Into<Box<dyn StdError + Send + Sync>>) -> Self {
        self.source = Some(Arc::from(source.into()));
        self
    }

    /// The failure category.
    #[must_use]
    pub fn kind(&self) -> AuthorizationErrorKind {
        self.kind
    }
}

impl fmt::Debug for AuthorizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorizationError")
            .field("kind", &self.kind)
            .field("has_source", &self.source.is_some())
            .finish()
    }
}

impl fmt::Display for AuthorizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            AuthorizationErrorKind::Denied => f.write_str("durable execution access denied"),
            AuthorizationErrorKind::Unavailable => {
                f.write_str("durable execution authorization is unavailable")
            }
        }
    }
}

impl StdError for AuthorizationError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}

/// The engine side of the port: timeout, nil-identity and constraint guards, and
/// safe diagnostics.
#[derive(Clone)]
pub struct Authority {
    authorizer: Arc<dyn Authorizer>,
    timeout: Duration,
}

/// Why an authorization was rejected; logged as the `reason` field.
#[derive(Clone, Copy)]
enum Rejection {
    Denied,
    Unavailable,
    Timeout,
    UnconstrainedScope,
    MissingIdentity,
}

impl Rejection {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Denied => "denied",
            Self::Unavailable => "unavailable",
            Self::Timeout => "timeout",
            Self::UnconstrainedScope => "unconstrained_scope",
            Self::MissingIdentity => "missing_identity",
        }
    }
}

/// Timeout of an [`Authorizer`] call, kept as the source of the resulting error.
#[derive(Debug, thiserror::Error)]
#[error("authorization did not answer within {0:?}")]
struct AuthorizationTimeout(Duration);

impl Authority {
    /// Wraps `authorizer`; every check that does not answer within `timeout` fails
    /// as unavailable.
    pub(crate) fn new(authorizer: Arc<dyn Authorizer>, timeout: Duration) -> Self {
        Self {
            authorizer,
            timeout,
        }
    }

    /// Scope for a caller operation. Rejects anonymous or tenantless callers before
    /// asking the port, and unconstrained scopes for actions that require constraints.
    pub(crate) async fn caller(
        &self,
        ctx: &SecurityContext,
        action: CallerAction,
    ) -> Result<AccessScope, AuthorizationError> {
        let log = |rejection: Rejection| {
            tracing::warn!(
                action = action.as_str(),
                resource = action.resource().name(),
                reason = rejection.as_str(),
                subject_id = %ctx.subject_id(),
                tenant_id = %ctx.subject_tenant_id(),
                "durable authorization rejected"
            );
        };
        if ctx.subject_id().is_nil() || ctx.subject_tenant_id().is_nil() {
            log(Rejection::MissingIdentity);
            return Err(AuthorizationError::denied());
        }
        let call = self.authorizer.caller_scope(ctx, action);
        self.check(call, action.constraints())
            .await
            .map_err(|(rejection, error)| {
                log(rejection);
                error
            })
    }

    /// Scope for background work under the service identity. Rejects unconstrained
    /// scopes for actions that require constraints.
    pub(crate) async fn service(
        &self,
        action: ServiceAction,
    ) -> Result<AccessScope, AuthorizationError> {
        let call = self.authorizer.service_scope(action);
        self.check(call, action.constraints())
            .await
            .map_err(|(rejection, error)| {
                tracing::warn!(
                    action = action.as_str(),
                    resource = action.resource().name(),
                    reason = rejection.as_str(),
                    "durable service authorization rejected"
                );
                error
            })
    }

    /// Bound the port call by the timeout and enforce the constraint policy.
    async fn check(
        &self,
        call: impl Future<Output = Result<AccessScope, AuthorizationError>>,
        constraints: ConstraintPolicy,
    ) -> Result<AccessScope, (Rejection, AuthorizationError)> {
        let scope = match tokio::time::timeout(self.timeout, call).await {
            Err(_) => {
                return Err((
                    Rejection::Timeout,
                    AuthorizationError::unavailable()
                        .with_source(AuthorizationTimeout(self.timeout)),
                ));
            }
            Ok(Err(error)) => {
                let rejection = match error.kind {
                    AuthorizationErrorKind::Denied => Rejection::Denied,
                    AuthorizationErrorKind::Unavailable => Rejection::Unavailable,
                };
                return Err((rejection, error));
            }
            Ok(Ok(scope)) => scope,
        };
        if constraints == ConstraintPolicy::Required && scope.is_unconstrained() {
            return Err((Rejection::UnconstrainedScope, AuthorizationError::denied()));
        }
        Ok(scope)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/unit/authorization_tests.rs"]
mod tests;
