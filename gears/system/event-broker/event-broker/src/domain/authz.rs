//! Authorization resources for publishing, subscriptions, and consumer groups.
//!
//! Concrete resources use their own GTS id as the PEP resource type
//! (`docs/DESIGN.md` §3.3). The id is also sent as a resource property.
//! Tenant scope is checked separately through the deployed authorization policy.

use std::sync::LazyLock;

use authz_resolver_sdk::{AccessRequest, PolicyEnforcer, ResourceType};
use event_broker_sdk::gts::{CONSUMER_GROUP_RESOURCE_TYPE, EventV1, REQUEST_RESOURCE_TYPE};
use gts::{GtsId, GtsIdPattern, GtsIdPatternSegment, GtsTypeId};
use toolkit_gts::{GtsInstanceId, GtsSchema};
use toolkit_security::{SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::error::{DomainError, ErrorCode};

/// Resource for `define` when creating an anonymous group, before a group exists.
/// Existing named groups use [`consumer_group_resource`] instead
/// (`docs/DESIGN.md`, Consumer Group Lifecycle).
pub const CONSUMER_GROUP_FAMILY_RESOURCE: ResourceType =
    ResourceType::from_static(CONSUMER_GROUP_RESOURCE_TYPE, &[]);

/// Matches the event base type and its descendants. Invalid SDK metadata is
/// handled by [`event_family`] without panicking.
static EVENT_FAMILY: LazyLock<Option<GtsIdPattern>> =
    LazyLock::new(|| GtsIdPattern::try_new(EventV1::TYPE_ID).ok());

/// The event family pattern; fails closed when it cannot be built.
fn event_family() -> Result<&'static GtsIdPattern, DomainError> {
    EVENT_FAMILY.as_ref().ok_or_else(|| {
        DomainError::Internal(format!(
            "event base type '{}' is not a valid GTS pattern",
            EventV1::TYPE_ID
        ))
    })
}

/// PEP resource for publishing or consuming a concrete event type.
/// Rejects types outside the event family before authorization.
///
/// # Errors
/// `DomainError::NotFound` with `EventTypeNotFound` when `event_type` does not
/// derive from `gts.cf.core.events.event.v1~`; `DomainError::Internal` if the
/// event family pattern cannot be built.
pub fn event_type_resource(event_type: &GtsTypeId) -> Result<ResourceType, DomainError> {
    let family = event_family()?;
    let is_event = GtsId::try_new(event_type.as_ref()).is_ok_and(|id| id.matches_pattern(family));
    if !is_event {
        return Err(DomainError::NotFound {
            code: ErrorCode::EventTypeNotFound,
            message: format!("event type '{event_type}' is not registered"),
            resource: event_type.to_string(),
        });
    }
    Ok(ResourceType::new(event_type.as_ref().to_owned(), &[]))
}

/// PEP resource for a JOIN event-type interest (`consume`).
///
/// Exact interests use their own type. Wildcards use the event base type because
/// a pattern is not a valid GTS id; the caller sends it as `event_type_id`.
/// The pattern must cover or be covered by the event family.
///
/// # Errors
/// `DomainError::Validation` with `BadTypePattern` when `pattern` cannot match
/// any event type; `DomainError::Internal` if the event family pattern cannot
/// be built.
pub fn event_type_interest_resource(pattern: &GtsIdPattern) -> Result<ResourceType, DomainError> {
    let family = event_family()?;
    if !(family.covers(pattern) || pattern.covers(family)) {
        return Err(DomainError::Validation {
            code: ErrorCode::BadTypePattern,
            message: format!(
                "event type selector '{}' cannot match any event type ({}…)",
                pattern.pattern(),
                EventV1::TYPE_ID
            ),
        });
    }
    let wildcard = pattern
        .segments()
        .iter()
        .any(GtsIdPatternSegment::is_wildcard);
    Ok(if wildcard {
        ResourceType::from_static(EventV1::TYPE_ID, &[])
    } else {
        ResourceType::new(pattern.pattern().to_owned(), &[])
    })
}

/// PEP resource for one concrete topic (`topic:consume` on JOIN).
#[must_use]
pub fn topic_resource(topic: &GtsInstanceId) -> ResourceType {
    ResourceType::new(topic.as_ref().to_owned(), &[])
}

/// PEP resource for an existing named consumer group.
/// Anonymous groups use tenant-scope authorization instead.
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
