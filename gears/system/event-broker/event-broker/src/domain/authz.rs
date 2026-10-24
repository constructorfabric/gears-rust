//! Shared authz primitives for `IngestServiceImpl::publish_event` and
//! `DeliveryServiceImpl::join` (`eb-authz-enforcement`, `gears-rust#4516`).
//!
//! Every check on a concrete broker resource - an event type, a topic, a
//! named consumer group - names that resource's own GTS id as the PEP
//! resource type (`docs/DESIGN.md` §3.3 "Permission grammar"): a grant on
//! `gts.cf.core.events.event.v1~vendor.audit.*` or on
//! `gts.cf.core.events.topic.v1~vendor.events.tenants.v1` can then be matched
//! by any policy decision point against the type string alone, the way it
//! matches every other gear's resources. Asking about a fixed family label
//! instead would make every `produce` grant cover every producer's events.
//! The same id is still sent as a resource property (`event_type_id`,
//! `topic_id`, `consumer_group_id`) for decision points that read it.
//!
//! Tenant scope (`TENANT_SCOPE_RESOURCE`) is *also* a `PolicyEnforcer` call,
//! not a `tenant-resolver-sdk` call - `oagw`'s
//! `bind.rs::validate_bind_constraints` precedent expresses "authorized to
//! act for tenant X" purely via `resource_property(OWNER_TENANT_ID, ..)` on
//! a `PolicyEnforcer::access_scope_with` call, trusting the deployed
//! authz-resolver plugin/policy to actually enforce tenant hierarchy -
//! "depending on configuration", not something this domain layer
//! re-implements against a second SDK.

use authz_resolver_sdk::{AccessRequest, PolicyEnforcer, ResourceType};
use event_broker_sdk::gts::{CONSUMER_GROUP_RESOURCE_TYPE, EventV1, REQUEST_RESOURCE_TYPE};
use gts::{GtsIdPattern, GtsTypeId};
use toolkit_gts::{GtsInstanceId, GtsSchema};
use toolkit_security::{SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::error::{DomainError, ErrorCode};

/// The consumer-group family, for the one check that has no concrete group
/// yet: `consumer_group:define` when a caller creates an anonymous group
/// (`docs/DESIGN.md`'s Consumer Group Lifecycle). Checks on an existing named
/// group use [`consumer_group_resource`] instead.
pub const CONSUMER_GROUP_FAMILY_RESOURCE: ResourceType =
    ResourceType::from_static(CONSUMER_GROUP_RESOURCE_TYPE, &[]);

/// PEP resource for one concrete event type: `produce` on publish, `consume`
/// on a JOIN interest that names the type exactly.
#[must_use]
pub fn event_type_resource(event_type: &GtsTypeId) -> ResourceType {
    ResourceType::new(event_type.as_ref().to_owned(), &[])
}

/// PEP resource for one JOIN event-type interest.
///
/// A pattern without a wildcard names one event type (or the event base type
/// itself), so it is checked as that type. A wildcard pattern such as
/// `gts.cf.core.events.event.v1~vendor.audit.*` is not a GTS identifier - a
/// decision point that parses the resource type as one would reject it - so it
/// is checked on the event base type `gts.cf.core.events.event.v1~`, with the
/// pattern itself in the `event_type_id` property for a decision point that
/// can tell whether a grant covers it. Either way the check is at least as
/// strict as before: the base type is what a family grant on events covers.
#[must_use]
pub fn event_type_interest_resource(pattern: &GtsIdPattern) -> ResourceType {
    let pattern = pattern.pattern();
    if pattern.contains('*') {
        ResourceType::from_static(EventV1::TYPE_ID, &[])
    } else {
        ResourceType::new(pattern.to_owned(), &[])
    }
}

/// PEP resource for one concrete topic (`topic:consume` on JOIN).
#[must_use]
pub fn topic_resource(topic: &GtsInstanceId) -> ResourceType {
    ResourceType::new(topic.as_ref().to_owned(), &[])
}

/// PEP resource for one existing named consumer group (`consume` on JOIN) -
/// `Anonymous` groups use `TENANT_SCOPE_RESOURCE` instead (owner-tenant
/// equality), never this (`eb-tenant-isolation-fix`).
#[must_use]
pub fn consumer_group_resource(group: &GtsInstanceId) -> ResourceType {
    ResourceType::new(group.as_ref().to_owned(), &[])
}
/// Generic resource for the platform-tenant-scope check
/// (`docs/openapi.yaml`'s `TenantIdNotAuthorized`) - deliberately distinct
/// from the topic/event-type resources: this call is about the
/// calling principal's authority over the *target tenant*, not the
/// topic/event type being acted on, so it stays its own `PolicyEnforcer`
/// call (and therefore its own denial code, independent of whichever of
/// the other checks passed or failed).
pub const TENANT_SCOPE_RESOURCE: ResourceType =
    ResourceType::from_static(REQUEST_RESOURCE_TYPE, &[pep_properties::OWNER_TENANT_ID]);

/// Overwrites a `DomainError::Forbidden`'s `code` with a specific
/// `docs/openapi.yaml` code (`TopicNotAuthorized`/`EventTypeNotAuthorized`/
/// `NotAuthorizedToProduce`/`TenantIdNotAuthorized`) - `From<EnforcerError>`
/// only knows a generic `"AuthzDenied"`, since which specific check failed
/// is known only at the call site (design.md "code-per-call-site"). A no-op
/// for every other `DomainError` variant (a PEP failure only ever produces
/// `Forbidden` or `Internal` per that `From` impl).
#[must_use]
pub fn with_forbidden_code(mut err: DomainError, code: ErrorCode) -> DomainError {
    if let DomainError::Forbidden { code: c, .. } = &mut err {
        *c = code;
    }
    err
}

/// Confirms the calling principal is authorized to act as `target` tenant -
/// a dedicated `PolicyEnforcer` check against `TENANT_SCOPE_RESOURCE`
/// (`pep_properties::OWNER_TENANT_ID` set to `target`), not a
/// `tenant-resolver-sdk` call. Whether this actually enforces tenant
/// hierarchy (vs. e.g. always allowing) is entirely the deployed
/// authz-resolver plugin/policy's concern.
///
/// # Errors
/// Returns `DomainError::Forbidden { code: "TenantIdNotAuthorized", .. }` if
/// the PEP denies, or the mapped `DomainError` for any other `EnforcerError`.
pub async fn tenant_authorized(
    ctx: &SecurityContext,
    policy_enforcer: &PolicyEnforcer,
    action: &'static str,
    target: Uuid,
) -> Result<(), DomainError> {
    policy_enforcer
        .access_scope_with(
            ctx,
            &TENANT_SCOPE_RESOURCE,
            action,
            None,
            &AccessRequest::new()
                .resource_property(pep_properties::OWNER_TENANT_ID, target)
                .require_constraints(false),
        )
        .await
        .map_err(|e| with_forbidden_code(e.into(), ErrorCode::TenantIdNotAuthorized))?;
    Ok(())
}

#[cfg(test)]
#[path = "authz_tests.rs"]
mod authz_tests;
