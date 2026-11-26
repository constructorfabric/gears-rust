//! Authorized operation admission: the one boundary every operation enters
//! (`features/foundation.md`, "Authorized Operation Admission").
//!
//! Admission has two modes (ADR-0009). **PDP mode** serves QE's REST and
//! in-process API: public shape check, PDP call through `PolicyEnforcer`, then
//! the `AccessScope` travels unmodified to the operation handler for
//! `SecureConn`. The PDP authorizes the complete explicit target, the target
//! tenant included as `owner_tenant_id`; QE never interprets the returned
//! scope and keeps no PDP decision cache. **Trusted mode** serves only the
//! quota resolver plugin: the same shape check, no PDP call, and a scope that
//! names the request's tenant only; it refuses policy management.
//!
//! Both modes return the actor of the operation with the scope. Below this
//! boundary nothing takes a `SecurityContext`.

use std::sync::Arc;

use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use authz_resolver_sdk::{EnforcerError, PolicyEnforcer};
use quota_enforcement_sdk::{Actor, TenantId};
use serde_json::{Map, Value};
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, PlatformIdentity, SecurityContext, pep_properties};
use uuid::Uuid;

use super::error::DomainError;
use super::ports::metrics::{DenialReason, QeMetrics};

/// The admission phase of one operation: every PDP call it makes, covering `item_count` items.
#[must_use]
pub fn admission_span(item_count: usize) -> tracing::Span {
    tracing::info_span!("quota_enforcement.admission", item_count)
}

/// The explicit, caller-supplied target of an operation. Untrusted until
/// admission accepts it.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionTarget {
    /// Target tenant.
    pub tenant_id: TenantId,
    /// Target resource, when the operation names one.
    pub resource_id: Option<Uuid>,
}

impl AdmissionTarget {
    /// A tenant-level target.
    #[must_use]
    pub const fn tenant(tenant_id: TenantId) -> Self {
        Self {
            tenant_id,
            resource_id: None,
        }
    }

    /// A resource-level target.
    #[must_use]
    pub const fn resource(tenant_id: TenantId, resource_id: Uuid) -> Self {
        Self {
            tenant_id,
            resource_id: Some(resource_id),
        }
    }
}

/// An admitted target: the scope the handler binds through `SecureConn` and
/// the actor the operation log and the events name.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Admitted {
    /// The admitted tenant.
    pub tenant_id: TenantId,
    /// The scope exactly as admission returned it.
    pub access_scope: AccessScope,
    /// The cause of the operation.
    pub actor: Actor,
}

/// Platform policy admission without a fabricated tenant identifier.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct OperatorAdmission {
    access_scope: AccessScope,
    actor: Actor,
}

impl OperatorAdmission {
    /// Preserve the PDP-issued scope for the policy storage boundary.
    #[must_use]
    pub const fn access_scope(&self) -> &AccessScope {
        &self.access_scope
    }

    /// The operator who acts.
    #[must_use]
    pub const fn actor(&self) -> &Actor {
        &self.actor
    }
}

/// The PDP boundary of QE's API, shared by every call.
// @cpt-dod:cpt-cf-quota-enforcement-dod-gateway-admission:p1
#[domain_model]
pub struct Admission {
    enforcer: PolicyEnforcer,
    metrics: Arc<dyn QeMetrics>,
}

impl Admission {
    /// Build the boundary around the platform PEP.
    #[must_use]
    pub fn new(enforcer: PolicyEnforcer, metrics: Arc<dyn QeMetrics>) -> Self {
        Self { enforcer, metrics }
    }

    /// The metrics port the boundary records on; the ingress step shares it.
    #[must_use]
    pub fn metrics(&self) -> &dyn QeMetrics {
        self.metrics.as_ref()
    }

    /// The same port as a handle, for a component that outlives the borrow.
    #[must_use]
    pub fn metrics_handle(&self) -> Arc<dyn QeMetrics> {
        Arc::clone(&self.metrics)
    }

    /// PDP mode for one call of QE's API, as its authenticated caller.
    #[must_use]
    pub fn pdp<'a>(&'a self, ctx: &'a SecurityContext) -> Gate<'a> {
        Gate {
            mode: Mode::Pdp {
                enforcer: &self.enforcer,
                ctx,
            },
            metrics: self.metrics.as_ref(),
        }
    }

    /// Trusted mode for one call through the quota resolver plugin, recording
    /// `caller` as its actor (ADR-0009).
    #[must_use]
    pub(crate) fn trusted(&self, caller: Option<PlatformIdentity>) -> Gate<'_> {
        Gate {
            mode: Mode::Trusted { caller },
            metrics: self.metrics.as_ref(),
        }
    }
}

/// Admission bound to one call, in one of its two modes.
#[domain_model]
pub struct Gate<'a> {
    mode: Mode<'a>,
    metrics: &'a dyn QeMetrics,
}

enum Mode<'a> {
    Pdp {
        enforcer: &'a PolicyEnforcer,
        ctx: &'a SecurityContext,
    },
    Trusted {
        caller: Option<PlatformIdentity>,
    },
}

impl<'a> Gate<'a> {
    /// The metrics port admission records on.
    #[must_use]
    pub fn metrics(&self) -> &'a dyn QeMetrics {
        self.metrics
    }

    /// The cause of the operation: the authenticated caller in PDP mode, the
    /// platform identity of the resolver's caller in trusted mode.
    #[must_use]
    pub fn actor(&self) -> Actor {
        match &self.mode {
            Mode::Pdp { ctx, .. } => Actor::Caller {
                subject_id: ctx.subject_id(),
            },
            Mode::Trusted { caller } => Actor::Platform {
                identity: caller.clone(),
            },
        }
    }

    /// Whether this is the trusted mode of the resolver plugin path.
    #[must_use]
    pub const fn is_trusted(&self) -> bool {
        matches!(self.mode, Mode::Trusted { .. })
    }

    /// The home tenant of the authenticated caller, the default target of an
    /// operation that names none. Trusted mode has no caller tenant.
    #[must_use]
    pub fn caller_tenant(&self) -> Option<TenantId> {
        match &self.mode {
            Mode::Pdp { ctx, .. } => Some(TenantId::new(ctx.subject_tenant_id())),
            Mode::Trusted { .. } => None,
        }
    }

    /// Admit a platform policy operation through the PDP without row
    /// properties. Trusted mode refuses it.
    ///
    /// # Errors
    /// Returns PDP denial (including unsupported constraints) or
    /// unavailability, and [`DomainError::PdpDenied`] in trusted mode.
    pub async fn admit_operator(&self, action: &str) -> Result<OperatorAdmission, DomainError> {
        let resource = &super::pep::resources::POLICY;
        let site = LogSite {
            actor: self.actor(),
            resource: resource.name(),
            action,
        };
        // @cpt-begin:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-trusted
        let (enforcer, ctx) = match &self.mode {
            Mode::Pdp { enforcer, ctx } => (*enforcer, *ctx),
            Mode::Trusted { .. } => {
                return Err(self.deny(
                    &site,
                    DenialReason::PermissionDenied,
                    DomainError::PdpDenied {
                        reason: Some("policy management is refused in trusted mode".to_owned()),
                    },
                ));
            }
        };
        // @cpt-end:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-trusted
        let request = AccessRequest::new().require_constraints(false);
        let access_scope = enforcer
            .access_scope_with(ctx, resource, action, None, &request)
            .await
            .map_err(|err| {
                self.deny(&site, denial_reason(&err), DomainError::from_enforcer(err))
            })?;
        Ok(OperatorAdmission {
            access_scope,
            actor: site.actor,
        })
    }

    /// Admit `action` on `resource` for the explicit `target`.
    ///
    /// # Errors
    ///
    /// - [`DomainError::InvalidArgument`] on a malformed public target, before
    ///   any PDP call.
    /// - [`DomainError::PdpDenied`] when the PDP denies or when its
    ///   constraints do not compile.
    /// - [`DomainError::PdpUnavailable`] when the PDP cannot be reached.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-authorized-admission:p1
    pub async fn admit(
        &self,
        resource: &ResourceType,
        action: &str,
        target: AdmissionTarget,
    ) -> Result<Admitted, DomainError> {
        self.admit_with_properties(resource, action, target, Map::new())
            .await
    }

    /// [`Self::admit`] with further resource properties the PDP evaluates:
    /// the rest of a caller-supplied attribution tuple (metric, subjects,
    /// resource). The explicit `target` tenant is authoritative: it is written
    /// after `properties`, so a caller-supplied `owner_tenant_id` can never
    /// replace it. Trusted mode has no PDP to send them to.
    ///
    /// # Errors
    ///
    /// As [`Self::admit`].
    pub async fn admit_with_properties(
        &self,
        resource: &ResourceType,
        action: &str,
        target: AdmissionTarget,
        properties: Map<String, Value>,
    ) -> Result<Admitted, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-request
        // @cpt-begin:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-authn
        // `target` stays untrusted until admission accepts it.
        let site = LogSite {
            actor: self.actor(),
            resource: resource.name(),
            action,
        };
        // @cpt-end:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-authn
        // @cpt-end:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-request

        // @cpt-begin:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-shape
        if let Err(err) = validate_target_shape(target) {
            return Err(self.deny(&site, DenialReason::InvalidArgument, err));
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-shape

        // @cpt-begin:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-trusted
        let (enforcer, ctx) = match &self.mode {
            Mode::Pdp { enforcer, ctx } => (*enforcer, *ctx),
            Mode::Trusted { .. } => {
                return Ok(Admitted {
                    tenant_id: target.tenant_id,
                    access_scope: AccessScope::for_tenant(target.tenant_id.as_uuid()),
                    actor: site.actor,
                });
            }
        };
        // @cpt-end:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-trusted

        // @cpt-begin:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-pdp
        let mut request = AccessRequest::new();
        for (key, value) in properties {
            request = request.resource_property(key, value);
        }
        let request = request
            .resource_property(pep_properties::OWNER_TENANT_ID, target.tenant_id.as_uuid())
            .require_constraints(true);
        let outcome = enforcer
            .access_scope_with(ctx, resource, action, target.resource_id, &request)
            .await;
        // @cpt-end:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-pdp

        // @cpt-begin:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-deny-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-deny
        let access_scope = outcome.map_err(|err| {
            self.deny(&site, denial_reason(&err), DomainError::from_enforcer(err))
        })?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-deny
        // @cpt-end:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-deny-if

        // @cpt-begin:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-scope
        // @cpt-begin:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-forward
        // Preserve hierarchy-aware filters for `SecureConn`; an in-memory
        // membership check cannot evaluate `InTenantSubtree`.
        Ok(Admitted {
            tenant_id: target.tenant_id,
            access_scope,
            actor: site.actor,
        })
        // @cpt-end:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-forward
        // @cpt-end:cpt-cf-quota-enforcement-flow-authorized-admission:p1:inst-adm-scope
    }

    /// Record a denial on the metrics port and in the log, then hand the
    /// error back. Identifiers go to the log only, never to a label.
    fn deny(&self, site: &LogSite<'_>, reason: DenialReason, err: DomainError) -> DomainError {
        self.metrics.record_denial(reason);
        tracing::warn!(
            target: "qe.admission",
            actor = %site.actor,
            resource = site.resource,
            action = site.action,
            reason = reason.as_label(),
            error = %err,
            "admission denied"
        );
        err
    }
}

/// Log context of one admission call.
struct LogSite<'a> {
    actor: Actor,
    resource: &'a str,
    action: &'a str,
}

/// Public shape rules that hold for every target, before the PDP call.
fn validate_target_shape(target: AdmissionTarget) -> Result<(), DomainError> {
    if target.tenant_id.as_uuid().is_nil() {
        return Err(DomainError::InvalidArgument {
            field: "tenant_id",
            reason: "TENANT_ID_REQUIRED",
        });
    }
    if target.resource_id.is_some_and(|id| id.is_nil()) {
        return Err(DomainError::InvalidArgument {
            field: "resource_id",
            reason: "RESOURCE_ID_INVALID",
        });
    }
    Ok(())
}

/// The denial metric reason for a PEP error.
const fn denial_reason(err: &EnforcerError) -> DenialReason {
    match err {
        EnforcerError::AccessDenied(_) | EnforcerError::CompileFailed(_) => {
            DenialReason::PermissionDenied
        }
        EnforcerError::EvaluationFailed(_) => DenialReason::PdpUnavailable,
        // An unknown outcome class is neither a denial nor PDP unavailability.
        _ => DenialReason::Internal,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "admission_tests.rs"]
mod admission_tests;
