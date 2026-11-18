// @cpt-dod:cpt-cf-settings-service-dod-category-management-authorization:p1
//! The authorization enforcement point.
//!
//! Every handler that touches a settings resource obtains its [`AccessScope`]
//! here first. The scope is not advisory: it is the query and visibility
//! predicate the handler applies, so a caller entitled to part of the tree
//! cannot read past it by asking differently.
//!
//! # Failing closed is structural, not a policy this module applies
//!
//! The platform's `PolicyEnforcer` returns `Result<AccessScope, EnforcerError>`
//! — an explicit deny, an unreachable decision point, and an unusable set of
//! constraints are all `Err`. There is no path that yields a scope without a
//! positive decision, so a handler cannot proceed on an unknown verdict by
//! forgetting a check: it has nothing to proceed *with*.
//!
//! What this module adds is the projection of that failure into the gear's own
//! error vocabulary, and the guarantee that **every** failure shape denies. A
//! new `EnforcerError` variant must be handled here or the match stops
//! compiling.
//!
//! # A handler can only ask for a listed permission
//!
//! The platform enforcer takes a resource type and an action as free values.
//! The gear does not hand it out: [`PermissionEnforcer`] owns it in a private
//! field, and its one method, [`PermissionEnforcer::access_scope`], takes a
//! [`Permission`]. That wrapper is what the gear stores and what every handler
//! and service receives, so the only question any of them can put to the
//! policy decision point is one of the rows of [`Permission`]. Its two probes,
//! [`PermissionEnforcer::optional_scope`] and
//! [`PermissionEnforcer::may_read_unmasked`], ask the same rows and differ only
//! in what a refusal means to the request.

use std::sync::Arc;

use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use authz_resolver_sdk::{EnforcerError, PolicyEnforcer};
use toolkit::client_hub::ClientHub;
use toolkit_security::AccessScope;

use crate::domain::error::DomainError;
use crate::domain::metric_labels::{AuthzReason, Operation};
use crate::domain::ports::SecurityMetrics;

/// The settings resources authorization decisions are made against.
///
/// These are the GTS type ids the policy decision point knows, so they are the
/// vocabulary a policy is written in — not an internal naming choice.
///
/// Each is built from the SDK constant its stub is published under
/// (`settings_service_sdk::gts`), so a permission cannot name a type the gear
/// does not register.
mod resource {
    use super::ResourceType;

    /// The type ids, as the `'static` strings a denial carries.
    pub(super) mod id {
        use settings_service_sdk::gts;

        pub(in super::super) const DECLARATION: &str = gts::DECLARATION_SCHEMA;
        pub(in super::super) const VALUE: &str = gts::VALUE_SCHEMA;
        pub(in super::super) const CATEGORY: &str = gts::CATEGORY_SCHEMA;
    }

    /// A setting declaration — the record of what a setting *is*.
    pub(super) const DECLARATION: ResourceType = ResourceType::from_static(id::DECLARATION, &[]);

    /// A stored setting value at some scope.
    pub(super) const VALUE: ResourceType = ResourceType::from_static(id::VALUE, &[]);

    /// A settings category.
    pub(super) const CATEGORY: ResourceType = ResourceType::from_static(id::CATEGORY, &[]);
}

/// Declares [`Permission`] and everything derived from its list, so the list
/// cannot be incomplete: the variants, [`Permission::ALL`], and the
/// accessors all come from the same rows.
macro_rules! permissions {
    ($($(#[$meta:meta])* $variant:ident => ($resource:ident, $action:literal)),+ $(,)?) => {
        /// What a handler asks the policy decision point for: one action on one
        /// settings resource type.
        ///
        /// This is the closed list of what the handlers enforce, and the list
        /// the gear's published permission catalogue (`crate::gts::permissions`)
        /// is declared from. A handler reaches the policy decision point only
        /// through [`PermissionEnforcer::access_scope`], which takes a
        /// `Permission` and nothing else, so a handler cannot ask for a pair
        /// that is not here, and a test pins every variant to exactly one
        /// catalogue entry. A role is composed of the catalogue's entries, so
        /// what can be granted cannot drift from what is enforced.
        ///
        /// Adding an enforced pair is three steps: a variant here, an instance
        /// in `crate::gts::permissions`, and, for a resource type not yet
        /// listed, the type's stub in the SDK (`settings_service_sdk::gts`),
        /// which the types registry needs to resolve a role's target type. The
        /// read of a change set's activation status, which the events and
        /// activation change adds, has to be added as a variant after that
        /// change merges.
        ///
        /// A policy is written against the spellings [`Permission::action`] and
        /// [`Permission::resource`] return: renaming one revokes every grant that
        /// names it.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Permission {
            $($(#[$meta])* $variant),+
        }

        impl Permission {
            /// Every permission, in declaration order.
            pub const ALL: &'static [Permission] = &[$(Permission::$variant),+];

            /// The resource type the permission is asked on.
            #[must_use]
            pub fn resource(self) -> ResourceType {
                match self {
                    $(Permission::$variant => resource::$resource),+
                }
            }

            /// The resource type's GTS id, as a `'static` string: what a
            /// denial carries, so it cannot hold a caller-supplied value even
            /// by accident.
            #[must_use]
            pub const fn resource_id(self) -> &'static str {
                match self {
                    $(Permission::$variant => resource::id::$resource),+
                }
            }

            /// The action, as the policy decision point spells it.
            #[must_use]
            pub const fn action(self) -> &'static str {
                match self {
                    $(Permission::$variant => $action),+
                }
            }
        }
    };
}

permissions! {
    /// Browse and read categories.
    CategoryRead => (CATEGORY, "read"),
    /// Create a category.
    CategoryCreate => (CATEGORY, "create"),
    /// Change a category's descriptive fields.
    CategoryUpdate => (CATEGORY, "update"),
    /// Delete an empty category.
    CategoryDelete => (CATEGORY, "delete"),
    /// Read declarations: list, get, search.
    DeclarationRead => (DECLARATION, "read"),
    /// Create a declaration, or reactivate a retired one.
    DeclarationCreate => (DECLARATION, "create"),
    /// Change a declaration's descriptive fields.
    DeclarationUpdate => (DECLARATION, "update"),
    /// Retire a declaration, or purge a retired one.
    DeclarationDelete => (DECLARATION, "delete"),
    /// Everything that reads values at a scope: resolve, browse, search and the
    /// history, the source side of a clone, validate and impact, the tenant
    /// access read and listing, the value side of the revival report, the
    /// category tallies, and the machine-only `resolve_secret` path, which
    /// hands a calling service the plaintext of a secret. Whoever is granted
    /// this on a service principal is granted that plaintext too.
    ValueRead => (VALUE, "read"),
    /// Read a value the classification would otherwise mask (`pii`).
    ValueReadUnmasked => (VALUE, "read_unmasked"),
    /// Write a value at a scope.
    ValueWrite => (VALUE, "write"),
    /// Restrict a descendant's access to a setting: a distinct power from
    /// writing its values.
    ValueDelegate => (VALUE, "delegate"),
}

impl Permission {
    /// The `operation` a refusal of this permission is counted under
    /// (`settings_authz_denial_total`): the action, as the label set spells it.
    ///
    /// `None` for the unmasked-read entitlement: it is a probe answered by
    /// masking, asked through [`PermissionEnforcer::may_read_unmasked`], and
    /// having no operation keeps it off the counted path whichever method asks.
    #[must_use]
    pub const fn operation(self) -> Option<Operation> {
        match self {
            Permission::CategoryRead | Permission::DeclarationRead | Permission::ValueRead => {
                Some(Operation::Read)
            }
            Permission::CategoryCreate | Permission::DeclarationCreate => Some(Operation::Create),
            Permission::CategoryUpdate | Permission::DeclarationUpdate => Some(Operation::Update),
            Permission::CategoryDelete | Permission::DeclarationDelete => Some(Operation::Delete),
            Permission::ValueWrite => Some(Operation::Write),
            Permission::ValueDelegate => Some(Operation::Delegate),
            Permission::ValueReadUnmasked => None,
        }
    }
}

/// Project an enforcement failure into the gear's error vocabulary.
///
/// Every known outcome denies. `EnforcerError` is `#[non_exhaustive]`: an
/// outcome class added upstream is an internal failure ([`DomainError::Internal`],
/// HTTP 500, no detail on the wire), never a default that might allow.
///
/// The three known outcomes produce the **same** [`DomainError::Unauthorized`],
/// which carries no identifier and no existence hint. A caller cannot learn from the
/// response whether the policy point was down, whether a policy explicitly
/// denied them, or whether the resource exists at all — and it should not: each
/// of those is information about a resource it has not been granted.
#[must_use]
fn deny(permission: Permission, err: &EnforcerError) -> DomainError {
    match err {
        // The decision point said no, or permitted a scope that matches nothing.
        EnforcerError::AccessDenied(_)
        // The decision point could not be reached. Step 4 of the enforcement
        // algorithm: fail closed rather than proceed on an unknown verdict.
        | EnforcerError::EvaluationFailed { .. }
        // A decision arrived but its constraints could not be compiled into a
        // scope. Allowing here would mean applying no predicate at all, which
        // is broader than any decision the policy point could have returned.
        | EnforcerError::CompileFailed { .. } => DomainError::Unauthorized {
            resource: permission.resource_id(),
        },
        // An unknown outcome class: stop with an internal failure; its detail
        // stays in the in-process diagnostic.
        unknown => DomainError::Internal {
            diagnostic: format!("authorization failed with an unknown outcome: {unknown}"),
        },
    }
}

/// Why the enforcement point refused, as the `reason` label spells it; `None`
/// for an outcome class this gear does not know. `EnforcerError` is
/// `#[non_exhaustive]`, and an unknown outcome is an internal failure
/// ([`deny`]), not a refusal, so it is not counted as one.
fn refusal_reason(err: &EnforcerError) -> Option<AuthzReason> {
    match err {
        EnforcerError::AccessDenied(_) => Some(AuthzReason::Denied),
        EnforcerError::EvaluationFailed { .. } => Some(AuthzReason::Unavailable),
        EnforcerError::CompileFailed { .. } => Some(AuthzReason::Constraints),
        _ => None,
    }
}

/// The gear's enforcement point: the platform enforcer, narrowed to the
/// questions in [`Permission`].
///
/// The platform's `PolicyEnforcer` accepts any resource type and any action
/// string. It is held here in a private field with no accessor, so nothing
/// outside this module can call it: a handler holds a `PermissionEnforcer` and
/// can only ask [`access_scope`](Self::access_scope), or one of the two
/// probes, for a `Permission`. That is what keeps the enforced pairs equal to
/// the published catalogue.
///
/// The gear shares one behind an `Arc`.
pub struct PermissionEnforcer {
    inner: PolicyEnforcer,
}

impl PermissionEnforcer {
    /// An enforcer that finds the policy decision point in the hub at each
    /// call, and denies when it is not there.
    #[must_use]
    pub fn from_hub(hub: Arc<ClientHub>) -> Self {
        Self {
            inner: PolicyEnforcer::from_hub(hub),
        }
    }

    /// An enforcer over a decision point a test supplies.
    #[cfg(test)]
    #[must_use]
    pub fn over(inner: PolicyEnforcer) -> Self {
        Self { inner }
    }

    /// Obtain the caller's access scope for a permission on a settings
    /// resource.
    ///
    /// A refusal fails the request and is counted on `metrics`.
    ///
    /// # Errors
    ///
    /// [`DomainError::Unauthorized`] for every known enforcement failure: an
    /// explicit deny, an unreachable decision point and an unusable set of
    /// constraints all produce the same error — see [`deny`].
    pub async fn access_scope(
        &self,
        metrics: &dyn SecurityMetrics,
        ctx: &toolkit_security::SecurityContext,
        permission: Permission,
        resource_id: Option<uuid::Uuid>,
    ) -> Result<AccessScope, DomainError> {
        self.enforce(ctx, permission, Some(metrics), resource_id)
            .await
    }

    /// The same decision as [`access_scope`](Self::access_scope), for a probe
    /// whose refusal does not fail the request: the category tallies, answered
    /// by omitting the counts. The scope, or `None` when the decision is a
    /// known refusal ([`DomainError::Unauthorized`]); any other failure, such
    /// as the internal failure an unknown enforcer outcome maps to, propagates
    /// and never reads as a refusal. Nothing is counted.
    ///
    /// # Errors
    ///
    /// Every enforcement failure other than [`DomainError::Unauthorized`].
    pub async fn optional_scope(
        &self,
        ctx: &toolkit_security::SecurityContext,
        permission: Permission,
    ) -> Result<Option<AccessScope>, DomainError> {
        optional(self.enforce(ctx, permission, None, None).await)
    }

    /// Whether the caller may see `pii` values unmasked: a separate decision
    /// on the value resource, answered by masking when it is a known refusal.
    /// A probe, so nothing is counted; any other failure is an error, never a
    /// mask.
    ///
    /// # Errors
    ///
    /// Every enforcement failure other than [`DomainError::Unauthorized`].
    pub async fn may_read_unmasked(
        &self,
        ctx: &toolkit_security::SecurityContext,
    ) -> Result<bool, DomainError> {
        Ok(optional(
            self.enforce(ctx, Permission::ValueReadUnmasked, None, None)
                .await,
        )?
        .is_some())
    }

    /// The one decision behind [`access_scope`](Self::access_scope),
    /// [`optional_scope`](Self::optional_scope) and
    /// [`may_read_unmasked`](Self::may_read_unmasked): `counted` is the metrics
    /// port a refusal is counted on, and is `None` for a probe. The label is
    /// the permission's [`Permission::operation`], so a permission without one
    /// is never a label.
    async fn enforce(
        &self,
        ctx: &toolkit_security::SecurityContext,
        permission: Permission,
        counted: Option<&dyn SecurityMetrics>,
        resource_id: Option<uuid::Uuid>,
    ) -> Result<AccessScope, DomainError> {
        let resource = &permission.resource();
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-3
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-8
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-9
        // `require_constraints(false)`, for every resource here, on purpose.
        //
        // None of the resources declares a PEP property (`resource::*` above
        // are all `&[]`), so there is nothing a scope constraint could clamp to.
        // Under the default the compiler fails such a request closed --
        // `ConstraintsRequiredButAbsent` -- and the static plugin logs exactly
        // that: "PEP requires constraints but declares none of the properties this
        // plugin constrains". Category and declaration are platform-global rows
        // with no tenant column, which is the whole story for them.
        //
        // A value row does carry `tenant_id`, and it still must not be clamped
        // here. Resolving a `cascading` or `global` setting reads the rows of the
        // caller's *ancestors* -- tenants outside any subtree a policy would clamp
        // the caller to -- so a PDP constraint on `tenant_id` would break
        // resolution rather than protect it (the resolver reads the chain under
        // `AccessScope::allow_all()` for exactly this reason). The tenant boundary
        // for values is enforced by the write and read gates instead:
        // `ValueWriter::gate` refuses a target outside the caller's subtree, a
        // standalone descendant, a tenant-scoped write to a `global` setting, and
        // a caller whose own access is not overridable; the read surface applies
        // the same subtree rule before it resolves. The domain-affinity filter this
        // scope also feeds cannot receive a constraint on a resource that declares
        // no domain property, so nothing is widened by leaving it unconstrained.
        //
        // The decision itself is still the PDP's: a deny is still a deny.
        let scope = self
            .inner
            .access_scope_with(
                ctx,
                resource,
                permission.action(),
                resource_id,
                &AccessRequest::new().require_constraints(false),
            )
            .await
            // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-4
            // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-5
            .map_err(|err| {
                // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-10
                // Counted before the refusal is collapsed into the one answer
                // every shape gets: the reason is the operator's, never the
                // caller's.
                if let (Some(metrics), Some(operation), Some(reason)) =
                    (counted, permission.operation(), refusal_reason(&err))
                {
                    metrics.authz_denial(operation, reason);
                }
                // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-10
                deny(permission, &err)
            })?;
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-5
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-4
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-9
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-8
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-3
        Ok(scope)
    }
}

/// A known refusal is an absent scope; any other failure stays an error.
fn optional(
    decision: Result<AccessScope, DomainError>,
) -> Result<Option<AccessScope>, DomainError> {
    match decision {
        Ok(scope) => Ok(Some(scope)),
        Err(DomainError::Unauthorized { .. }) => Ok(None),
        Err(err) => Err(err),
    }
}

impl std::fmt::Debug for PermissionEnforcer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionEnforcer").finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "authz_tests.rs"]
mod authz_tests;
